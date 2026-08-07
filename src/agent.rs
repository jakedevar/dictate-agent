use crate::audio::AudioCapture;
use crate::config::Config;
use crate::grammar::GrammarCorrector;
use crate::history::HistoryStore;
use crate::local_executor::LocalExecutor;
use crate::notify::Notifier;
use crate::output::OutputHandler;
use crate::router::{self, RouteType};
use crate::timer::TimerExecutor;
use crate::transcribe::Transcriber;
use anyhow::Result;
use signal_hook::consts::signal::{SIGINT, SIGTERM, SIGUSR1, SIGUSR2};
use signal_hook_tokio::Signals;
use std::process::Command;
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

pub struct DictateAgent {
    config: Config,
    recording: bool,
    audio: AudioCapture,
    transcriber: Transcriber,
    output: OutputHandler,
    notifier: Notifier,
    grammar: GrammarCorrector,
    local_executor: LocalExecutor,
    timer_executor: TimerExecutor,
    history: HistoryStore,
}

impl DictateAgent {
    pub async fn new(config: Config) -> Result<Self> {
        // Write PID file
        let pid_path = crate::config::pid_file_path();
        if let Some(parent) = pid_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&pid_path, std::process::id().to_string())?;
        info!("PID file written to {}", pid_path.display());

        let audio = AudioCapture::new()?;

        // Initialize transcriber and start loading model in background
        let transcriber = Transcriber::new(&config.whisper);
        transcriber.load_model_async();

        let output = OutputHandler::new(&config.output);
        let notifier = Notifier::new(&config.notifications);
        let grammar = GrammarCorrector::new(&config.grammar);
        let local_executor = LocalExecutor::new(&config.local);
        let timer_executor = TimerExecutor::new(&config.timer);
        let history = HistoryStore::new(&config.history)?;

        // Try to ensure Ollama is running (non-blocking best-effort)
        {
            let (host, port) = crate::grammar::parse_host_port(&config.grammar.host);
            tokio::spawn(async move {
                crate::local_executor::ensure_ollama_running(&host, port, 10).await;
            });
        }

        Ok(Self {
            config,
            recording: false,
            audio,
            transcriber,
            output,
            notifier,
            grammar,
            local_executor,
            timer_executor,
            history,
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        let mut signals = Signals::new([SIGUSR1, SIGUSR2, SIGINT, SIGTERM])?;

        info!("Dictate agent running. Waiting for signals...");

        while let Some(signal) = signals.next().await {
            match signal {
                SIGUSR1 => self.toggle().await,
                SIGUSR2 => self.cancel().await,
                SIGINT | SIGTERM => {
                    info!("Shutdown signal received");
                    break;
                }
                _ => unreachable!(),
            }
        }

        self.shutdown().await;
        Ok(())
    }

    async fn toggle(&mut self) {
        if self.recording {
            info!("Stopping recording, processing pipeline...");
            self.recording = false;
            self.stop_recording_and_process().await;
        } else {
            info!("Starting recording...");
            // Pause media if playing — matches main.py:160-164
            self.pause_media_if_playing();
            self.notifier.recording();
            match self.audio.start() {
                Ok(()) => {
                    self.recording = true;
                }
                Err(e) => {
                    error!("Failed to start recording: {}", e);
                    self.notifier.clear_status();
                    self.notifier.error(&format!("Recording failed: {}", e));
                    self.resume_media_if_needed();
                }
            }
        }
    }

    async fn stop_recording_and_process(&mut self) {
        self.notifier.transcribing();

        // Start interaction tracking
        let mut interaction = self.history.begin();

        // 1. Stop recording
        let samples = match self.audio.stop().await {
            Some(s) => s,
            None => {
                warn!("No audio captured");
                self.notifier.no_speech();
                self.resume_media_if_needed();
                return;
            }
        };

        let audio_duration = AudioCapture::duration_secs(&samples);
        interaction.audio_duration_s = Some(audio_duration);

        // 2. Transcribe
        let transcription_start = std::time::Instant::now();
        let result = match self.transcriber.transcribe(&samples).await {
            Ok(Some(r)) => r,
            Ok(None) => {
                self.notifier.no_speech();
                self.resume_media_if_needed();
                return;
            }
            Err(e) => {
                error!("Transcription failed: {}", e);
                self.notifier.clear_status();
                self.notifier.error(&format!("Transcription failed: {}", e));
                interaction.error_summary = Some(format!("Transcription failed: {}", e));
                self.history.commit(&interaction);
                self.resume_media_if_needed();
                return;
            }
        };
        interaction.transcription_duration_s = Some(transcription_start.elapsed().as_secs_f64());
        interaction.raw_transcription = Some(result.text.clone());
        interaction.corrected_transcription = Some(result.text.clone());

        info!("Transcribed: \"{}\"", result.text);

        // 3. Grammar correction (fail-open)
        let grammar_result = self.grammar.correct(&result.text).await;
        let text = grammar_result.corrected.clone();

        interaction.grammar_input = Some(grammar_result.original.clone());
        interaction.grammar_output = Some(grammar_result.corrected.clone());
        interaction.grammar_changed = grammar_result.corrected != grammar_result.original;
        interaction.grammar_error = grammar_result.error.clone();
        interaction.grammar_duration_s = Some(grammar_result.duration_s);

        if interaction.grammar_changed {
            info!(
                "Grammar corrected: \"{}\" → \"{}\"",
                grammar_result.original, grammar_result.corrected
            );
        }

        // 4. Route
        let route = router::route(&text);
        info!("Routed to {:?}: \"{}\"", route.route, route.text);

        interaction.route_type = Some(format!("{:?}", route.route).to_lowercase());
        interaction.route_model = Some(route.model.clone());
        interaction.route_confidence = Some(route.confidence);

        // 5. Dispatch
        match route.route {
            RouteType::Type => {
                let typed = self.output.type_text(&route.text);
                interaction.output_typed = typed;
                if typed {
                    interaction.output_char_count = Some(route.text.len());
                }
            }
            RouteType::Local => {
                self.notifier.processing(&self.config.local.model);
                interaction.prompt_sent = Some(route.text.clone());
                interaction.execution_model = Some(self.config.local.model.clone());

                let exec_start = std::time::Instant::now();
                let exec_result = self.local_executor.execute(&route.text, None).await;
                interaction.execution_duration_s = Some(exec_start.elapsed().as_secs_f64());
                interaction.execution_success = Some(exec_result.success);

                if exec_result.success {
                    interaction.response_text = Some(exec_result.response.clone());
                    let typed = self.output.type_text(&exec_result.response);
                    interaction.output_typed = typed;
                    if typed {
                        interaction.output_char_count = Some(exec_result.response.len());
                    }
                } else {
                    interaction.execution_error = exec_result.error.clone();
                    self.notifier.error(&exec_result.error.unwrap_or_default());
                }
            }
            RouteType::Timer => {
                let timer_result = self.timer_executor.execute(&route.text);
                interaction.execution_success = Some(timer_result.success);

                if timer_result.success {
                    interaction.response_text = Some(timer_result.response.clone());
                    self.notifier.timer_set(&timer_result.response);
                } else {
                    interaction.execution_error = timer_result.error.clone();
                    self.notifier.error(&timer_result.error.unwrap_or_default());
                }
            }
            RouteType::Edit => {
                self.notifier.error("Edit route not implemented");
                interaction.error_summary = Some("Edit route not implemented".into());
            }
            RouteType::Command => {
                self.notifier.error("Command route not implemented");
                interaction.error_summary = Some("Command route not implemented".into());
            }
        }

        // 6. Clear status notification and resume media
        self.notifier.clear_status();
        self.resume_media_if_needed();

        // 7. Commit history
        interaction.completed = true;
        self.history.commit(&interaction);
    }

    async fn cancel(&mut self) {
        if self.recording {
            info!("Recording cancelled");
            self.audio.cancel();
            self.recording = false;
            self.notifier.cancelled();
            self.resume_media_if_needed();
        } else {
            warn!("Cancel received but not recording");
        }
    }

    async fn shutdown(&mut self) {
        info!("Shutting down...");
        if self.recording {
            self.audio.cancel();
        }
        let pid_path = crate::config::pid_file_path();
        let _ = std::fs::remove_file(&pid_path);
        info!("PID file removed");
    }

    // --- Media pause/resume ---
    // Port of main.py:160-168 (pause) and 351-358 (resume)

    /// Check if media is playing via playerctl, pause if so, and write state file.
    fn pause_media_if_playing(&self) {
        let is_playing = Command::new("playerctl")
            .arg("status")
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .eq_ignore_ascii_case("Playing")
            })
            .unwrap_or(false);

        let state_path = crate::config::media_state_path();

        if is_playing {
            // Write state file and pause
            let _ = std::fs::write(&state_path, "playing");
            let _ = Command::new("playerctl").arg("pause").output();
            info!("Media paused");
        } else if state_path.exists() {
            // Clean up stale state file from a previous crash
            let _ = std::fs::remove_file(&state_path);
        }
    }

    /// Resume media if state file exists (meaning we paused it).
    fn resume_media_if_needed(&self) {
        let state_path = crate::config::media_state_path();
        if state_path.exists() {
            let _ = std::fs::remove_file(&state_path);
            let _ = Command::new("playerctl").arg("play").output();
            info!("Media resumed");
        }
    }
}

impl Drop for DictateAgent {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(crate::config::pid_file_path());
    }
}
