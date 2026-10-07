//! `dictate transcribe <file.wav>` — hand a recording to the daemon and print
//! what it heard.
//!
//! A thin protocol client: the file's bytes go to the daemon inside a
//! `transcribe_audio` request and the transcript comes back as the response.
//! The daemon does the decoding, resampling, recognition and formatting, which
//! is why this binary stays free of the transcription stack.

use std::path::Path;

use anyhow::{bail, Context, Result};
use dictate_proto::{
    AudioFormat, AudioSource, Command, CommandResult, ErrorCode, Route, SessionOptions, Transcript,
};

use crate::client::Client;
use crate::render;

/// What the caller asked for on the command line.
#[derive(Debug, Clone, Default)]
pub struct TranscribeArgs {
    /// The WAV file.
    pub file: String,
    /// `--inject`: type the result into the focused window.
    pub inject: bool,
    /// `--route R`: force a route instead of consulting the router.
    pub route: Option<String>,
    /// `--privacy`: persist nothing about this session.
    pub privacy: bool,
    /// `--json`: print the raw protocol result.
    pub json: bool,
}

/// Whether `bytes` start like a RIFF/WAVE file.
fn looks_like_wav(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE"
}

/// The size a payload will occupy on the wire once base64-encoded, plus the
/// JSON envelope around it.
fn encoded_size(payload_bytes: usize) -> u64 {
    (payload_bytes as u64).div_ceil(3) * 4 + 512
}

/// Parse `--route`, refusing a route this build does not know rather than
/// forwarding a typo that would silently fall through to the router.
fn parse_route(name: &str) -> Result<Route> {
    let route = Route::from(name);
    if !route.is_known() {
        let known: Vec<&str> = Route::known().iter().map(Route::as_str).collect();
        bail!(
            "unknown route '{name}'; choose one of: {}",
            known.join(", ")
        );
    }
    Ok(route)
}

/// Build the request for `args` and `bytes`.
pub fn build_command(args: &TranscribeArgs, bytes: Vec<u8>) -> Result<Command> {
    let route = args.route.as_deref().map(parse_route).transpose()?;
    Ok(Command::TranscribeAudio {
        audio: AudioSource::Inline {
            format: AudioFormat::wav(),
            data: bytes,
        },
        options: Some(SessionOptions {
            route,
            // Explicit either way: what an upload does with the text must never
            // depend on a default the daemon may change.
            inject: Some(args.inject),
            privacy: args.privacy.then_some(true),
            ..SessionOptions::default()
        }),
    })
}

/// Run `dictate transcribe`.
pub async fn run(client: &mut Client, args: &TranscribeArgs) -> Result<i32> {
    let path = Path::new(&args.file);
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if !looks_like_wav(&bytes) {
        bail!(
            "{} is not a WAV file (expected a RIFF/WAVE header); convert it first, \
             e.g. `ffmpeg -i in.mp3 out.wav`",
            path.display()
        );
    }

    // Fail fast, locally, with the daemon's own advertised limit, instead of
    // making it read and refuse a huge line.
    let limit = u64::from(client.hello.capabilities.limits.max_message_bytes);
    let wire = encoded_size(bytes.len());
    if wire > limit {
        bail!(
            "{} is {} bytes ({wire} once encoded); this daemon accepts requests of at most {limit} \
             bytes — raise [upload] max_bytes and restart it, or send a shorter clip",
            path.display(),
            bytes.len()
        );
    }

    let command = build_command(args, bytes)?;
    match client.try_request(command).await? {
        Ok(CommandResult::Transcript(t)) => {
            print_transcript(&t, args.json);
            Ok(0)
        }
        Ok(other) => bail!(
            "expected a transcript, the daemon answered {}",
            other.name()
        ),
        Err(e) => {
            eprintln!(
                "dictate: {} ({})",
                crate::safe::inline(&e.message),
                e.code.as_str()
            );
            if let Some(detail) = e.detail() {
                eprintln!("dictate: {}", crate::safe::inline(&detail.to_string()));
            }
            if e.code == ErrorCode::Busy {
                eprintln!("dictate: another session is running; retry in a moment");
            }
            Ok(1)
        }
    }
}

/// Print the transcript.
///
/// Text goes to stdout on its own so the command composes in a pipeline
/// (`dictate transcribe f.wav | wc -w`); the route/timing summary goes to
/// stderr. `--json` prints the raw result instead.
fn print_transcript(t: &Transcript, json: bool) {
    if json {
        render::result(&CommandResult::Transcript(Box::new(t.clone())), true);
        return;
    }
    // stdout is the data channel: raw when piped, defused on a terminal.
    if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        println!("{}", crate::safe::block(t.text.as_str()));
    } else {
        println!("{}", t.text.as_str());
    }
    eprintln!("{}", render::transcript_summary(t));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> TranscribeArgs {
        TranscribeArgs {
            file: "x.wav".into(),
            ..Default::default()
        }
    }

    fn options(c: &Command) -> &SessionOptions {
        c.options().expect("transcribe carries options")
    }

    #[test]
    fn by_default_the_daemon_is_told_not_to_type() {
        let c = build_command(&args(), vec![1]).unwrap();
        assert_eq!(
            options(&c).inject,
            Some(false),
            "explicit, not merely omitted"
        );
        assert_eq!(options(&c).route, None);
        assert_eq!(options(&c).privacy, None);
    }

    #[test]
    fn the_flags_map_onto_session_options() {
        let c = build_command(
            &TranscribeArgs {
                inject: true,
                privacy: true,
                route: Some("type".into()),
                ..args()
            },
            vec![1],
        )
        .unwrap();
        assert_eq!(options(&c).inject, Some(true));
        assert_eq!(options(&c).privacy, Some(true));
        assert_eq!(options(&c).route, Some(Route::Type));
    }

    #[test]
    fn a_mistyped_route_is_an_error_here_not_a_silent_fallthrough() {
        let err = build_command(
            &TranscribeArgs {
                route: Some("tpye".into()),
                ..args()
            },
            vec![1],
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("unknown route 'tpye'") && err.contains("timer"),
            "{err}"
        );
    }

    #[test]
    fn the_wav_upload_declares_itself_as_wav() {
        let c = build_command(&args(), vec![1, 2, 3]).unwrap();
        match c {
            Command::TranscribeAudio {
                audio: AudioSource::Inline { format, data },
                ..
            } => {
                assert_eq!(format, AudioFormat::wav());
                assert_eq!(data, vec![1, 2, 3]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn wav_files_are_recognised_by_their_header_not_their_name() {
        assert!(looks_like_wav(b"RIFF\x24\x00\x00\x00WAVEfmt "));
        assert!(!looks_like_wav(b"ID3\x04 an mp3"));
        assert!(!looks_like_wav(b"RIFF"));
        assert!(!looks_like_wav(b""));
    }

    #[test]
    fn the_encoded_size_accounts_for_base64_inflation() {
        // 3 bytes -> 4 chars; the envelope adds a fixed allowance.
        assert_eq!(encoded_size(3), 4 + 512);
        assert_eq!(encoded_size(4), 8 + 512);
        assert!(encoded_size(1_000_000) > 1_333_333);
    }
}
