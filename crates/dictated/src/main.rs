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

    let config_path = flag_value(&args, "--config").map(std::path::PathBuf::from);
    if args.iter().any(|a| a == "--check-config") {
        return check_config(config_path.as_deref());
    }
    let rotate = args.iter().any(|a| a == "--rotate-api-token");
    if rotate || args.iter().any(|a| a == "--api-token") {
        return api_token(config_path.as_deref(), rotate);
    }

    let (config, report) = config::load_config_with_report(config_path.as_deref())?;
    for warning in &report.warnings {
        tracing::warn!("config: {warning}");
    }
    if !report.errors.is_empty() {
        anyhow::bail!(
            "invalid configuration in {}:\n  - {}",
            report.path.display(),
            report.errors.join("\n  - ")
        );
    }

    // Built here rather than via `#[tokio::main]` so the worker count is a
    // deliberate choice: the daemon is almost entirely idle, and its blocking
    // work (clipboard paste, SQLite, whisper) goes to the blocking pool
    // regardless.
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(dictated::run_with_report(config, report))
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
        "dictate_context",
        "dictate_server",
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
    --check                 Verify external tools this daemon shells out to
    --check-config          Print the effective configuration and any warnings, then
                            exit (non-zero if the configuration is invalid)
    --config <PATH>         Read this config file instead of
                            $XDG_CONFIG_HOME/dictate-agent/config.toml
    --api-token             Print the network API bearer token, creating it if
                            there is none (token on stdout; file, URL and TLS
                            fingerprint on stderr). Pair a client with this.
    --rotate-api-token      Replace the token; a running daemon accepts only the
                            new one from its next request on
    --version               Print the version
    --help                  Print this help

CONTROL:
    dictate toggle | cancel | status | tail     (unix socket)
    kill -USR1 <pid>                            (toggle, same code path)
    kill -USR2 <pid>                            (cancel, same code path)
    [api] enabled = true                        (network API, S33: loopback,
                                                 bearer token, transcription only)
",
        env!("CARGO_PKG_VERSION")
    );
}

/// The value following `flag`, if present.
fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

/// `dictated --check-config`: load the configuration exactly as the daemon
/// would, print what it resolved to and everything it had to say about the
/// file, and exit non-zero when the daemon would refuse to start.
fn check_config(path: Option<&std::path::Path>) -> Result<()> {
    let (config, report) = config::load_config_with_report(path)?;
    println!("config file : {}", report.path.display());
    println!(
        "status      : {}",
        if !report.existed {
            "not found — built-in defaults apply"
        } else {
            "loaded"
        }
    );
    println!();
    println!("effective configuration:");
    println!("{config:#?}");
    println!();
    if report.warnings.is_empty() {
        println!("warnings: none");
    } else {
        println!("warnings ({}):", report.warnings.len());
        for w in &report.warnings {
            println!("  - {w}");
        }
    }
    if report.errors.is_empty() {
        println!("errors  : none");
        Ok(())
    } else {
        println!("errors ({}):", report.errors.len());
        for e in &report.errors {
            println!("  - {e}");
        }
        std::process::exit(1);
    }
}

/// `dictated --api-token` / `--rotate-api-token`: issue the network API's
/// bearer token (design §5). The token is the only thing on stdout, so it can
/// be piped; everything a pairing needs besides it goes to stderr.
fn api_token(path: Option<&std::path::Path>, rotate: bool) -> Result<()> {
    let (config, _report) = config::load_config_with_report(path)?;
    let api = &config.api;
    let file = api.token_path();
    let token = if rotate {
        dictate_server::token::rotate(&file)?
    } else {
        let (token, created) = dictate_server::token::ensure(&file)?;
        if created {
            eprintln!("created a new API token");
        }
        token
    };
    println!("{token}");
    eprintln!("token file  : {}", file.display());
    let scheme = if api.tls_cert.trim().is_empty() {
        "http"
    } else {
        "https"
    };
    if api.enabled {
        eprintln!("API         : {scheme}://{}/v1/", api.bind.trim());
    } else {
        eprintln!("API         : disabled (set [api] enabled = true)");
    }
    if !api.tls_cert.trim().is_empty() {
        match dictate_server::tls::certificate_fingerprint(std::path::Path::new(
            api.tls_cert.trim(),
        )) {
            Ok(fp) => eprintln!("TLS SHA-256 : {fp}  (pin this in the client)"),
            Err(e) => eprintln!("TLS         : {e}"),
        }
    }
    if rotate {
        eprintln!("the previous token stops working on a running daemon's next request");
    }
    Ok(())
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
