use anyhow::Result;
use tracing_subscriber::EnvFilter;

use dictate_core::{agent, config};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing (replaces Python's print statements).
    //
    // Pre-workspace-split this was a single crate ("dictate_agent"), so one
    // `dictate_agent=info` directive covered every module. The split spread
    // that code across several crates, so the default floor now has to name
    // each of them individually to keep the same log verbosity as before.
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

    // Parse args: --check flag for dependency verification
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--check") {
        return check_all_dependencies();
    }

    // Load config
    let config = config::load_config(None)?;

    // Create and run agent
    let mut agent = agent::DictateAgent::new(config).await?;
    agent.run().await
}

/// Check each external program that's still used as a subprocess.
/// Port of main.py check_all_dependencies pattern.
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
            println!("  [OK]      {:<15} {}", cmd, description);
        } else {
            println!("  [MISSING] {:<15} {} — {}", cmd, description, install_hint);
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
