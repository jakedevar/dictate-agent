//! `dictate` — the control client.
//!
//! Speaks the protocol for daemon control. `model` is deliberately local: it
//! can establish a verified first-run model before a daemon exists.
//!
//! # Toggle is one atomic protocol request
//!
//! `dictate toggle` sends protocol `toggle` once. The daemon resolves whether
//! to start or stop inside its single-writer engine actor, sharing the same
//! operation used by SIGUSR1 and avoiding a `get_status`-then-act race.

mod client;
mod render;

use anyhow::{bail, Result};
use dictate_proto::{Command, DictationMode, Event, HistoryQuery};

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
    history             List past dictations
    dict                Personal dictionary (not implemented until S22)
    model pull [NAME]   Download and SHA-256 verify a pinned GGUF (default large-v3-turbo)
    model list          List catalog models and local verification state

OPTIONS:
    --limit <N>         history: rows to return (default 20)
    --text <QUERY>      history: substring to match
    --errors            history: only sessions that failed
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
    if command == "model" {
        return model(&args);
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

/// Atomically start if idle, or stop if recording.
async fn toggle(client: &mut Client, args: &Args) -> Result<i32> {
    match client.try_request(Command::Toggle).await? {
        Ok(result) => {
            render::result(&result, args.json);
            Ok(0)
        }
        // Do not retry: a busy result describes the state observed atomically
        // by the engine, and a retry could become a different user action.
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

/// Manage pinned local Whisper models without requiring a running daemon.
fn model(args: &Args) -> Result<i32> {
    let action = args.model_action.as_deref().unwrap_or("list");
    let config = dictate_stt::WhisperConfig::default();
    let cache_dir = dictate_stt::config::expand_tilde(&config.model_path)
        .parent()
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("default Whisper model path has no parent"))?;
    let manager = dictate_stt::ModelManager::new(cache_dir);
    match action {
        "pull" => {
            let name = args.model_name.as_deref().unwrap_or("large-v3-turbo");
            let path = manager.ensure(name)?;
            let spec = dictate_stt::catalog_model(name).expect("ensure validated catalog name");
            println!("pulled {} ({}, SHA-256 verified)\n{}", spec.id, spec.revision, path.display());
            Ok(0)
        }
        "list" => {
            for item in manager.list()? {
                let state = match (item.present, item.verified) {
                    (false, _) => "missing",
                    (true, true) => "verified",
                    (true, false) => "CORRUPT",
                };
                println!(
                    "{:<16} {:<8} {:>10}  {}\n  {}",
                    item.spec.id,
                    state,
                    item.bytes_on_disk.unwrap_or(item.spec.bytes),
                    item.spec.revision,
                    item.path.display()
                );
            }
            Ok(0)
        }
        other => bail!("model expects `pull [NAME]` or `list`, got '{other}'"),
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
    model_action: Option<String>,
    model_name: Option<String>,
    limit: Option<u32>,
    text: Option<String>,
    events: Option<String>,
    socket: Option<String>,
    errors: bool,
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
                other if out.command.as_deref() == Some("model") && out.model_action.is_none() => {
                    out.model_action = Some(other.to_string())
                }
                other if out.command.as_deref() == Some("model") && out.model_name.is_none() => {
                    out.model_name = Some(other.to_string())
                }
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
    fn model_subcommands_accept_an_optional_model_name() {
        let pull = parse(&["model", "pull", "tiny.en"]).unwrap();
        assert_eq!(pull.model_action.as_deref(), Some("pull"));
        assert_eq!(pull.model_name.as_deref(), Some("tiny.en"));
        let list = parse(&["model", "list"]).unwrap();
        assert_eq!(list.model_action.as_deref(), Some("list"));
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
