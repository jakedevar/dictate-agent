//! Snippet management (S24). Speaks only the protocol, like `dict`.
use crate::{client::Client, render};
use anyhow::{bail, Context, Result};
use dictate_proto::{Command, CommandResult, ErrorCode, Snippet};
use std::io::Read;

pub async fn run(client: &mut Client, raw: &[String], json: bool) -> Result<i32> {
    let args = Args::parse(raw)?;
    let result = execute(client, &args, json).await;
    if let Err(e) = &result {
        if e.to_string()
            .contains(ErrorCode::UnsupportedCommand.as_str())
        {
            eprintln!("dictate: snippets are not available in this build — {e}");
            return Ok(1);
        }
    }
    result.map(|_| 0)
}

#[derive(Debug, Default)]
struct Args {
    action: String,
    /// The trigger phrase (add) or a trigger/id (rm, enable, disable).
    target: Option<String>,
    expansion: Option<String>,
    file: Option<String>,
    apps: Vec<String>,
    category: Option<String>,
}

impl Args {
    fn parse(raw: &[String]) -> Result<Self> {
        let mut a = Self {
            action: raw.first().cloned().unwrap_or_else(|| "list".into()),
            ..Default::default()
        };
        let mut it = raw.iter().skip(1);
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--app" => a.apps.push(
                    it.next()
                        .context("--app needs an application identifier")?
                        .clone(),
                ),
                "--category" => {
                    a.category = Some(it.next().context("--category needs a label")?.clone())
                }
                "--file" => a.file = Some(it.next().context("--file needs a path")?.clone()),
                s if s.starts_with('-') && s != "-" => bail!("unknown snippet option '{s}'"),
                _ if a.target.is_none() => a.target = Some(arg.clone()),
                _ if a.expansion.is_none() => a.expansion = Some(arg.clone()),
                _ => bail!("unexpected snippet argument '{arg}'"),
            }
        }
        if !["list", "add", "rm", "enable", "disable"].contains(&a.action.as_str()) {
            bail!("unknown snippet action '{}'", a.action);
        }
        if a.action != "list" && a.target.is_none() {
            bail!("snippet {} needs a trigger phrase or id", a.action);
        }
        if a.action == "list" && a.target.is_some() {
            bail!("snippet list takes no positional value");
        }
        if a.action != "add"
            && (a.expansion.is_some()
                || a.file.is_some()
                || !a.apps.is_empty()
                || a.category.is_some())
        {
            bail!("an expansion, --file, --app and --category apply to snippet add");
        }
        if a.action == "add" && a.expansion.is_some() == a.file.is_some() {
            bail!("snippet add needs exactly one of an expansion argument (`-` reads stdin) or --file");
        }
        Ok(a)
    }
}

async fn all(client: &mut Client) -> Result<Vec<Snippet>> {
    match client
        .request(Command::ListSnippets {
            query: None,
            limit: None,
        })
        .await?
    {
        CommandResult::Snippets { snippets } => Ok(snippets),
        r => bail!("expected snippets, got {}", r.name()),
    }
}

async fn execute(client: &mut Client, a: &Args, json: bool) -> Result<()> {
    let result = match a.action.as_str() {
        "list" => {
            client
                .request(Command::ListSnippets {
                    query: None,
                    limit: None,
                })
                .await?
        }
        "add" => {
            let expansion = match (&a.file, a.expansion.as_deref()) {
                (Some(path), _) => {
                    std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?
                }
                (None, Some("-")) => {
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s)?;
                    s
                }
                (None, Some(text)) => text.to_string(),
                (None, None) => unreachable!("parse requires one"),
            };
            let mut snippet = Snippet::new(a.target.as_ref().unwrap(), expansion);
            snippet.apps = a.apps.clone();
            snippet.category = a.category.clone();
            client.request(Command::UpsertSnippet { snippet }).await?
        }
        "rm" | "enable" | "disable" => {
            let value = a.target.as_deref().unwrap();
            let snippet = all(client)
                .await?
                .into_iter()
                .find(|s| {
                    s.trigger.to_lowercase() == value.to_lowercase()
                        || s.id.is_some_and(|id| id.to_string() == value)
                })
                .context("snippet not found")?;
            let id = snippet
                .id
                .context("daemon returned a snippet without an id")?;
            if a.action == "rm" {
                client.request(Command::DeleteSnippet { id }).await?
            } else {
                let mut snippet = snippet;
                snippet.enabled = a.action == "enable";
                client.request(Command::UpsertSnippet { snippet }).await?
            }
        }
        other => bail!("unknown snippet action '{other}'"),
    };
    render::result(&result, json);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &[&str]) -> Result<Args> {
        Args::parse(&s.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn add_parses_trigger_expansion_scope_and_category() {
        let a = parse(&[
            "add",
            "work email",
            "me@example.com",
            "--app",
            "slack",
            "--category",
            "contact",
        ])
        .unwrap();
        assert_eq!(a.target.as_deref(), Some("work email"));
        assert_eq!(a.expansion.as_deref(), Some("me@example.com"));
        assert_eq!(a.apps, ["slack"]);
        assert_eq!(a.category.as_deref(), Some("contact"));
        assert!(parse(&["add", "sig", "--file", "/tmp/x"]).is_ok());
        assert_eq!(parse(&[]).unwrap().action, "list");
    }

    #[test]
    fn usage_errors_are_clear() {
        for s in [
            &["add"][..],
            &["add", "sig"][..],
            &["add", "sig", "x", "--file", "f"][..],
            &["rm"][..],
            &["rm", "sig", "extra"][..],
            &["list", "x"][..],
            &["list", "--app", "a"][..],
            &["add", "sig", "x", "--bogus"][..],
            &["add", "sig", "x", "--app"][..],
            &["unknown"][..],
        ] {
            assert!(parse(s).is_err(), "{s:?}");
        }
    }
}
