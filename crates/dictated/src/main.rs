//! The `dictated` binary.
//!
//! Thin on purpose: everything worth testing lives in the library beside it,
//! so `tests/control_plane.rs` drives the same daemon this starts.

use anyhow::Result;
use tracing_subscriber::EnvFilter;

use dictate_core::config;

fn main() -> Result<()> {
    init_tracing()?;

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--check") {
        return check_all_dependencies();
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("dictated {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return Ok(());
    }

    let config = config::load_config(None)?;

    // Built here rather than via `#[tokio::main]` so the worker count is a
    // deliberate choice: the daemon is almost entirely idle, and its blocking
    // work (clipboard paste, SQLite, whisper) goes to the blocking pool
    // regardless.
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(dictated::run(config))
}

fn init_tracing() -> Result<()> {
    // The pre-S00 crate was a single `dictate_agent` target, so one directive
    // covered every module. The workspace split spread that across crates, so
    // the default floor names each one.
    let mut filter = EnvFilter::from_default_env();
    for target in [
        "dictate_core",
        "dictate_audio",
        "dictate_stt",
        "dictate_fmt",
        "dictate_history",
        "dictate_inject",
        "dictated",
    ] {
        filter = filter.add_directive(format!("{target}=info").parse()?);
    }
    tracing_subscriber::fmt().with_env_filter(filter).init();
    Ok(())
}

fn print_help() {
    println!(
        "dictated {} — dictation daemon

USAGE:
    dictated [OPTIONS]

OPTIONS:
    --check      Verify external tools this daemon shells out to
    --version    Print the version
    --help       Print this help

CONTROL:
    dictate toggle | cancel | status | tail     (unix socket)
    kill -USR1 <pid>                            (toggle, same code path)
    kill -USR2 <pid>                            (cancel, same code path)
",
        env!("CARGO_PKG_VERSION")
    );
}

/// Check each external program still used as a subprocess.
fn check_all_dependencies() -> Result<()> {
    println!("Dictate Agent — Dependency Check");
    println!("================================");

    let deps = [
        ("playerctl", "Media control", "sudo apt install playerctl"),
        (
            "systemd-run",
            "Timer creation",
            "Part of systemd (should be installed)",
        ),
        ("dunstify", "Timer notifications", "sudo apt install dunst"),
        ("play", "Timer alarm sound (sox)", "sudo apt install sox"),
        ("ollama", "Local LLM inference", "See https://ollama.ai"),
    ];

    let mut all_ok = true;
    for (cmd, description, install_hint) in &deps {
        let found = std::process::Command::new("which")
            .arg(cmd)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if found {
            println!("  [OK]      {cmd:<15} {description}");
        } else {
            println!("  [MISSING] {cmd:<15} {description} — {install_hint}");
            all_ok = false;
        }
    }

    println!();
    if all_ok {
        println!("All dependencies found.");
        Ok(())
    } else {
        println!("Some dependencies are missing. Install them for full functionality.");
        std::process::exit(1);
    }
}
