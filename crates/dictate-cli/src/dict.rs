//! Dictionary management speaks only the protocol; no store or matcher linkage.
use crate::{client::Client, render};
use anyhow::{bail, Context, Result};
use dictate_proto::{Command, CommandResult, DictionaryEntry, DictionarySuggestion, ErrorCode};
use std::io::{Read, Write};

pub async fn run(client: &mut Client, raw: &[String], json: bool) -> Result<i32> {
    let args = Args::parse(raw)?;
    let result = execute(client, &args, json).await;
    if let Err(e) = &result {
        // Keep an older daemon's refusal actionable instead of implying an
        // empty dictionary. Structured validation errors keep their own text.
        if e.to_string()
            .contains(ErrorCode::UnsupportedCommand.as_str())
        {
            eprintln!("dictate: the personal dictionary is not available in this build — {e}");
            return Ok(1);
        }
    }
    result.map(|_| 0)
}
#[derive(Debug, Default)]
struct Args {
    action: String,
    value: Option<String>,
    sounds_like: Vec<String>,
    apps: Vec<String>,
    case_sensitive: bool,
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
                "--sounds-like" => {
                    a.sounds_like = it
                        .next()
                        .context("--sounds-like needs comma-separated aliases")?
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .collect();
                }
                "--app" => a.apps.push(
                    it.next()
                        .context("--app needs an application identifier")?
                        .clone(),
                ),
                "--case-sensitive" => a.case_sensitive = true,
                s if s.starts_with('-') && s != "-" => bail!("unknown dictionary option '{s}'"),
                _ if a.value.is_none() => a.value = Some(arg.clone()),
                _ => bail!("unexpected dictionary argument '{arg}'"),
            }
        }
        if ![
            "list", "add", "rm", "enable", "disable", "suggest", "accept", "import", "export",
        ]
        .contains(&a.action.as_str())
        {
            bail!("unknown dictionary action '{}'", a.action);
        }
        if ["add", "rm", "enable", "disable", "accept"].contains(&a.action.as_str())
            && a.value.is_none()
        {
            bail!("dict {} needs a phrase or entry id", a.action);
        }
        if a.action != "add"
            && (!a.sounds_like.is_empty() || !a.apps.is_empty() || a.case_sensitive)
        {
            bail!("--sounds-like, --app and --case-sensitive apply to dict add");
        }
        if ["list", "suggest"].contains(&a.action.as_str()) && a.value.is_some() {
            bail!("dict {} takes no positional value", a.action);
        }
        Ok(a)
    }
}
async fn entries(client: &mut Client) -> Result<Vec<DictionaryEntry>> {
    match client
        .request(Command::ListDictionary {
            query: None,
            limit: None,
        })
        .await?
    {
        CommandResult::Dictionary { entries } => Ok(entries),
        r => bail!("expected dictionary, got {}", r.name()),
    }
}
async fn suggestions(client: &mut Client) -> Result<Vec<DictionarySuggestion>> {
    match client
        .request(Command::ListDictionarySuggestions { limit: None })
        .await?
    {
        CommandResult::DictionarySuggestions { suggestions } => Ok(suggestions),
        r => bail!("expected dictionary_suggestions, got {}", r.name()),
    }
}
async fn execute(client: &mut Client, a: &Args, json: bool) -> Result<()> {
    let result = match a.action.as_str() {
        "list" => {
            client
                .request(Command::ListDictionary {
                    query: None,
                    limit: None,
                })
                .await?
        }
        "suggest" => {
            client
                .request(Command::ListDictionarySuggestions { limit: None })
                .await?
        }
        "add" => {
            let mut entry = DictionaryEntry::new(a.value.as_ref().unwrap());
            entry.sounds_like = a.sounds_like.clone();
            entry.apps = a.apps.clone();
            entry.case_sensitive = a.case_sensitive;
            client
                .request(Command::UpsertDictionaryEntry { entry })
                .await?
        }
        "rm" | "enable" | "disable" => {
            let value = a.value.as_deref().unwrap();
            let entry = entries(client)
                .await?
                .into_iter()
                .find(|e| {
                    e.phrase.to_lowercase() == value.to_lowercase()
                        || e.id.is_some_and(|id| id.to_string() == value)
                })
                .context("dictionary entry not found")?;
            if a.action == "rm" {
                client
                    .request(Command::DeleteDictionaryEntry {
                        id: entry.id.context("daemon returned an entry without an id")?,
                    })
                    .await?
            } else {
                let mut entry = entry;
                entry.enabled = a.action == "enable";
                client
                    .request(Command::UpsertDictionaryEntry { entry })
                    .await?
            }
        }
        "accept" => {
            let phrase = a.value.as_ref().unwrap();
            let proposal = suggestions(client)
                .await?
                .into_iter()
                .find(|s| s.entry.phrase.to_lowercase() == phrase.to_lowercase())
                .context("dictionary suggestion not found")?;
            client
                .request(Command::UpsertDictionaryEntry {
                    entry: proposal.entry,
                })
                .await?
        }
        "import" => {
            let mut input = String::new();
            match a.value.as_deref() {
                Some(path) if path != "-" => {
                    input =
                        std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?
                }
                _ => {
                    std::io::stdin().read_to_string(&mut input)?;
                }
            }
            // Parse the entire file before sending any mutation.
            let imported: Vec<DictionaryEntry> = input
                .lines()
                .enumerate()
                .filter(|(_, s)| !s.trim().is_empty())
                .map(|(n, s)| {
                    serde_json::from_str(s)
                        .with_context(|| format!("invalid dictionary JSON on line {}", n + 1))
                })
                .collect::<Result<_>>()?;
            let mut current = entries(client).await?;
            let mut count = 0;
            for mut entry in imported {
                entry.id = current
                    .iter()
                    .find(|e| e.phrase.to_lowercase() == entry.phrase.to_lowercase())
                    .and_then(|e| e.id);
                entry.hit_count = None;
                match client
                    .request(Command::UpsertDictionaryEntry { entry })
                    .await?
                {
                    CommandResult::DictionaryEntry { entry } => {
                        current.retain(|e| e.id != entry.id);
                        current.push(entry);
                        count += 1;
                    }
                    r => bail!("expected dictionary_entry, got {}", r.name()),
                }
            }
            eprintln!("imported {count} dictionary entries");
            return Ok(());
        }
        "export" => {
            let entries = entries(client).await?;
            let mut output: Box<dyn Write> = match a.value.as_deref() {
                Some(path) if path != "-" => Box::new(
                    std::fs::File::create(path).with_context(|| format!("writing {path}"))?,
                ),
                _ => Box::new(std::io::stdout().lock()),
            };
            for entry in entries {
                writeln!(output, "{}", serde_json::to_string(&entry)?)?;
            }
            output.flush()?;
            return Ok(());
        }
        _ => unreachable!("validated action"),
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
    fn add_supports_aliases_scope_and_case() {
        let a = parse(&[
            "add",
            "Kubernetes",
            "--sounds-like",
            "cube ernetties, kubernetties",
            "--app",
            "slack",
            "--case-sensitive",
        ])
        .unwrap();
        assert_eq!(a.sounds_like, ["cube ernetties", "kubernetties"]);
        assert_eq!(a.apps, ["slack"]);
        assert!(a.case_sensitive);
    }
    #[test]
    fn dictionary_usage_errors_are_clear() {
        for s in [
            &["add"][..],
            &["rm"][..],
            &["suggest", "extra"][..],
            &["add", "Term", "--app"][..],
            &["list", "--case-sensitive"][..],
            &["unknown"][..],
        ] {
            assert!(parse(s).is_err(), "{s:?}");
        }
        assert_eq!(parse(&[]).unwrap().action, "list");
        assert_eq!(parse(&["import", "-"]).unwrap().value.as_deref(), Some("-"));
    }
}
