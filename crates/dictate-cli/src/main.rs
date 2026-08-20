//! `dictate` — the control client.
//!
//! Speaks the protocol and nothing else. It has no pipeline, no state machine,
//! and deliberately no dependency on `dictate-core`: a keybinding runs this
//! binary on every dictation, and linking whisper.cpp into it would trade real
//! startup latency for a little shared code.
//!
//! # Toggle is resolved here, and that is a compromise
//!
//! The protocol has no `toggle` command — it has `start_dictation` and `stop`.
//! So `dictate toggle` reads the state and then acts on it, which leaves a
//! window where another actor can move first. The daemon closes that window
//! for itself (SIGUSR1 toggles *inside* the engine task, atomically), but a
//! socket client cannot borrow that guarantee without a protocol change.
//!
//! The consequence is bounded and honest: if the CLI loses the race it is told
//! `busy` or `no_active_session`, and it reports that rather than retrying into
//! a second session. Jake's hotkey path goes through the signal shim and is not
//! affected. Adding a `toggle` command would be an additive protocol change —
//! flagged for the Epic-lead rather than taken unilaterally, since S32 and S33
//! share this contract.

mod client;
mod render;

use anyhow::{bail, Result};
use dictate_proto::{Command, CommandResult, DictationMode, Event, HistoryQuery, State};

use crate::client::Client;

const USAGE: &str = "\
dictate — control client for the dictation daemon

USAGE:
    dictate <COMMAND> [OPTIONS]

COMMANDS:
    toggle              Start recording, or stop and transcribe if recording
    start               Start recording
    stop                Stop recording and run the pipeline
    cancel              Abandon the current session, injecting nothing
    status              Show daemon and session state
    tail                Stream events until interrupted
    history             List past dictations, or purge with --purge
    dict                Personal dictionary (not implemented until S22)

OPTIONS:
    --limit <N>         history: rows to return (default 20)
    --text <QUERY>      history: substring to match
    --errors            history: only sessions that failed
    --purge             history: permanently delete all stored dictations
    --analytics         history: show WPM, daily words, and streaks
    --events <A,B>      tail: only these event types
    --json              print raw protocol JSON instead of a summary
    --socket <PATH>     override the control socket
    -h, --help          this help
    -V, --version       version

ENVIRONMENT:
    DICTATE_SOCKET      default socket path override
";

#[tokio::main(flavor = "current_thread")]
async fn main() {
    match run().await {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("dictate: {e}");
            std::process::exit(1);
        }
    }
}

/// Exit codes: 0 success, 1 failure, 2 usage.
async fn run() -> Result<i32> {
    let args = Args::parse(std::env::args().skip(1))?;

    if args.help {
        print!("{USAGE}");
        return Ok(0);
    }
    if args.version {
        println!("dictate {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }
    let Some(command) = args.command.clone() else {
        eprint!("{USAGE}");
        return Ok(2);
    };

    if let Some(path) = &args.socket {
        // SAFETY: single-threaded runtime, set before any concurrent access.
        unsafe { std::env::set_var(client::SOCKET_ENV, path) };
    }
    let mut client = Client::connect_default().await?;

    match command.as_str() {
        "toggle" => toggle(&mut client, &args).await,
        "start" => {
            let result = client
                .request(Command::StartDictation {
                    mode: DictationMode::Toggle,
                    options: None,
                })
                .await?;
            render::result(&result, args.json);
            Ok(0)
        }
        "stop" => {
            let result = client.request(Command::Stop).await?;
            render::result(&result, args.json);
            Ok(0)
        }
        "cancel" => {
            let result = client.request(Command::Cancel).await?;
            render::result(&result, args.json);
            Ok(0)
        }
        "status" => {
            let result = client.request(Command::GetStatus).await?;
            render::result(&result, args.json);
            Ok(0)
        }
        "tail" => tail(&mut client, &args).await,
        "history" => history(&mut client, &args).await,
        "dict" => dict(&mut client, &args).await,
        other => {
            eprintln!("dictate: unknown command '{other}'\n");
            eprint!("{USAGE}");
            Ok(2)
        }
    }
}

/// Start if idle, stop if recording.
///
/// See the module docs for why this is two round trips and what happens when
/// it loses the race.
async fn toggle(client: &mut Client, args: &Args) -> Result<i32> {
    let CommandResult::Status(status) = client.request(Command::GetStatus).await? else {
        bail!("the daemon did not answer get_status with a status");
    };

    let command = match status.state {
        State::Recording => Command::Stop,
        s if s.is_terminal() || s == State::Idle => Command::StartDictation {
            mode: DictationMode::Toggle,
            options: None,
        },
        // Mid-pipeline: neither verb applies. Saying so beats queueing a
        // second session behind one the user cannot see.
        other => {
            eprintln!(
                "dictate: session is {}; wait for it to finish, or `dictate cancel`",
                other.as_str()
            );
            return Ok(1);
        }
    };

    match client.try_request(command).await? {
        Ok(result) => {
            render::result(&result, args.json);
            Ok(0)
        }
        // Lost the race with another actor. Report it; do not retry into a
        // state the user did not ask for.
        Err(e) => {
            eprintln!("dictate: {} ({})", e.message, e.code.as_str());
            Ok(1)
        }
    }
}

async fn tail(client: &mut Client, args: &Args) -> Result<i32> {
    let events = args
        .events
        .as_ref()
        .map(|s| s.split(',').map(|v| v.trim().to_string()).collect())
        .unwrap_or_default();
    client.request(Command::Subscribe { events }).await?;

    if !args.json {
        eprintln!("watching {} — ctrl-c to stop", client::socket_path().display());
    }
    loop {
        let event = client.next_event().await?;
        render::event(&event, args.json);
        if matches!(event, Event::Unknown) {
            // A newer daemon sent something this build does not model. Not an
            // error — the client degrades and carries on.
            continue;
        }
    }
}

async fn history(client: &mut Client, args: &Args) -> Result<i32> {
    if args.analytics {
        return match client.try_request(Command::GetHistoryAnalytics).await? {
            Ok(result) => {
                render::result(&result, args.json);
                Ok(0)
            }
            Err(e) => {
                eprintln!("dictate: {} ({})", e.message, e.code.as_str());
                Ok(1)
            }
        };
    }
    if args.purge {
        return match client.try_request(Command::PurgeHistory).await? {
            Ok(result) => {
                render::result(&result, args.json);
                Ok(0)
            }
            Err(e) => {
                eprintln!("dictate: {} ({})", e.message, e.code.as_str());
                Ok(1)
            }
        };
    }
    let query = HistoryQuery {
        text: args.text.clone(),
        limit: Some(args.limit.unwrap_or(20)),
        errors_only: args.errors.then_some(true),
        ..HistoryQuery::default()
    };
    match client.try_request(Command::QueryHistory { query }).await? {
        Ok(result) => {
            render::result(&result, args.json);
            Ok(0)
        }
        Err(e) => {
            eprintln!("dictate: {} ({})", e.message, e.code.as_str());
            Ok(1)
        }
    }
}

/// The dictionary is wired to the protocol but has no backing store until S22.
///
/// The command is sent for real rather than short-circuited locally, so what
/// the user sees is the daemon's own answer — and the day S22 lands, this
/// starts working with no change here.
async fn dict(client: &mut Client, args: &Args) -> Result<i32> {
    match client
        .try_request(Command::ListDictionary {
            query: args.text.clone(),
            limit: args.limit,
        })
        .await?
    {
        Ok(result) => {
            render::result(&result, args.json);
            Ok(0)
        }
        Err(e) => {
            eprintln!(
                "dictate: the personal dictionary is not available in this build \
                 — {} ({})",
                e.message,
                e.code.as_str()
            );
            Ok(1)
        }
    }
}

/// Hand-rolled argument parsing.
///
/// A dependency-free parse keeps this binary small and its startup short; the
/// surface is one subcommand and a handful of flags.
#[derive(Debug, Default, Clone)]
struct Args {
    command: Option<String>,
    limit: Option<u32>,
    text: Option<String>,
    events: Option<String>,
    socket: Option<String>,
    errors: bool,
    purge: bool,
    analytics: bool,
    json: bool,
    help: bool,
    version: bool,
}

impl Args {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self> {
        let mut out = Self::default();
        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-h" | "--help" => out.help = true,
                "-V" | "--version" => out.version = true,
                "--json" => out.json = true,
                "--errors" => out.errors = true,
                "--purge" => out.purge = true,
                "--analytics" => out.analytics = true,
                "--limit" => {
                    let raw = args.next().ok_or_else(|| anyhow::anyhow!("--limit needs a value"))?;
                    out.limit = Some(raw.parse().map_err(|_| {
                        anyhow::anyhow!("--limit must be a whole number, got '{raw}'")
                    })?);
                }
                "--text" => {
                    out.text =
                        Some(args.next().ok_or_else(|| anyhow::anyhow!("--text needs a value"))?)
                }
                "--events" => {
                    out.events =
                        Some(args.next().ok_or_else(|| anyhow::anyhow!("--events needs a value"))?)
                }
                "--socket" => {
                    out.socket =
                        Some(args.next().ok_or_else(|| anyhow::anyhow!("--socket needs a value"))?)
                }
                other if other.starts_with('-') => bail!("unknown option '{other}'"),
                other if out.command.is_none() => out.command = Some(other.to_string()),
                other => bail!("unexpected argument '{other}'"),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args> {
        Args::parse(args.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn a_bare_subcommand_parses() {
        let a = parse(&["toggle"]).unwrap();
        assert_eq!(a.command.as_deref(), Some("toggle"));
        assert!(!a.json);
    }

    #[test]
    fn flags_may_precede_or_follow_the_subcommand() {
        let a = parse(&["--json", "status"]).unwrap();
        assert_eq!(a.command.as_deref(), Some("status"));
        assert!(a.json);

        let b = parse(&["status", "--json"]).unwrap();
        assert_eq!(b.command.as_deref(), Some("status"));
        assert!(b.json);
    }

    #[test]
    fn history_options_parse() {
        let a = parse(&["history", "--limit", "5", "--text", "hello", "--errors"]).unwrap();
        assert_eq!(a.limit, Some(5));
        assert_eq!(a.text.as_deref(), Some("hello"));
        assert!(a.errors);
    }

    #[test]
    fn a_non_numeric_limit_is_a_clear_error_not_a_silent_default() {
        let err = parse(&["history", "--limit", "lots"]).unwrap_err();
        assert!(err.to_string().contains("whole number"), "{err}");
    }

    #[test]
    fn a_flag_missing_its_value_is_an_error() {
        assert!(parse(&["history", "--limit"]).is_err());
        assert!(parse(&["tail", "--events"]).is_err());
        assert!(parse(&["status", "--socket"]).is_err());
    }

    #[test]
    fn an_unknown_option_is_rejected_rather_than_ignored() {
        let err = parse(&["status", "--colour"]).unwrap_err();
        assert!(err.to_string().contains("--colour"), "{err}");
    }

    #[test]
    fn a_second_positional_argument_is_rejected() {
        assert!(parse(&["status", "extra"]).is_err());
    }

    #[test]
    fn help_and_version_parse_without_a_subcommand() {
        assert!(parse(&["--help"]).unwrap().help);
        assert!(parse(&["-V"]).unwrap().version);
        assert!(parse(&[]).unwrap().command.is_none());
    }

    #[test]
    fn the_events_filter_splits_on_commas() {
        let a = parse(&["tail", "--events", "state_changed, final"]).unwrap();
        let parsed: Vec<String> = a
            .events
            .unwrap()
            .split(',')
            .map(|v| v.trim().to_string())
            .collect();
        assert_eq!(parsed, vec!["state_changed", "final"]);
    }
}
