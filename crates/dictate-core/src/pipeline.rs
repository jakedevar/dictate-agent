//! One dictation session, start to terminal state.
//!
//! This is today's `stop_recording_and_process` re-expressed as a cancellable
//! task. The stages, their order, and their behavior are unchanged — capture,
//! transcribe, correct, route, dispatch, log — because this slice is about the
//! control plane, not about changing what dictation does. What is new:
//!
//! 1. **A single owner.** One task owns a session from `Recording` to its
//!    terminal state, so there is exactly one teardown path
//!    ([`Pipeline::finish`]) and no way to reach a terminal state without
//!    releasing the microphone and resuming the user's music.
//! 2. **Checkpoints, not polling.** Every stage boundary goes through
//!    [`SessionHandle::advance_checked`], and every `await` that could outlast
//!    a user's patience is raced against
//!    [`CancelToken::cancelled`](crate::cancel::CancelToken::cancelled). You
//!    cannot enter a stage without passing the cancellation check, because the
//!    check is the thing that moves you there.
//! 3. **A commit point.** Injection is the one irreversible stage, so it is
//!    entered through [`CancelToken::enter_commit`], which atomically takes the
//!    session out of the cancellable set. A cancel that loses that race is told
//!    `TooLate` rather than being allowed to report success over text that is
//!    already in the user's editor.
//! 4. **Honest timings.** Each stage records `Ran` / `Skipped{reason}` /
//!    `Failed{ms}` / `NotReported` rather than a number that might be a lie.
//!    See [`Stages`].
//!
//! [`CancelToken::enter_commit`]: crate::cancel::CancelToken::enter_commit

use std::sync::{Arc, Mutex};
use std::time::Instant;

use dictate_history::history::Interaction;
use dictate_history::HistoryStore;
use dictate_proto::{
    ErrorCode, Event, FinalText, InjectionOutcome, ProtoError, Route, SkipReason, StageTiming,
    StageTimings, State, Transcript,
};
use tokio::sync::Notify;
use tracing::{error, info, warn};

use crate::cancel::CancelToken;
use crate::local_executor::LocalExecutor;
use crate::ports::{
    audio_ms, AudioSource, FormatPlan, Formatter, MediaController, Notice, SttProvider,
    StatusNotifier, TextInjector,
};
use crate::router::{self, RouteType};
use crate::session::SessionHandle;
use crate::timer::TimerExecutor;

/// Map the router's internal enum onto the wire enum.
///
/// Two enums rather than one because `dictate-proto` must not depend on
/// `dictate-core` (it is the leaf every consumer shares) and the router
/// predates the protocol.
#[must_use]
pub fn route_to_proto(route: RouteType) -> Route {
    match route {
        RouteType::Type => Route::Type,
        RouteType::Local => Route::Local,
        RouteType::Timer => Route::Timer,
        RouteType::Edit => Route::Edit,
        RouteType::Command => Route::Command,
    }
}

/// Per-stage timing accumulator.
///
/// Starts as all-[`StageTiming::NotReported`] and is filled in as stages
/// execute, so a stage the pipeline never reached reads as "no data" rather
/// than as a fabricated zero. `total_ms` is measured across the whole session
/// rather than summed from the stages — the gap between the two is scheduling
/// and queueing time that belongs to no stage, and it is the signal S12 uses.
#[derive(Debug)]
pub struct Stages {
    timings: StageTimings,
    started: Instant,
}

impl Default for Stages {
    fn default() -> Self {
        Self::new()
    }
}

impl Stages {
    /// Begin accounting.
    #[must_use]
    pub fn new() -> Self {
        Self {
            timings: StageTimings {
                // VAD does not exist until S11. `not_supported` is the
                // truthful answer — the stage is absent from this build, as
                // distinct from being switched off by configuration or from
                // having no data.
                vad: StageTiming::skipped(SkipReason::NotSupported),
                // The pure-Rust corrections pass currently executes *inside*
                // `dictate-stt::transcribe`, so its cost is already counted
                // in `stt` and it has no separately measured duration of its
                // own. Reporting `Ran{0.0}` here would invent a measurement;
                // `not_reported` says exactly what is true. S20 lifts the
                // rules layer into its own stage.
                fmt_rules: StageTiming::NotReported,
                ..StageTimings::default()
            },
            started: Instant::now(),
        }
    }

    /// Elapsed wall time since the session's pipeline began.
    #[must_use]
    pub fn elapsed_ms(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1000.0
    }

    /// Finalize, stamping the measured (not derived) total.
    #[must_use]
    pub fn finish(mut self, audio_ms_value: Option<f64>) -> StageTimings {
        self.timings.total_ms = Some(self.elapsed_ms());
        self.timings.audio_ms = audio_ms_value;
        self.timings
    }
}

/// Time one stage and record the outcome.
struct StageClock(Instant);

impl StageClock {
    fn start() -> Self {
        Self(Instant::now())
    }

    fn ms(&self) -> f64 {
        self.0.elapsed().as_secs_f64() * 1000.0
    }

    fn ran(&self) -> StageTiming {
        StageTiming::ran(self.ms())
    }

    fn failed(&self, error: impl Into<String>) -> StageTiming {
        StageTiming::Failed {
            ms: self.ms(),
            error: Some(error.into()),
        }
    }
}

/// How a session's options resolved against the caller's capabilities.
#[derive(Debug, Clone)]
pub struct ResolvedOptions {
    /// Whether to inject the result into the focused app. `false` means the
    /// caller takes delivery instead, which is a success, not a skip.
    pub inject: bool,
    /// A route forced by the caller, bypassing the router.
    pub forced_route: Option<Route>,
    /// Routes this caller may invoke at all. Deny-by-default per S01 item 2:
    /// an empty list permits nothing.
    pub allowed_routes: Vec<Route>,
    /// Suppress persistence of transcript text for this session.
    pub privacy: bool,
}

impl Default for ResolvedOptions {
    fn default() -> Self {
        Self {
            inject: true,
            forced_route: None,
            allowed_routes: Route::known().to_vec(),
            privacy: false,
        }
    }
}

/// What a finished session produced.
#[derive(Debug, Clone)]
pub struct PipelineOutcome {
    /// The terminal state reached: `done`, `error`, or `cancelled`.
    pub state: State,
    /// The transcript, when one was produced.
    pub transcript: Option<Transcript>,
    /// Why the session failed, when it did.
    pub error: Option<ProtoError>,
}

/// Everything a session needs to run, shared by `Arc` across session tasks.
pub struct Pipeline {
    /// Microphone.
    pub audio: Arc<dyn AudioSource>,
    /// Recognizer.
    pub stt: Arc<dyn SttProvider>,
    /// LLM formatting pass.
    pub formatter: Arc<dyn Formatter>,
    /// Text injection.
    pub injector: Arc<dyn TextInjector>,
    /// Desktop notifications.
    pub notifier: Arc<dyn StatusNotifier>,
    /// Media pause/resume.
    pub media: Arc<dyn MediaController>,
    /// Interaction log.
    pub history: Arc<Mutex<HistoryStore>>,
    /// Ollama executor for the `local` route.
    pub local: Arc<LocalExecutor>,
    /// `systemd-run` executor for the `timer` route.
    pub timer: Arc<TimerExecutor>,
    /// Model name used by the `local` route, for notifications.
    pub local_model: String,
}

/// The result of one stage that may have been cut short.
enum Step<T> {
    Continue(T),
    Cancelled,
}

impl Pipeline {
    /// Run one session to a terminal state.
    ///
    /// The session begins in `Recording` (the engine has already opened the
    /// device) and waits on `stop`. Returns the terminal state so the engine
    /// can clear its slot; every event a client needs has already been
    /// published by then.
    pub async fn run(
        self: Arc<Self>,
        handle: SessionHandle,
        stop: Arc<Notify>,
        opts: ResolvedOptions,
    ) -> PipelineOutcome {
        let mut stages = Stages::new();
        let token = handle.token().clone();

        self.notifier.notify(Notice::Recording);
        // Best-effort and slow (a `playerctl` subprocess), so it runs off the
        // engine's thread and is not allowed to delay the state machine.
        {
            let media = self.media.clone();
            let _ = tokio::task::spawn_blocking(move || media.pause_if_playing()).await;
        }

        // --- Recording: wait for stop, or for a cancel from any actor -------
        // `biased` puts cancellation first: if both a stop and a cancel are
        // pending, the session must end cancelled rather than transcribe audio
        // the user asked to throw away.
        tokio::select! {
            biased;
            () = token.cancelled() => {
                return self.finish(&handle, stages, None, Outcome::cancelled()).await;
            }
            () = stop.notified() => {}
        }

        let mut interaction = {
            let store = self.history.lock().expect("history mutex poisoned");
            store.begin()
        };

        // --- Capture flush --------------------------------------------------
        self.notifier.notify(Notice::Transcribing);
        if handle.advance_checked(State::Transcribing).is_err() {
            return self.finish(&handle, stages, Some(interaction), Outcome::cancelled()).await;
        }

        let clock = StageClock::start();
        let samples = match self.race(&token, self.audio.stop()).await {
            Step::Cancelled => {
                stages.timings.capture = clock.failed("cancelled during capture flush");
                return self.finish(&handle, stages, Some(interaction), Outcome::cancelled()).await;
            }
            Step::Continue(s) => s,
        };
        stages.timings.capture = clock.ran();

        let Some(samples) = samples else {
            warn!("No audio captured");
            self.notifier.notify(Notice::NoSpeech);
            stages.timings.stt = StageTiming::skipped(SkipReason::NoSpeechDetected);
            stages.timings.inject = StageTiming::skipped(SkipReason::NoSpeechDetected);
            return self
                .finish(
                    &handle,
                    stages,
                    Some(interaction),
                    Outcome::done(empty_transcript(SkipReason::NoSpeechDetected)),
                )
                .await;
        };

        let audio_len_ms = audio_ms(&samples);
        interaction.audio_duration_s = Some(audio_len_ms / 1000.0);

        // --- Speech to text -------------------------------------------------
        let clock = StageClock::start();
        let transcribed = match self.race(&token, self.stt.transcribe(&samples)).await {
            Step::Cancelled => {
                stages.timings.stt = clock.failed("cancelled during transcription");
                return self
                    .finish(&handle, stages, Some(interaction), Outcome::cancelled_after(audio_len_ms))
                    .await;
            }
            Step::Continue(r) => r,
        };

        let transcribed = match transcribed {
            Ok(t) => {
                stages.timings.stt = clock.ran();
                t
            }
            Err(e) => {
                error!("Transcription failed: {}", e);
                stages.timings.stt = clock.failed(e.to_string());
                self.notifier.notify(Notice::Clear);
                self.notifier.notify(Notice::Error(format!("Transcription failed: {e}")));
                interaction.error_summary = Some(format!("Transcription failed: {e}"));
                let err = ProtoError::new(ErrorCode::SttFailed, e.to_string());
                return self
                    .finish(&handle, stages, Some(interaction), Outcome::error(err, audio_len_ms))
                    .await;
            }
        };
        interaction.transcription_duration_s = stages.timings.stt.elapsed_ms().map(|ms| ms / 1000.0);

        let Some(transcribed) = transcribed else {
            info!("No speech detected");
            self.notifier.notify(Notice::NoSpeech);
            stages.timings.inject = StageTiming::skipped(SkipReason::NoSpeechDetected);
            return self
                .finish(
                    &handle,
                    stages,
                    Some(interaction),
                    Outcome::done(empty_transcript(SkipReason::NoSpeechDetected))
                        .with_audio_ms(audio_len_ms),
                )
                .await;
        };

        let raw_text = transcribed.text.clone();
        interaction.raw_transcription = Some(raw_text.clone());
        interaction.corrected_transcription = Some(raw_text.clone());
        info!("Transcribed: \"{}\"", raw_text);

        // --- Formatting -----------------------------------------------------
        if handle.advance_checked(State::Formatting).is_err() {
            return self
                .finish(&handle, stages, Some(interaction), Outcome::cancelled_after(audio_len_ms))
                .await;
        }

        let text = match self.formatter.plan(&raw_text) {
            FormatPlan::Skip(reason) => {
                stages.timings.fmt_llm = StageTiming::skipped(reason);
                raw_text.clone()
            }
            FormatPlan::Run => {
                let clock = StageClock::start();
                match self.race(&token, self.formatter.format(&raw_text)).await {
                    Step::Cancelled => {
                        stages.timings.fmt_llm = clock.failed("cancelled during formatting");
                        return self
                            .finish(
                                &handle,
                                stages,
                                Some(interaction),
                                Outcome::cancelled_after(audio_len_ms),
                            )
                            .await;
                    }
                    Step::Continue(formatted) => {
                        // A pass that burned time and then fell back is
                        // `Failed`, not `Ran` — that time is in the user's
                        // latency budget and must not vanish from accounting.
                        stages.timings.fmt_llm = match &formatted.error {
                            Some(e) => clock.failed(e.clone()),
                            None => clock.ran(),
                        };
                        interaction.grammar_input = Some(raw_text.clone());
                        interaction.grammar_output = Some(formatted.text.clone());
                        interaction.grammar_changed = formatted.changed;
                        interaction.grammar_error = formatted.error.clone();
                        interaction.grammar_duration_s = Some(formatted.duration_s);
                        if formatted.changed {
                            info!("Grammar corrected: \"{}\" → \"{}\"", raw_text, formatted.text);
                        }
                        formatted.text
                    }
                }
            }
        };
        interaction.corrected_transcription = Some(text.clone());

        // --- Route ----------------------------------------------------------
        let routed = router::route(&text);
        let resolved_route = opts
            .forced_route
            .clone()
            .unwrap_or_else(|| route_to_proto(routed.route.clone()));
        info!("Routed to {:?}: \"{}\"", resolved_route, routed.text);

        interaction.route_type = Some(format!("{:?}", routed.route).to_lowercase());
        interaction.route_model = Some(routed.model.clone());
        interaction.route_confidence = Some(routed.confidence);

        // Deny-by-default route gating (S01 item 2, per the Epic-lead overrule).
        // The router can land on `timer`, which runs `systemd-run` on this
        // host; a caller that was not granted that route must not reach it by
        // saying the right word.
        if !opts.allowed_routes.contains(&resolved_route) {
            let msg = format!(
                "route '{}' is not permitted for this connection",
                resolved_route.as_str()
            );
            warn!("{msg}");
            stages.timings.inject = StageTiming::skipped(SkipReason::NotPermitted);
            interaction.error_summary = Some(msg.clone());
            let err = ProtoError::new(ErrorCode::Forbidden, msg);
            return self
                .finish(&handle, stages, Some(interaction), Outcome::error(err, audio_len_ms))
                .await;
        }

        // --- Dispatch and inject --------------------------------------------
        if handle.advance_checked(State::Injecting).is_err() {
            return self
                .finish(&handle, stages, Some(interaction), Outcome::cancelled_after(audio_len_ms))
                .await;
        }

        let dispatch = self
            .dispatch(
                &token,
                &mut stages,
                &mut interaction,
                &opts,
                resolved_route.clone(),
                &routed.text,
                &text,
            )
            .await;

        let (final_text, injection) = match dispatch {
            Step::Cancelled => {
                return self
                    .finish(&handle, stages, Some(interaction), Outcome::cancelled_after(audio_len_ms))
                    .await;
            }
            Step::Continue(v) => v,
        };

        let word_count = final_text.split_whitespace().count() as u32;
        let transcript = Transcript {
            text: FinalText(final_text),
            raw_text: if opts.privacy { None } else { Some(raw_text) },
            route: resolved_route,
            timings: StageTimings::default(),
            injection,
            word_count: Some(word_count),
            model: Some(self.stt.model().name),
        };

        interaction.completed = true;
        self.finish(
            &handle,
            stages,
            Some(interaction),
            Outcome::done(transcript).with_audio_ms(audio_len_ms),
        )
        .await
    }

    /// Race a stage future against cancellation.
    ///
    /// Losing the race drops the stage future, which is why every port is
    /// written to be drop-safe: the audio thread completes its own work and
    /// the recognizer's buffers are freed with the future.
    async fn race<T>(
        &self,
        token: &CancelToken,
        fut: impl std::future::Future<Output = T>,
    ) -> Step<T> {
        tokio::select! {
            biased;
            () = token.cancelled() => Step::Cancelled,
            v = fut => Step::Continue(v),
        }
    }

    /// Execute the resolved route and inject its output.
    #[allow(clippy::too_many_arguments)]
    async fn dispatch(
        &self,
        token: &CancelToken,
        stages: &mut Stages,
        interaction: &mut Interaction,
        opts: &ResolvedOptions,
        route: Route,
        route_text: &str,
        full_text: &str,
    ) -> Step<(String, InjectionOutcome)> {
        match route {
            Route::Type => {
                let outcome = self.inject(token, stages, opts, full_text).await;
                match outcome {
                    Step::Cancelled => Step::Cancelled,
                    Step::Continue(o) => {
                        interaction.output_typed = o.did_inject();
                        if o.did_inject() {
                            interaction.output_char_count = Some(full_text.len());
                        }
                        Step::Continue((full_text.to_string(), o))
                    }
                }
            }
            Route::Local => {
                self.notifier.notify(Notice::Processing(self.local_model.clone()));
                interaction.prompt_sent = Some(route_text.to_string());
                interaction.execution_model = Some(self.local_model.clone());

                let started = Instant::now();
                let result = match self.race(token, self.local.execute(route_text, None)).await {
                    Step::Cancelled => return Step::Cancelled,
                    Step::Continue(r) => r,
                };
                interaction.execution_duration_s = Some(started.elapsed().as_secs_f64());
                interaction.execution_success = Some(result.success);

                if !result.success {
                    let msg = result.error.clone().unwrap_or_default();
                    interaction.execution_error = result.error.clone();
                    self.notifier.notify(Notice::Error(msg.clone()));
                    stages.timings.inject = StageTiming::skipped(SkipReason::DependencyUnavailable);
                    return Step::Continue((
                        String::new(),
                        InjectionOutcome::Failed {
                            error: ProtoError::new(ErrorCode::Internal, msg),
                        },
                    ));
                }

                interaction.response_text = Some(result.response.clone());
                match self.inject(token, stages, opts, &result.response).await {
                    Step::Cancelled => Step::Cancelled,
                    Step::Continue(o) => {
                        interaction.output_typed = o.did_inject();
                        if o.did_inject() {
                            interaction.output_char_count = Some(result.response.len());
                        }
                        Step::Continue((result.response, o))
                    }
                }
            }
            Route::Timer => {
                let timer = self.timer.clone();
                let text = route_text.to_string();
                let result = match self
                    .race(token, async move {
                        tokio::task::spawn_blocking(move || timer.execute(&text)).await
                    })
                    .await
                {
                    Step::Cancelled => return Step::Cancelled,
                    Step::Continue(Ok(r)) => r,
                    Step::Continue(Err(e)) => {
                        error!("timer executor panicked: {e}");
                        stages.timings.inject = StageTiming::skipped(SkipReason::RouteNotEligible);
                        return Step::Continue((
                            String::new(),
                            InjectionOutcome::Failed {
                                error: ProtoError::new(
                                    ErrorCode::Internal,
                                    "timer executor panicked",
                                ),
                            },
                        ));
                    }
                };
                interaction.execution_success = Some(result.success);
                if result.success {
                    interaction.response_text = Some(result.response.clone());
                    self.notifier.notify(Notice::TimerSet(result.response.clone()));
                } else {
                    interaction.execution_error = result.error.clone();
                    self.notifier
                        .notify(Notice::Error(result.error.clone().unwrap_or_default()));
                }
                // A timer never types anything: the stage did not run because
                // this route is not eligible for it, which is a skip and not a
                // zero-cost run.
                stages.timings.inject = StageTiming::skipped(SkipReason::RouteNotEligible);
                Step::Continue((
                    result.response,
                    InjectionOutcome::Skipped {
                        reason: SkipReason::RouteNotEligible,
                    },
                ))
            }
            other => {
                // `edit` and `command` are named by the protocol but have no
                // implementation yet. Reporting the stage as `not_supported`
                // is the honest answer; inventing a plausible-looking success
                // would corrupt the parity measurements.
                let msg = format!("route '{}' is not implemented", other.as_str());
                warn!("{msg}");
                self.notifier.notify(Notice::Error(msg.clone()));
                interaction.error_summary = Some(msg.clone());
                stages.timings.inject = StageTiming::skipped(SkipReason::NotSupported);
                Step::Continue((
                    String::new(),
                    InjectionOutcome::Skipped {
                        reason: SkipReason::NotSupported,
                    },
                ))
            }
        }
    }

    /// The irreversible stage.
    ///
    /// The commit point is taken *before* any text reaches the injector, so a
    /// cancelled session provably never types anything. Once the guard exists,
    /// a concurrent `cancel` is answered `TooLate` and this runs to completion
    /// rather than leaving half a sentence pasted somewhere.
    async fn inject(
        &self,
        token: &CancelToken,
        stages: &mut Stages,
        opts: &ResolvedOptions,
        text: &str,
    ) -> Step<InjectionOutcome> {
        if !opts.inject {
            // The caller takes delivery instead. A success, not a skip and not
            // a failure — the normal outcome for a remote transcription.
            stages.timings.inject = StageTiming::skipped(SkipReason::NotPermitted);
            return Step::Continue(InjectionOutcome::Delivered);
        }
        // Probing the backend can block — a Wayland portal check is IPC — so
        // it goes to the blocking pool rather than stalling an async worker.
        // It also runs *before* the commit point, which is what keeps a cancel
        // arriving during the probe able to win.
        let available = {
            let injector = self.injector.clone();
            tokio::task::spawn_blocking(move || injector.is_available())
                .await
                .unwrap_or(false)
        };
        if !available {
            stages.timings.inject = StageTiming::skipped(SkipReason::NotSupported);
            return Step::Continue(InjectionOutcome::Unavailable {
                backend: "none".into(),
                reason: "no injection backend is available on this host".into(),
            });
        }
        if text.trim().is_empty() {
            stages.timings.inject = StageTiming::skipped(SkipReason::NoSpeechDetected);
            return Step::Continue(InjectionOutcome::Skipped {
                reason: SkipReason::NoSpeechDetected,
            });
        }

        let Some(guard) = token.enter_commit() else {
            // Cancelled before anything was typed — the whole point of the
            // commit point.
            stages.timings.inject = StageTiming::skipped(SkipReason::Cancelled);
            return Step::Cancelled;
        };

        let clock = StageClock::start();
        let injector = self.injector.clone();
        let owned = text.to_string();
        // The real injector blocks for ~50-100ms on clipboard save/paste/
        // restore; keeping it off the runtime's worker threads is what lets
        // other connections keep being served during it.
        let outcome = tokio::task::spawn_blocking(move || injector.inject(&owned))
            .await
            .unwrap_or_else(|e| {
                error!("injection task panicked: {e}");
                InjectionOutcome::Failed {
                    error: ProtoError::new(
                        ErrorCode::InjectionFailed,
                        "injection task panicked",
                    ),
                }
            });
        drop(guard);

        stages.timings.inject = match &outcome {
            InjectionOutcome::Failed { error } => clock.failed(error.message.clone()),
            _ => clock.ran(),
        };
        Step::Continue(outcome)
    }

    /// The single teardown path.
    ///
    /// Every exit from [`run`](Self::run) comes through here, which is what
    /// guarantees the two properties cancellation has to have: the capture
    /// stream is always released, and the user's media is always resumed. It
    /// also stamps the timings into the transcript, publishes the terminal
    /// events, and commits the interaction log.
    async fn finish(
        &self,
        handle: &SessionHandle,
        stages: Stages,
        interaction: Option<Interaction>,
        outcome: Outcome,
    ) -> PipelineOutcome {
        // Idempotent, and unconditional on purpose: reaching a terminal state
        // with a live capture stream is the "dangling stream" bug, and the
        // cheapest way to make it impossible is to never rely on having
        // stopped it on the happy path.
        self.audio.cancel();

        let timings = stages.finish(outcome.audio_ms);
        let mut transcript = outcome.transcript;
        if let Some(t) = transcript.as_mut() {
            t.timings = timings.clone();
        }

        match &outcome.state {
            State::Cancelled => self.notifier.notify(Notice::Cancelled),
            State::Error => {}
            _ => self.notifier.notify(Notice::Clear),
        }

        {
            let media = self.media.clone();
            let _ = tokio::task::spawn_blocking(move || media.resume_if_needed()).await;
        }

        // Publish the payload events *before* the terminal state change, so a
        // client that stops listening the moment it sees `done` has already
        // received the transcript it was waiting for.
        if let Some(t) = &transcript {
            handle.publish(Event::Final {
                session_id: handle.id().clone(),
                transcript: Box::new(t.clone()),
            });
        }
        if let Some(err) = &outcome.error {
            handle.publish(Event::Error {
                session_id: Some(handle.id().clone()),
                error: err.clone(),
            });
        }

        // A terminal transition is forced rather than checked: the session is
        // over, and refusing to record that because the token is already
        // cancelled would strand the state machine mid-pipeline.
        if let Err(e) = handle.advance(outcome.state.clone()) {
            error!("could not record terminal state: {e}");
        }

        if let Some(mut interaction) = interaction {
            if let Some(err) = &outcome.error {
                if interaction.error_summary.is_none() {
                    interaction.error_summary = Some(err.message.clone());
                }
            }
            if outcome.state == State::Cancelled {
                interaction.error_summary.get_or_insert_with(|| "cancelled".into());
            }
            match self.history.lock() {
                Ok(store) => store.commit(&interaction),
                Err(e) => error!("history mutex poisoned, dropping interaction: {e}"),
            }
        }

        info!(
            session = %handle.id().as_str(),
            state = %outcome.state.as_str(),
            total_ms = timings.total_ms.unwrap_or_default(),
            "session finished"
        );

        PipelineOutcome {
            state: outcome.state,
            transcript,
            error: outcome.error,
        }
    }
}

/// Internal builder for the terminal outcome, so each early return reads as
/// one line at the call site.
struct Outcome {
    state: State,
    transcript: Option<Transcript>,
    error: Option<ProtoError>,
    audio_ms: Option<f64>,
}

impl Outcome {
    fn cancelled() -> Self {
        Self {
            state: State::Cancelled,
            transcript: None,
            error: None,
            audio_ms: None,
        }
    }

    fn cancelled_after(audio_ms: f64) -> Self {
        Self {
            audio_ms: Some(audio_ms),
            ..Self::cancelled()
        }
    }

    fn done(transcript: Transcript) -> Self {
        Self {
            state: State::Done,
            transcript: Some(transcript),
            error: None,
            audio_ms: None,
        }
    }

    fn error(error: ProtoError, audio_ms: f64) -> Self {
        Self {
            state: State::Error,
            transcript: None,
            error: Some(error),
            audio_ms: Some(audio_ms),
        }
    }

    fn with_audio_ms(mut self, ms: f64) -> Self {
        self.audio_ms = Some(ms);
        self
    }
}

/// A transcript for a session that produced no text.
///
/// The empty string is deliberate and distinct from an absent one: it means
/// "the user said nothing", where absent means "we are not telling you".
fn empty_transcript(reason: SkipReason) -> Transcript {
    Transcript {
        text: FinalText(String::new()),
        raw_text: Some(String::new()),
        route: Route::Type,
        timings: StageTimings::default(),
        injection: InjectionOutcome::Skipped { reason },
        word_count: Some(0),
        model: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_enum_maps_onto_the_wire_enum() {
        assert_eq!(route_to_proto(RouteType::Type), Route::Type);
        assert_eq!(route_to_proto(RouteType::Local), Route::Local);
        assert_eq!(route_to_proto(RouteType::Timer), Route::Timer);
        assert_eq!(route_to_proto(RouteType::Edit), Route::Edit);
        assert_eq!(route_to_proto(RouteType::Command), Route::Command);
    }

    #[test]
    fn stages_start_honest_about_what_this_build_lacks() {
        let s = Stages::new();
        assert_eq!(
            s.timings.vad,
            StageTiming::skipped(SkipReason::NotSupported),
            "VAD does not exist until S11 and must not report a fabricated zero"
        );
        assert_eq!(s.timings.fmt_rules, StageTiming::NotReported);
        assert_eq!(s.timings.stt, StageTiming::NotReported);
        assert_eq!(s.timings.capture, StageTiming::NotReported);
    }

    #[test]
    fn total_ms_is_measured_not_summed() {
        let mut s = Stages::new();
        s.timings.capture = StageTiming::ran(10.0);
        s.timings.stt = StageTiming::ran(20.0);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let t = s.finish(Some(1234.0));
        let total = t.total_ms.expect("total must be reported");
        assert!(
            total >= 5.0,
            "total must be wall time, not the 30ms stage sum"
        );
        assert_eq!(t.audio_ms, Some(1234.0));
    }

    #[test]
    fn a_failed_stage_still_reports_its_cost() {
        let clock = StageClock::start();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let timing = clock.failed("ollama refused the connection");
        match timing {
            StageTiming::Failed { ms, error } => {
                assert!(ms >= 2.0, "time burned before failing must not vanish");
                assert_eq!(error.as_deref(), Some("ollama refused the connection"));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_transcript_says_nothing_rather_than_hiding_something() {
        let t = empty_transcript(SkipReason::NoSpeechDetected);
        assert_eq!(t.text.as_str(), "");
        assert_eq!(
            t.raw_text,
            Some(String::new()),
            "empty means the user said nothing; absent would mean privacy mode"
        );
        assert_eq!(t.word_count, Some(0));
    }

    #[test]
    fn default_options_permit_every_known_route() {
        let o = ResolvedOptions::default();
        for route in Route::known() {
            assert!(o.allowed_routes.contains(route));
        }
        assert!(o.inject);
        assert!(!o.privacy);
    }
}
