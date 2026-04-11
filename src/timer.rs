use regex::Regex;
use std::collections::HashMap;
use std::process::Command;
use std::sync::LazyLock;
use tracing::{error, info};

// --- Word-to-number map ---
// Port of timer_executor.py:15-23 — complete 28-entry map

static WORD_TO_NUM: LazyLock<HashMap<&'static str, u64>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    m.insert("zero", 0);
    m.insert("one", 1);
    m.insert("two", 2);
    m.insert("three", 3);
    m.insert("four", 4);
    m.insert("five", 5);
    m.insert("six", 6);
    m.insert("seven", 7);
    m.insert("eight", 8);
    m.insert("nine", 9);
    m.insert("ten", 10);
    m.insert("eleven", 11);
    m.insert("twelve", 12);
    m.insert("thirteen", 13);
    m.insert("fourteen", 14);
    m.insert("fifteen", 15);
    m.insert("sixteen", 16);
    m.insert("seventeen", 17);
    m.insert("eighteen", 18);
    m.insert("nineteen", 19);
    m.insert("twenty", 20);
    m.insert("thirty", 30);
    m.insert("forty", 40);
    m.insert("fifty", 50);
    m.insert("sixty", 60);
    m.insert("ninety", 90);
    m.insert("a", 1);
    m.insert("an", 1);
    m
});

// --- Unit aliases ---
// Port of timer_executor.py:25-29

#[derive(Debug, Clone, Copy, PartialEq)]
enum TimeUnit {
    Seconds,
    Minutes,
    Hours,
}

impl TimeUnit {
    fn to_seconds(self, count: u64) -> u64 {
        match self {
            TimeUnit::Seconds => count,
            TimeUnit::Minutes => count * 60,
            TimeUnit::Hours => count * 3600,
        }
    }

    fn half_seconds(self) -> u64 {
        match self {
            TimeUnit::Seconds => 30,  // half a second doesn't make sense, but 30 for consistency
            TimeUnit::Minutes => 30,
            TimeUnit::Hours => 1800,
        }
    }
}

static UNIT_ALIASES: LazyLock<HashMap<&'static str, TimeUnit>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    // Seconds
    m.insert("s", TimeUnit::Seconds);
    m.insert("sec", TimeUnit::Seconds);
    m.insert("secs", TimeUnit::Seconds);
    m.insert("second", TimeUnit::Seconds);
    m.insert("seconds", TimeUnit::Seconds);
    // Minutes
    m.insert("m", TimeUnit::Minutes);
    m.insert("min", TimeUnit::Minutes);
    m.insert("mins", TimeUnit::Minutes);
    m.insert("minute", TimeUnit::Minutes);
    m.insert("minutes", TimeUnit::Minutes);
    // Hours
    m.insert("h", TimeUnit::Hours);
    m.insert("hr", TimeUnit::Hours);
    m.insert("hrs", TimeUnit::Hours);
    m.insert("hour", TimeUnit::Hours);
    m.insert("hours", TimeUnit::Hours);
    m
});

/// Build the regex pattern for matching duration expressions.
/// Port of timer_executor.py:79-90.
///
/// Pattern: (\d+|word_alts)(?:\s+and\s+a\s+half)?\s+(unit_alts)(?=\s|$)
/// Word and unit alternatives are sorted longest-first to prevent shorter matches shadowing.
static DURATION_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    let mut word_keys: Vec<&str> = WORD_TO_NUM.keys().copied().collect();
    word_keys.sort_by_key(|b| std::cmp::Reverse(b.len()));
    let word_alts = word_keys.join("|");

    let mut unit_keys: Vec<&str> = UNIT_ALIASES.keys().copied().collect();
    unit_keys.sort_by_key(|b| std::cmp::Reverse(b.len()));
    let unit_alts = unit_keys.join("|");

    let pattern = format!(
        r"(?i)(\d+|{})(?:\s+and\s+a\s+half)?\s+({})\b",
        word_alts, unit_alts
    );

    Regex::new(&pattern).expect("Failed to compile duration regex")
});

/// Regex for "half [an] hour" prefix
static HALF_HOUR_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^half\s+(?:an?\s+)?hour").expect("Failed to compile half-hour regex")
});

/// Parse a word as a number — either a digit string or a word from WORD_TO_NUM.
/// Port of timer_executor.py:41-46.
fn parse_word_number(word: &str) -> Option<u64> {
    let word = word.to_lowercase();
    let word = word.trim();
    if let Ok(n) = word.parse::<u64>() {
        return Some(n);
    }
    WORD_TO_NUM.get(word).copied()
}

/// Parse a duration expression from text.
/// Returns (total_seconds, remaining_text). None seconds = parse failure.
///
/// Port of timer_executor.py:49-138.
///
/// Steps:
/// 1. Check "half hour" / "half an hour" prefix
/// 2. Greedy match loop at current position with DURATION_PATTERN
/// 3. Fallback: search anywhere in original string
/// 4. Return (None, original) if nothing found
pub fn parse_duration(text: &str) -> (Option<u64>, String) {
    let original = text.to_string();
    let mut text = text.trim().to_string();
    let mut total_seconds: u64 = 0;
    let mut found_any = false;

    // Step 1: Check for "half [an] hour" prefix
    if let Some(m) = HALF_HOUR_PATTERN.find(&text) {
        total_seconds += 1800;
        found_any = true;
        text = text[m.end()..].trim().to_string();
    }

    // Step 2: Greedy match loop at current position
    loop {
        let trimmed = text.trim_start();
        // Try to strip leading "and" connector
        let search_text = if trimmed.to_lowercase().starts_with("and ") {
            trimmed[4..].trim_start().to_string()
        } else {
            trimmed.to_string()
        };

        if let Some(m) = DURATION_PATTERN.find(&search_text) {
            if m.start() != 0 {
                // Pattern didn't match at the start — stop greedy matching
                break;
            }

            let caps = DURATION_PATTERN.captures(&search_text).unwrap();
            let num_str = &caps[1];
            let unit_str = caps[2].to_lowercase();

            if let (Some(num), Some(unit)) =
                (parse_word_number(num_str), UNIT_ALIASES.get(unit_str.as_str()))
            {
                total_seconds += unit.to_seconds(num);

                // Check for "and a half"
                let match_text = m.as_str().to_lowercase();
                if match_text.contains("and a half") {
                    total_seconds += unit.half_seconds();
                }

                found_any = true;
                // Advance past the match
                text = search_text[m.end()..].trim().to_string();
            } else {
                break;
            }
        } else {
            break;
        }
    }

    // Step 3: Fallback — search anywhere in original string
    if !found_any {
        if let Some(m) = DURATION_PATTERN.find(&original) {
            let caps = DURATION_PATTERN.captures(&original).unwrap();
            let num_str = &caps[1];
            let unit_str = caps[2].to_lowercase();

            if let (Some(num), Some(unit)) =
                (parse_word_number(num_str), UNIT_ALIASES.get(unit_str.as_str()))
            {
                total_seconds += unit.to_seconds(num);

                let match_text = m.as_str().to_lowercase();
                if match_text.contains("and a half") {
                    total_seconds += unit.half_seconds();
                }

                found_any = true;
                text = original[m.end()..].trim().to_string();
            }
        }
    }

    if found_any && total_seconds > 0 {
        (Some(total_seconds), text)
    } else {
        (None, original)
    }
}

/// Formats seconds as systemd OnActiveSec duration (e.g., "1h30m")
pub fn format_systemd_duration(seconds: u64) -> String {
    let h = seconds / 3600;
    let m = (seconds % 3600) / 60;
    let s = seconds % 60;
    let mut result = String::new();
    if h > 0 {
        result.push_str(&format!("{}h", h));
    }
    if m > 0 {
        result.push_str(&format!("{}m", m));
    }
    if s > 0 || result.is_empty() {
        result.push_str(&format!("{}s", s));
    }
    result
}

/// Formats seconds as human-readable string (e.g., "1 hour 30 minutes")
pub fn format_human_duration(seconds: u64) -> String {
    let h = seconds / 3600;
    let m = (seconds % 3600) / 60;
    let s = seconds % 60;

    let mut parts = Vec::new();
    if h > 0 {
        parts.push(if h == 1 {
            "1 hour".to_string()
        } else {
            format!("{} hours", h)
        });
    }
    if m > 0 {
        parts.push(if m == 1 {
            "1 minute".to_string()
        } else {
            format!("{} minutes", m)
        });
    }
    if s > 0 || parts.is_empty() {
        parts.push(if s == 1 {
            "1 second".to_string()
        } else {
            format!("{} seconds", s)
        });
    }

    parts.join(" ")
}

// --- Timer Executor ---
// Port of timer_executor.py:177-272

#[derive(Debug, Clone)]
pub struct TimerResult {
    pub success: bool,
    pub response: String,
    pub error: Option<String>,
}

pub struct TimerExecutor {
    sound_enabled: bool,
    sound_file: String,
}

impl TimerExecutor {
    pub fn new(config: &crate::config::TimerConfig) -> Self {
        let sound_file = crate::config::expand_tilde(&config.sound_file)
            .to_string_lossy()
            .into_owned();
        Self {
            sound_enabled: config.sound_enabled,
            sound_file,
        }
    }

    /// Execute a timer. NEVER raises.
    /// Port of timer_executor.py:177-272
    pub fn execute(&self, text: &str) -> TimerResult {
        let (seconds, remaining) = parse_duration(text);

        let seconds = match seconds {
            Some(s) if s > 0 => s,
            _ => {
                return TimerResult {
                    success: false,
                    response: String::new(),
                    error: Some(format!("Could not parse duration from: {}", text)),
                }
            }
        };

        let label = if remaining.trim().is_empty() {
            "Timer complete".to_string()
        } else {
            remaining.trim().to_string()
        };

        let human_dur = format_human_duration(seconds);
        let systemd_dur = format_systemd_duration(seconds);

        // Build notify command (bash one-liner)
        let notify_cmd = if self.sound_enabled {
            format!(
                r#"SOUND_FILE="{}"; ( while true; do play -q "$SOUND_FILE" 2>/dev/null; sleep 1; done ) & SOUND_PID=$!; dunstify -a "Dictate Agent" -i alarm-symbolic -u critical -t 0 "Timer: {}" "{} elapsed" --action="default,Dismiss"; kill $SOUND_PID 2>/dev/null; wait $SOUND_PID 2>/dev/null"#,
                self.sound_file, label, human_dur
            )
        } else {
            format!(
                r#"dunstify -a "Dictate Agent" -i alarm-symbolic -u critical -t 0 "Timer: {}" "{} elapsed" --action="default,Dismiss""#,
                label, human_dur
            )
        };

        // Run systemd-run
        match Command::new("systemd-run")
            .args([
                "--user",
                &format!("--on-active={}", systemd_dur),
                "--description=Dictate Agent Timer",
                "/bin/bash",
                "-c",
                &notify_cmd,
            ])
            .output()
        {
            Ok(output) if output.status.success() => {
                info!("Timer set: {} ({})", human_dur, label);
                TimerResult {
                    success: true,
                    response: format!("Timer set for {}: {}", human_dur, label),
                    error: None,
                }
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                error!("systemd-run failed: {}", stderr);
                TimerResult {
                    success: false,
                    response: String::new(),
                    error: Some(stderr.to_string()),
                }
            }
            Err(e) => {
                error!("Failed to run systemd-run: {}", e);
                TimerResult {
                    success: false,
                    response: String::new(),
                    error: Some(e.to_string()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_duration tests ---

    #[test]
    fn test_numeric_minutes() {
        let (secs, remaining) = parse_duration("5 minutes");
        assert_eq!(secs, Some(300));
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_numeric_short_unit() {
        let (secs, _) = parse_duration("5 m");
        assert_eq!(secs, Some(300));
    }

    #[test]
    fn test_compound_duration() {
        let (secs, remaining) = parse_duration("1 hour 30 minutes");
        assert_eq!(secs, Some(5400));
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_half_an_hour() {
        let (secs, remaining) = parse_duration("half an hour");
        assert_eq!(secs, Some(1800));
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_half_hour() {
        let (secs, _) = parse_duration("half hour");
        assert_eq!(secs, Some(1800));
    }

    #[test]
    fn test_a_minute() {
        let (secs, _) = parse_duration("a minute");
        assert_eq!(secs, Some(60));
    }

    #[test]
    fn test_an_hour() {
        let (secs, _) = parse_duration("an hour");
        assert_eq!(secs, Some(3600));
    }

    #[test]
    fn test_word_numbers() {
        let (secs, _) = parse_duration("five minutes");
        assert_eq!(secs, Some(300));
    }

    #[test]
    fn test_word_compound() {
        let (secs, _) = parse_duration("two hours thirty minutes");
        assert_eq!(secs, Some(9000));
    }

    #[test]
    fn test_fallback_search() {
        let (secs, remaining) = parse_duration("set a timer for 5 minutes");
        assert_eq!(secs, Some(300));
        assert!(remaining.is_empty() || !remaining.contains("minutes"));
    }

    #[test]
    fn test_fallback_search_with_remaining() {
        let (secs, remaining) = parse_duration("set a timer for 5 minutes please");
        assert_eq!(secs, Some(300));
        assert_eq!(remaining.trim(), "please");
    }

    #[test]
    fn test_invalid_returns_none() {
        let (secs, remaining) = parse_duration("no duration here");
        assert_eq!(secs, None);
        assert_eq!(remaining, "no duration here");
    }

    #[test]
    fn test_empty_input() {
        let (secs, remaining) = parse_duration("");
        assert_eq!(secs, None);
        assert_eq!(remaining, "");
    }

    #[test]
    fn test_seconds() {
        let (secs, _) = parse_duration("30 seconds");
        assert_eq!(secs, Some(30));
    }

    #[test]
    fn test_hours() {
        let (secs, _) = parse_duration("2 hours");
        assert_eq!(secs, Some(7200));
    }

    #[test]
    fn test_and_a_half() {
        let (secs, _) = parse_duration("5 and a half minutes");
        assert_eq!(secs, Some(330)); // 5*60 + 30
    }

    #[test]
    fn test_remaining_text() {
        let (secs, remaining) = parse_duration("5 minutes do laundry");
        assert_eq!(secs, Some(300));
        assert_eq!(remaining.trim(), "do laundry");
    }

    #[test]
    fn test_with_and_connector() {
        let (secs, _) = parse_duration("1 hour and 30 minutes");
        assert_eq!(secs, Some(5400));
    }

    // --- format_systemd_duration tests ---

    #[test]
    fn test_systemd_format_seconds() {
        assert_eq!(format_systemd_duration(90), "1m30s");
    }

    #[test]
    fn test_systemd_format_hour() {
        assert_eq!(format_systemd_duration(3600), "1h");
    }

    #[test]
    fn test_systemd_format_zero() {
        assert_eq!(format_systemd_duration(0), "0s");
    }

    #[test]
    fn test_systemd_format_complex() {
        assert_eq!(format_systemd_duration(3661), "1h1m1s");
    }

    #[test]
    fn test_systemd_format_minutes_only() {
        assert_eq!(format_systemd_duration(300), "5m");
    }

    #[test]
    fn test_systemd_format_59s() {
        assert_eq!(format_systemd_duration(59), "59s");
    }

    #[test]
    fn test_systemd_format_60s() {
        assert_eq!(format_systemd_duration(60), "1m");
    }

    // --- format_human_duration tests ---

    #[test]
    fn test_human_format_zero() {
        assert_eq!(format_human_duration(0), "0 seconds");
    }

    #[test]
    fn test_human_format_singular() {
        assert_eq!(format_human_duration(1), "1 second");
        assert_eq!(format_human_duration(60), "1 minute");
        assert_eq!(format_human_duration(3600), "1 hour");
    }

    #[test]
    fn test_human_format_plural() {
        assert_eq!(format_human_duration(120), "2 minutes");
        assert_eq!(format_human_duration(7200), "2 hours");
    }

    #[test]
    fn test_human_format_compound() {
        assert_eq!(format_human_duration(5400), "1 hour 30 minutes");
    }

    #[test]
    fn test_human_format_complex() {
        assert_eq!(format_human_duration(3661), "1 hour 1 minute 1 second");
    }
}
