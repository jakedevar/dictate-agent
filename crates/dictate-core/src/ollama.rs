//! A read-only look at an Ollama server: is it up, and which models does it
//! have?
//!
//! Shared by the startup health probe and `dictate doctor`, because both ask
//! the same question and must give the same answer. It never generates
//! anything, so it costs a single `/api/tags` round trip and never loads a
//! model into VRAM.

use std::time::Duration;

/// What a probe found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OllamaProbe {
    /// Whether the server answered `/api/tags`.
    pub reachable: bool,
    /// Installed model names, e.g. `qwen3.6:27b`. Empty when unreachable.
    pub models: Vec<String>,
    /// Why the probe failed, when it did.
    pub error: Option<String>,
}

impl OllamaProbe {
    /// Whether `wanted` is installed. See [`model_installed`].
    #[must_use]
    pub fn has_model(&self, wanted: &str) -> bool {
        model_installed(&self.models, wanted)
    }
}

/// A client for the Ollama server at `host`, a URL such as
/// `http://localhost:11434` (no port means Ollama's default, 11434).
///
/// `ollama-rs`'s builder panics on a string it cannot parse, so every place
/// that talks to Ollama builds its client here instead; config loading calls
/// it too, so a malformed host is a load error rather than a crash later.
///
/// # Errors
///
/// When `host` is not an absolute `http://` or `https://` URL with a host.
pub fn client(host: &str) -> Result<ollama_rs::Ollama, String> {
    let (hostname, port) = dictate_fmt::grammar::parse_host_port(host.trim());
    client_at(&hostname, port).map_err(|why| format!("'{host}' {why}"))
}

/// [`client`] for a host already split from its port.
///
/// # Errors
///
/// As [`client`]; the message omits the offending value.
pub fn client_at(hostname: &str, port: u16) -> Result<ollama_rs::Ollama, String> {
    use ollama_rs::IntoUrlSealed;
    let mut url = hostname
        .into_url()
        .map_err(|e| format!("is not a URL ({e}); expected e.g. http://localhost:11434"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!(
            "must be an http:// or https:// URL, not {}://",
            url.scheme()
        ));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err("has no host name".into());
    }
    url.set_port(Some(port))
        .map_err(|()| "cannot take a port".to_string())?;
    Ok(ollama_rs::Ollama::from_url(url))
}

/// Query `host` (a URL such as `http://localhost:11434`) with a hard timeout.
///
/// Never panics: a host that is not a usable URL is a failed probe that says
/// why.
pub async fn probe(host: &str, timeout: Duration) -> OllamaProbe {
    let client = match client(host) {
        Ok(client) => client,
        Err(why) => {
            return OllamaProbe {
                reachable: false,
                models: Vec::new(),
                error: Some(format!("Ollama host {why}")),
            }
        }
    };
    match tokio::time::timeout(timeout, client.list_local_models()).await {
        Ok(Ok(models)) => OllamaProbe {
            reachable: true,
            models: models.into_iter().map(|m| m.name).collect(),
            error: None,
        },
        Ok(Err(e)) => OllamaProbe {
            reachable: false,
            models: Vec::new(),
            error: Some(e.to_string()),
        },
        Err(_) => OllamaProbe {
            reachable: false,
            models: Vec::new(),
            error: Some(format!("no answer within {:.0}s", timeout.as_secs_f64())),
        },
    }
}

/// Whether `wanted` names an installed model.
///
/// Ollama resolves a bare name to its `:latest` tag, so `gemma4` is satisfied by
/// `gemma4:latest` — but `gemma4` is *not* satisfied by `gemma4:12b`, which is a
/// different model and must not be silently substituted.
#[must_use]
pub fn model_installed(installed: &[String], wanted: &str) -> bool {
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return false;
    }
    let with_tag = if wanted.contains(':') {
        wanted.to_string()
    } else {
        format!("{wanted}:latest")
    };
    installed.iter().any(|m| m == wanted || *m == with_tag)
}

/// Whether an Ollama error message says the model does not exist.
#[must_use]
pub fn is_model_missing_error(message: &str) -> bool {
    let m = message.to_lowercase();
    m.contains("not found") && m.contains("model")
}

/// Whether an Ollama error message says the server could not be reached.
#[must_use]
pub fn is_unreachable_error(message: &str) -> bool {
    let m = message.to_lowercase();
    m.contains("connection refused")
        || m.contains("connection reset")
        || m.contains("error sending request")
        || m.contains("timed out")
        || m.contains("dns error")
        || m.contains("connect error")
}

/// A short, human list of installed alternatives for a "model missing" message.
#[must_use]
pub fn describe_installed(models: &[String]) -> String {
    if models.is_empty() {
        "none installed".to_string()
    } else {
        models.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed() -> Vec<String> {
        ["qwen3.6:27b", "gemma4:12b", "llama3:latest"]
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }

    #[test]
    fn a_tagged_model_must_match_exactly() {
        assert!(model_installed(&installed(), "gemma4:12b"));
        assert!(!model_installed(&installed(), "gemma4:e4b"));
        assert!(
            !model_installed(&installed(), "qwen3:14b"),
            "the model the June config names is not installed"
        );
    }

    #[test]
    fn a_bare_name_means_latest_and_never_another_tag() {
        assert!(model_installed(&installed(), "llama3"));
        assert!(
            !model_installed(&installed(), "gemma4"),
            "gemma4:12b is a different model from gemma4:latest"
        );
    }

    #[test]
    fn an_empty_name_is_never_installed() {
        assert!(!model_installed(&installed(), ""));
        assert!(!model_installed(&installed(), "  "));
    }

    #[test]
    fn ollamas_missing_model_error_is_recognised() {
        assert!(is_model_missing_error("model 'qwen3:14b' not found"));
        assert!(is_model_missing_error(
            "Ollama error: model \"qwen3:14b\" not found, try pulling it first"
        ));
        assert!(!is_model_missing_error("connection refused"));
        assert!(!is_model_missing_error("file not found"));
    }

    #[test]
    fn a_dead_server_is_recognised_and_is_not_a_missing_model() {
        assert!(is_unreachable_error(
            "error sending request for url (http://localhost:11434/api/generate)"
        ));
        assert!(is_unreachable_error("Connection refused (os error 111)"));
        assert!(!is_unreachable_error("model 'x' not found"));
    }

    #[test]
    fn installed_alternatives_are_listed_for_the_fix_message() {
        assert_eq!(
            describe_installed(&installed()),
            "qwen3.6:27b, gemma4:12b, llama3:latest"
        );
        assert_eq!(describe_installed(&[]), "none installed");
    }

    /// `ollama_probe_panics`: these hosts made `ollama-rs`'s builder panic.
    const MALFORMED_HOSTS: &[&str] = &[
        "localhost:11434",
        "localhost",
        "",
        "http://",
        "not a url",
        "ftp://example.com",
        "file:///tmp/ollama",
        "mailto:someone@example.com",
    ];

    #[tokio::test]
    async fn a_malformed_host_is_a_clean_probe_failure_not_a_panic() {
        for host in MALFORMED_HOSTS {
            let p = probe(host, Duration::from_millis(200)).await;
            assert!(!p.reachable, "{host:?}");
            assert!(
                p.error
                    .as_deref()
                    .is_some_and(|e| e.contains("Ollama host")),
                "{host:?}: {p:?}"
            );
        }
    }

    #[test]
    fn client_accepts_http_urls_with_or_without_a_port() {
        for host in [
            "http://localhost:11434",
            "http://localhost",
            "https://ollama.example.com",
            "http://127.0.0.1:8080/",
            " http://localhost:11434 ",
        ] {
            assert!(client(host).is_ok(), "{host:?}: {:?}", client(host).err());
        }
        assert_eq!(
            client("http://localhost").unwrap().url().port(),
            Some(11434),
            "no port means Ollama's default"
        );
        for host in MALFORMED_HOSTS {
            assert!(client(host).is_err(), "{host:?}");
        }
    }

    #[tokio::test]
    async fn an_unreachable_server_reports_why_instead_of_hanging() {
        // Port 1 on loopback is never an Ollama server.
        let p = probe("http://127.0.0.1:1", Duration::from_secs(2)).await;
        assert!(!p.reachable);
        assert!(p.models.is_empty());
        assert!(p.error.is_some());
    }
}
