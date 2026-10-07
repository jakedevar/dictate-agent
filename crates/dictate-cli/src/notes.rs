//! The scratchpad (S35): `dictate notes`. Speaks only the protocol, like
//! `snippet`; the one local act is putting a note on the clipboard.
use crate::{client::Client, render, safe};
use anyhow::{anyhow, bail, Context, Result};
use dictate_proto::{
    Command, CommandResult, DictationMode, ErrorCode, Note, Route, SessionOptions,
};
use std::io::Write;
use std::process::{Command as Process, Stdio};

pub async fn run(
    client: &mut Client,
    raw: &[String],
    limit: Option<u32>,
    text: Option<String>,
    json: bool,
) -> Result<i32> {
    let args = Args::parse(raw, text)?;
    let result = execute(client, &args, limit, json).await;
    if let Err(e) = &result {
        if e.to_string()
            .contains(ErrorCode::UnsupportedCommand.as_str())
        {
            eprintln!("dictate: notes are not available in this daemon — {e}");
            return Ok(1);
        }
    }
    result.map(|()| 0)
}

#[derive(Debug, PartialEq)]
enum Action {
    List { query: Option<String> },
    Show(i64),
    Copy(i64),
    Rm(i64),
    New,
}

#[derive(Debug)]
struct Args {
    action: Action,
}

impl Args {
    fn parse(raw: &[String], text_flag: Option<String>) -> Result<Self> {
        let verb = raw.first().map_or("list", String::as_str);
        let rest = raw.get(1..).unwrap_or_default();
        let id = |what: &str| -> Result<i64> {
            match rest {
                [one] => one
                    .parse()
                    .map_err(|_| anyhow!("notes {what} needs a note number, got '{one}'")),
                [] => bail!("notes {what} needs a note number (see `dictate notes list`)"),
                _ => bail!("notes {what} takes exactly one note number"),
            }
        };
        let action = match verb {
            "list" => {
                if !rest.is_empty() {
                    bail!("notes list takes no positional value (use `notes search QUERY`)");
                }
                Action::List { query: text_flag }
            }
            "search" => {
                let query = rest.join(" ");
                if query.trim().is_empty() {
                    bail!("notes search needs something to look for");
                }
                Action::List { query: Some(query) }
            }
            "show" => Action::Show(id("show")?),
            "copy" => Action::Copy(id("copy")?),
            "rm" => Action::Rm(id("rm")?),
            "new" => {
                if !rest.is_empty() {
                    bail!("notes new takes no arguments");
                }
                Action::New
            }
            other => bail!("unknown notes action '{other}' (list, search, show, copy, rm, new)"),
        };
        Ok(Self { action })
    }
}

async fn one(client: &mut Client, id: i64) -> Result<Note> {
    match client
        .request(Command::ListNotes {
            query: None,
            limit: Some(1),
            id: Some(id),
        })
        .await?
    {
        CommandResult::Notes { mut notes } if !notes.is_empty() => Ok(notes.remove(0)),
        CommandResult::Notes { .. } => bail!("no note number {id}"),
        r => bail!("expected notes, got {}", r.name()),
    }
}

async fn execute(client: &mut Client, args: &Args, limit: Option<u32>, json: bool) -> Result<()> {
    match &args.action {
        Action::List { query } => {
            let result = client
                .request(Command::ListNotes {
                    query: query.clone(),
                    limit,
                    id: None,
                })
                .await?;
            render::result(&result, json);
        }
        Action::Show(id) => {
            let note = one(client, *id).await?;
            if json {
                render::result(&CommandResult::Notes { notes: vec![note] }, true);
            } else {
                println!("{}", safe::block(&note.text));
            }
        }
        Action::Copy(id) => {
            let note = one(client, *id).await?;
            let tool = copy_to_clipboard(&note.text)?;
            if !json {
                eprintln!("copied note {id} ({} words) with {tool}", note.word_count);
            }
        }
        Action::Rm(id) => {
            let result = client.request(Command::DeleteNote { id: *id }).await?;
            render::result(&result, json);
        }
        Action::New => {
            // The same session `dictate start` opens, but its transcript goes
            // to the scratchpad. `dictate stop` (or the hotkey) ends it.
            let result = client
                .request(Command::StartDictation {
                    mode: DictationMode::Toggle,
                    options: Some(SessionOptions {
                        route: Some(Route::Note),
                        ..SessionOptions::default()
                    }),
                })
                .await?;
            render::result(&result, json);
        }
    }
    Ok(())
}

/// Clipboard programs worth trying for this session, best first.
fn clipboard_candidates(wayland: bool, x11: bool) -> Vec<(&'static str, &'static [&'static str])> {
    let mut tools: Vec<(&'static str, &'static [&'static str])> = Vec::new();
    if wayland {
        tools.push(("wl-copy", &[]));
    }
    if x11 {
        tools.push(("xclip", &["-selection", "clipboard", "-in"]));
        tools.push(("xsel", &["--clipboard", "--input"]));
    }
    tools
}

/// Put `text` on the clipboard with the first tool that works; returns its name.
fn copy_to_clipboard(text: &str) -> Result<&'static str> {
    let tools = clipboard_candidates(
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
        std::env::var_os("DISPLAY").is_some(),
    );
    if tools.is_empty() {
        bail!("no display to copy to (neither WAYLAND_DISPLAY nor DISPLAY is set); use `dictate notes show` instead");
    }
    let mut last = None;
    for (program, args) in &tools {
        match pipe_to(program, args, text) {
            Ok(()) => return Ok(program),
            Err(e) => last = Some(format!("{program}: {e}")),
        }
    }
    bail!(
        "could not copy ({}); install wl-clipboard or xclip, or use `dictate notes show`",
        last.unwrap_or_default()
    )
}

/// Run `program args…` with `text` on its stdin. Its output is discarded so a
/// clipboard owner that keeps running (xclip) cannot hold our pipes open.
fn pipe_to(program: &str, args: &[&str], text: &str) -> Result<()> {
    let mut child = Process::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("starting {program}"))?;
    child
        .stdin
        .take()
        .context("no stdin")?
        .write_all(text.as_bytes())?;
    let status = child.wait()?;
    if !status.success() {
        bail!("exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &[&str]) -> Result<Action> {
        Args::parse(&s.iter().map(|s| s.to_string()).collect::<Vec<_>>(), None).map(|a| a.action)
    }

    #[test]
    fn the_default_action_is_list_and_the_text_flag_is_its_filter() {
        assert_eq!(parse(&[]).unwrap(), Action::List { query: None });
        let a = Args::parse(&["list".into()], Some("milk".into())).unwrap();
        assert_eq!(
            a.action,
            Action::List {
                query: Some("milk".into())
            }
        );
    }

    #[test]
    fn search_joins_its_words() {
        assert_eq!(
            parse(&["search", "oat", "milk"]).unwrap(),
            Action::List {
                query: Some("oat milk".into())
            }
        );
    }

    #[test]
    fn id_actions_take_exactly_one_number() {
        assert_eq!(parse(&["show", "7"]).unwrap(), Action::Show(7));
        assert_eq!(parse(&["copy", "7"]).unwrap(), Action::Copy(7));
        assert_eq!(parse(&["rm", "7"]).unwrap(), Action::Rm(7));
        assert_eq!(parse(&["new"]).unwrap(), Action::New);
    }

    #[test]
    fn usage_errors_are_clear() {
        for s in [
            &["show"][..],
            &["show", "x"][..],
            &["copy", "1", "2"][..],
            &["rm"][..],
            &["search"][..],
            &["search", "  "][..],
            &["list", "extra"][..],
            &["new", "extra"][..],
            &["frobnicate"][..],
        ] {
            assert!(parse(s).is_err(), "{s:?}");
        }
    }

    #[test]
    fn clipboard_tools_follow_the_session_type() {
        assert!(clipboard_candidates(false, false).is_empty());
        assert_eq!(clipboard_candidates(true, false)[0].0, "wl-copy");
        let x11: Vec<_> = clipboard_candidates(false, true)
            .iter()
            .map(|t| t.0)
            .collect();
        assert_eq!(x11, ["xclip", "xsel"]);
        // Both: Wayland first (Xwayland clipboards are not shared everywhere).
        assert_eq!(clipboard_candidates(true, true)[0].0, "wl-copy");
    }

    #[test]
    fn text_reaches_the_tool_on_stdin_and_failures_are_reported() {
        let out = std::env::temp_dir().join(format!("dictate-notes-clip-{}", std::process::id()));
        let script = format!("cat > '{}'", out.display());
        pipe_to("sh", &["-c", &script], "line one\nline two ünï").unwrap();
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            "line one\nline two ünï"
        );
        let _ = std::fs::remove_file(out);
        assert!(pipe_to("sh", &["-c", "exit 3"], "x").is_err());
        assert!(pipe_to("definitely-not-a-clipboard-tool", &[], "x").is_err());
    }
}
