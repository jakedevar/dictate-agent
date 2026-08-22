use anyhow::Result;
use std::time::{Duration, Instant};
use tracing::{info, warn};

use crate::text_cleanup::scrub_returned_text;

/// Grammar correction prompt — port of grammar.py:13-18.
const GRAMMAR_PROMPT: &str =
    "Fix only grammar, spelling, and punctuation errors in the following text. \
Do not change meaning, add words, remove words, or rephrase. \
Output ONLY the corrected text with no explanation.\n\nText: {text}";

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct GrammarResult {
    pub success: bool,
    pub corrected: String,
    pub original: String,
    pub duration_s: f64,
    pub error: Option<String>,
}

pub struct GrammarCorrector {
    enabled: bool,
    host: String,
    port: u16,
    model: String,
    timeout: Duration,
    min_words: usize,
}

impl GrammarCorrector {
    pub fn new(config: &crate::config::GrammarConfig) -> Self {
        let (host, port) = parse_host_port(&config.host);
        Self {
            enabled: config.enabled,
            host,
            port,
            model: config.model.clone(),
            timeout: Duration::from_secs_f64(config.timeout_s),
            min_words: config.min_words,
        }
    }

    /// Correct grammar. NEVER returns an error — fail-open.
    /// On any failure, returns the original text in `corrected`.
    /// Port of grammar.py:49-105.
    pub async fn correct(&self, text: &str) -> GrammarResult {
        let original = text.to_string();
        let start = Instant::now();

        // Fast paths: disabled or too short — matches grammar.py:55-60
        if !self.enabled {
            return GrammarResult::pass_through(original, start);
        }
        if text.trim().is_empty() || text.split_whitespace().count() < self.min_words {
            return GrammarResult::pass_through(original, start);
        }

        // Call Ollama
        match self.call_ollama(text).await {
            Ok(corrected) => {
                // Length ratio guard — matches grammar.py:85-92
                let ratio = corrected.len() as f64 / text.len() as f64;
                if !(0.5..=1.5).contains(&ratio) {
                    warn!("Grammar correction rejected: length ratio {:.2}", ratio);
                    GrammarResult::fail(
                        original,
                        start,
                        &format!("Length ratio {:.2} outside 0.5-1.5 range", ratio),
                    )
                } else {
                    info!("Grammar corrected in {:.3}s", start.elapsed().as_secs_f64());
                    GrammarResult {
                        success: true,
                        corrected,
                        original,
                        duration_s: start.elapsed().as_secs_f64(),
                        error: None,
                    }
                }
            }
            Err(e) => {
                warn!("Grammar correction failed (fail-open): {}", e);
                GrammarResult::fail(original, start, &e.to_string())
            }
        }
    }

    async fn call_ollama(&self, text: &str) -> Result<String> {
        use ollama_rs::generation::completion::request::GenerationRequest;
        use ollama_rs::generation::parameters::ThinkType;
        use ollama_rs::models::ModelOptions;
        use ollama_rs::Ollama;

        let ollama = Ollama::builder().host(&self.host).port(self.port).build();
        let prompt = GRAMMAR_PROMPT.replace("{text}", text);

        let request = GenerationRequest::new(self.model.clone(), prompt)
            .options(ModelOptions::default().num_predict(256).temperature(0.1))
            .think(ThinkType::False); // Disable chain-of-thought — matches grammar.py:72

        let response = tokio::time::timeout(self.timeout, ollama.generate(request)).await??;

        let text = response.response.trim().to_string();
        if text.is_empty() {
            anyhow::bail!("Empty response from grammar model");
        }

        // Strip any <think>...</think> wrapper if present (fallback safety)
        let text = strip_think_tags(&text);

        Ok(scrub_returned_text(&text))
    }
}

impl GrammarResult {
    fn pass_through(text: String, start: Instant) -> Self {
        Self {
            success: true,
            corrected: text.clone(),
            original: text,
            duration_s: start.elapsed().as_secs_f64(),
            error: None,
        }
    }
    fn fail(original: String, start: Instant, error: &str) -> Self {
        Self {
            success: false,
            corrected: original.clone(),
            original,
            duration_s: start.elapsed().as_secs_f64(),
            error: Some(error.into()),
        }
    }
}

/// Strip <think>...</think> tags from Qwen3 output.
/// Replaces the Python `think=False` parameter as a fallback safety net.
fn strip_think_tags(text: &str) -> String {
    if let Some(rest) = text.strip_prefix("<think>") {
        if let Some(pos) = rest.find("</think>") {
            return rest[pos + 8..].trim().to_string();
        }
    }
    text.to_string()
}

/// Parse "http://localhost:11434" into ("http://localhost", 11434).
/// ollama-rs wants host and port separately.
pub fn parse_host_port(url: &str) -> (String, u16) {
    // Try to parse as URL
    if let Some(last_colon) = url.rfind(':') {
        let potential_port = &url[last_colon + 1..];
        // Check if what's after the last colon is a port number (not part of "http://")
        if let Ok(port) = potential_port.parse::<u16>() {
            let host = &url[..last_colon];
            // Make sure we're not splitting "http:" or "https:"
            if !host.is_empty() && !host.ends_with('/') {
                return (host.to_string(), port);
            }
        }
    }
    // Default: assume localhost:11434
    (url.to_string(), 11434)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_think_tags_with_think() {
        assert_eq!(
            strip_think_tags("<think>reasoning here</think>The corrected text"),
            "The corrected text"
        );
    }

    #[test]
    fn test_strip_think_tags_no_think() {
        assert_eq!(strip_think_tags("Just plain text"), "Just plain text");
    }

    #[test]
    fn test_strip_think_tags_empty() {
        assert_eq!(strip_think_tags(""), "");
    }

    #[test]
    fn test_strip_think_tags_unclosed() {
        assert_eq!(
            strip_think_tags("<think>no closing tag"),
            "<think>no closing tag"
        );
    }

    #[test]
    fn test_parse_host_port() {
        let (host, port) = parse_host_port("http://localhost:11434");
        assert_eq!(host, "http://localhost");
        assert_eq!(port, 11434);
    }

    #[test]
    fn test_parse_host_port_different_port() {
        let (host, port) = parse_host_port("http://192.168.1.100:8080");
        assert_eq!(host, "http://192.168.1.100");
        assert_eq!(port, 8080);
    }

    #[test]
    fn test_parse_host_port_no_port() {
        let (host, port) = parse_host_port("http://localhost");
        assert_eq!(host, "http://localhost");
        assert_eq!(port, 11434);
    }

    #[tokio::test]
    async fn test_grammar_disabled() {
        let config = crate::config::GrammarConfig {
            enabled: false,
            ..Default::default()
        };
        let corrector = GrammarCorrector::new(&config);
        let result = corrector.correct("test text here").await;
        assert!(result.success);
        assert_eq!(result.corrected, "test text here");
    }

    #[tokio::test]
    async fn test_grammar_short_text_bypass() {
        let config = crate::config::GrammarConfig {
            enabled: true,
            min_words: 3,
            ..Default::default()
        };
        let corrector = GrammarCorrector::new(&config);
        // "timer" is 1 word, below min_words=3
        let result = corrector.correct("timer").await;
        assert!(result.success);
        assert_eq!(result.corrected, "timer");
    }
}
