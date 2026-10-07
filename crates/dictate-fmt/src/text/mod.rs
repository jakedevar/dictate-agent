//! The deterministic text chain: everything between Whisper and the router.
//!
//! ```text
//! raw STT
//!   → protect               URLs, emails, paths, slash commands, code → placeholders
//!   → hallucination_scrub   trailing artifacts, whitespace — unprotected text only
//!   → builtin_corrections   the historical Whisper mis-hearings
//!   → [dictionary]          S22 plugs in here (Slot::Dictionary)
//!   → [snippets]            S24 plugs in here (Slot::Snippets)
//!   → spoken_punctuation, spoken_line_breaks (off by default)
//!   → fillers → stutters → numbers → spacing → casing → terminal_punctuation
//!   = rules output          routed, then (Route::Type only) given to the LLM
//! ```
//!
//! `protect` runs first (contract §1), on the raw transcript, so no stage —
//! the scrub included — can change a protected byte
//! (`SCRUB_CORRUPTS_PROTECTED_BYTES`: scrubbing first collapsed the spaces
//! inside `` `a  b` `` and deleted a backticked `[BLANK_AUDIO]`). The scrub's
//! artifacts get explicit exceptions: whisper.cpp's `[BLANK_AUDIO]` is never
//! protected outside backticks, and a trailing `/no_think` is removed even
//! though it is protected as a slash command. The scrub re-runs detection on
//! what it changed, so a token it unglued from an artifact is protected too.
//!
//! Every stage is synchronous, pure, and linear in the input. The whole chain
//! is one `fmt_rules` timing in the session's [`StageTimings`]; per-stage
//! micro-timings come back in [`ChainRun`] for debug logs.
//!
//! [`StageTimings`]: dictate_proto::StageTimings

mod doc;
mod lex;
mod protect;
pub mod rules;

use std::fmt;
use std::time::Instant;

use dictate_proto::{AppContext, Route, Tone};

pub use doc::{
    is_placeholder, Edit, EditError, ProtectedSpan, Replacement, SpanKind, SpanViolation, TextDoc,
    ViolationKind, MAX_PROTECTED_SPANS,
};
pub use protect::{detect_spans, Protect};

use crate::config::FormatConfig;

/// What the formatting layers know about the dictation they are formatting.
///
/// Pinned by the Wave 2 integration contract (§2): other slices add fields
/// only through the integrator.
#[derive(Debug, Clone, PartialEq)]
pub struct FormatContext {
    /// The destination app. `None` is the normal headless/remote case.
    pub app: Option<AppContext>,
    /// Register the LLM pass aims for; resolved from profile/category.
    pub tone: Tone,
    /// The session's route. Inside the deterministic chain this is
    /// **provisional** — the caller's forced route, else `Route::Type` —
    /// because routing runs on the chain's output. The LLM pass sees the
    /// resolved route.
    pub route: Route,
    /// Detected or pinned STT language.
    pub language: Option<String>,
    /// In-scope dictionary phrases (S22). May be empty.
    pub vocabulary: Vec<String>,
    /// Apply the personal dictionary and snippets this session
    /// (`SessionOptions.use_dictionary`). Defaults to `true`.
    pub use_dictionary: bool,
    /// Whether this session may leave persistent traces such as dictionary
    /// hit counts. `false` in privacy mode — and by default, so a caller that
    /// forgets to decide can only err toward privacy.
    pub persist: bool,
    /// Per-session override of `[format.rules] spoken_punctuation` from the
    /// resolved app profile (S23). `None` keeps the configured default.
    pub spoken_punctuation: Option<bool>,
    /// Per-session override of `[format.rules] spoken_line_breaks`.
    pub spoken_line_breaks: Option<bool>,
    /// `SessionOptions.format_llm` as the caller sent it. `Some(true)` asks
    /// the LLM pass to run below `min_words` and despite a profile that
    /// turned it off; `Some(false)` never reaches a formatter (the pipeline
    /// skips the pass as `disabled`).
    pub format_llm: Option<bool>,
    /// Byte ranges of the protected spans in the text handed to the LLM
    /// pass. Set by the pipeline after the chain runs; empty inside it.
    pub protected: Vec<std::ops::Range<usize>>,
}

impl Default for FormatContext {
    fn default() -> Self {
        Self {
            app: None,
            tone: Tone::default(),
            route: Route::default(),
            language: None,
            vocabulary: Vec::new(),
            use_dictionary: true,
            persist: false,
            spoken_punctuation: None,
            spoken_line_breaks: None,
            format_llm: None,
            protected: Vec::new(),
        }
    }
}

/// A stage that a per-session profile can switch on or off.
///
/// The spoken-command rules are off by default (in code prompts "period" and
/// "new line" are usually literal) but an app profile may enable them, so they
/// are always present in the chain and consult the session's override here.
struct ProfileToggled {
    inner: Box<dyn TextStage>,
    default_on: bool,
    select: fn(&FormatContext) -> Option<bool>,
}

impl TextStage for ProfileToggled {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn apply(&self, doc: &mut TextDoc, ctx: &FormatContext) {
        if (self.select)(ctx).unwrap_or(self.default_on) {
            self.inner.apply(doc, ctx);
        }
    }
}

/// One deterministic text stage.
///
/// Implementations must be pure (no I/O, no clocks, no randomness), must not
/// block, and must run in time linear in the text. They edit the document's
/// working text; placeholders in it are protected spans and must be left
/// exactly as they are (use [`TextDoc::apply_edits`], which enforces that). A
/// stage that produces text no later stage may alter protects it with
/// [`Replacement::Protected`].
pub trait TextStage: Send + Sync {
    /// Stable name, used in debug timings and configuration.
    fn name(&self) -> &'static str;

    /// Apply this stage to `doc`.
    fn apply(&self, doc: &mut TextDoc, ctx: &FormatContext);
}

/// Where a pluggable stage runs. The built-in stages have fixed positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// After the built-in corrections, before snippets (S22).
    Dictionary,
    /// After the dictionary, before the rules (S24).
    Snippets,
}

/// Wall time of one stage in one run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StageTime {
    /// The stage's [`TextStage::name`].
    pub name: &'static str,
    /// Microseconds.
    pub micros: f64,
}

/// The per-stage timings of one run, displayable as one debug-log line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StageTimes(pub Vec<StageTime>);

impl StageTimes {
    /// Sum of the stage times, in milliseconds.
    #[must_use]
    pub fn total_ms(&self) -> f64 {
        self.0.iter().map(|t| t.micros).sum::<f64>() / 1000.0
    }
}

impl fmt::Display for StageTimes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, t) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{}={:.1}µs", t.name, t.micros)?;
        }
        Ok(())
    }
}

/// The result of running the chain.
#[derive(Debug, Clone)]
pub struct ChainRun {
    /// The formatted document; [`TextDoc::restore`] gives the rules output.
    pub doc: TextDoc,
    /// How long each stage took.
    pub timings: StageTimes,
}

/// The ordered deterministic chain.
pub struct TextChain {
    enabled: bool,
    leading: Vec<Box<dyn TextStage>>,
    dictionary: Vec<Box<dyn TextStage>>,
    snippets: Vec<Box<dyn TextStage>>,
    rules: Vec<Box<dyn TextStage>>,
}

impl fmt::Debug for TextChain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextChain")
            .field("enabled", &self.enabled)
            .field("stages", &self.stage_names())
            .finish()
    }
}

impl Default for TextChain {
    /// The chain with every rule at its default setting.
    fn default() -> Self {
        Self::standard(&FormatConfig::default())
    }
}

impl TextChain {
    /// The built-in chain as configured. Cheap enough to build per profile.
    #[must_use]
    pub fn standard(config: &FormatConfig) -> Self {
        use rules::*;
        let r = &config.rules;
        let mut leading: Vec<Box<dyn TextStage>> = vec![Box::new(Protect)];
        if r.hallucination_scrub {
            leading.push(Box::new(HallucinationScrub));
        }
        if r.builtin_corrections {
            leading.push(Box::new(BuiltinCorrections::new(r.claude_corrections)));
        }
        let mut rules: Vec<Box<dyn TextStage>> = vec![
            Box::new(ProfileToggled {
                inner: Box::new(SpokenPunctuation),
                default_on: r.spoken_punctuation,
                select: |ctx| ctx.spoken_punctuation,
            }),
            Box::new(ProfileToggled {
                inner: Box::new(SpokenLineBreaks),
                default_on: r.spoken_line_breaks,
                select: |ctx| ctx.spoken_line_breaks,
            }),
        ];
        if r.fillers {
            rules.push(Box::new(Fillers));
        }
        if r.stutters {
            rules.push(Box::new(Stutters));
        }
        if r.numbers {
            rules.push(Box::new(Numbers));
        }
        if r.spacing {
            rules.push(Box::new(Spacing));
        }
        if r.casing {
            rules.push(Box::new(Casing));
        }
        if r.terminal_punctuation {
            rules.push(Box::new(TerminalPunctuation));
        }
        Self {
            enabled: config.enabled,
            leading,
            dictionary: Vec::new(),
            snippets: Vec::new(),
            rules,
        }
    }

    /// Add a stage at `slot` (after any stage already there).
    #[must_use]
    pub fn with_stage(mut self, slot: Slot, stage: Box<dyn TextStage>) -> Self {
        match slot {
            Slot::Dictionary => self.dictionary.push(stage),
            Slot::Snippets => self.snippets.push(stage),
        }
        self
    }

    /// Whether `[format] enabled` is on.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Stage names in execution order.
    #[must_use]
    pub fn stage_names(&self) -> Vec<&'static str> {
        self.stages().map(|s| s.name()).collect()
    }

    fn stages(&self) -> impl Iterator<Item = &dyn TextStage> + '_ {
        self.leading
            .iter()
            .chain(&self.dictionary)
            .chain(&self.snippets)
            .chain(&self.rules)
            .map(AsRef::as_ref)
    }

    /// Run every stage over `input`.
    ///
    /// Runs regardless of [`is_enabled`](Self::is_enabled); the pipeline
    /// checks that and reports `skipped{disabled}` itself.
    #[must_use]
    pub fn run(&self, input: &str, ctx: &FormatContext) -> ChainRun {
        let mut doc = TextDoc::new(input);
        let mut timings = Vec::with_capacity(12);
        for stage in self.stages() {
            let started = Instant::now();
            stage.apply(&mut doc, ctx);
            timings.push(StageTime {
                name: stage.name(),
                micros: started.elapsed().as_secs_f64() * 1e6,
            });
        }
        ChainRun {
            doc,
            timings: StageTimes(timings),
        }
    }

    /// Convenience: run and restore.
    #[must_use]
    pub fn format(&self, input: &str, ctx: &FormatContext) -> String {
        self.run(input, ctx).doc.restore()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RulesConfig;

    #[test]
    fn the_default_chain_runs_in_the_pinned_order() {
        assert_eq!(
            TextChain::default().stage_names(),
            vec![
                "protect",
                "hallucination_scrub",
                "builtin_corrections",
                // Always present, inert unless config or an app profile
                // enables them for the session.
                "spoken_punctuation",
                "spoken_line_breaks",
                "fillers",
                "stutters",
                "numbers",
                "spacing",
                "casing",
                "terminal_punctuation",
            ]
        );
    }

    #[test]
    fn every_rule_toggle_removes_exactly_its_stage_and_protect_always_runs() {
        let all_off = FormatConfig {
            enabled: true,
            rules: RulesConfig {
                builtin_corrections: false,
                claude_corrections: false,
                hallucination_scrub: false,
                fillers: false,
                stutters: false,
                numbers: false,
                casing: false,
                spacing: false,
                terminal_punctuation: false,
                spoken_punctuation: false,
                spoken_line_breaks: false,
            },
            ..FormatConfig::default()
        };
        assert_eq!(
            TextChain::standard(&all_off).stage_names(),
            vec!["protect", "spoken_punctuation", "spoken_line_breaks"],
            "only the profile-switchable spoken rules remain, and they are inert"
        );
        assert_eq!(
            TextChain::standard(&all_off).format("a new line b period", &FormatContext::default()),
            "a new line b period"
        );
        let all_on = FormatConfig {
            enabled: true,
            rules: RulesConfig {
                spoken_punctuation: true,
                spoken_line_breaks: true,
                ..RulesConfig::default()
            },
            ..FormatConfig::default()
        };
        let names = TextChain::standard(&all_on).stage_names();
        assert_eq!(names.len(), 11);
        let pos = |n| names.iter().position(|x| *x == n).unwrap();
        assert!(pos("spoken_punctuation") < pos("fillers"));
        assert!(pos("spoken_line_breaks") < pos("casing"));
    }

    #[test]
    fn an_app_profile_switches_spoken_commands_per_session() {
        let chain = TextChain::default(); // both spoken rules off in config
        let plain = FormatContext::default();
        let with_breaks = FormatContext {
            spoken_line_breaks: Some(true),
            ..FormatContext::default()
        };
        assert!(!chain.format("first new line second", &plain).contains('\n'));
        assert!(chain
            .format("first new line second", &with_breaks)
            .contains('\n'));

        let on = TextChain::standard(&FormatConfig {
            enabled: true,
            rules: RulesConfig {
                spoken_line_breaks: true,
                ..RulesConfig::default()
            },
            ..FormatConfig::default()
        });
        let profile_off = FormatContext {
            spoken_line_breaks: Some(false),
            ..FormatContext::default()
        };
        assert!(on.format("first new line second", &plain).contains('\n'));
        assert!(
            !on.format("first new line second", &profile_off)
                .contains('\n'),
            "a profile can also switch a configured rule off"
        );
    }

    struct Upper;
    impl TextStage for Upper {
        fn name(&self) -> &'static str {
            "test_upper"
        }
        fn apply(&self, doc: &mut TextDoc, _ctx: &FormatContext) {
            let text = doc.working_text();
            let edits: Vec<Edit> = text
                .match_indices("widget")
                .map(|(i, m)| Edit::protected(i..i + m.len(), "WidgetCo", SpanKind::Term))
                .collect();
            doc.apply_edits(edits).unwrap();
        }
    }

    #[test]
    fn plugged_stages_run_in_their_slot_and_their_output_is_protected() {
        let chain = TextChain::default().with_stage(Slot::Dictionary, Box::new(Upper));
        let names = chain.stage_names();
        assert_eq!(names[3], "test_upper", "right after builtin_corrections");
        let run = chain.run(
            "um the widget the widget is broken",
            &FormatContext::default(),
        );
        // `WidgetCo` is protected: casing does not touch it, stutters do not
        // see "the WidgetCo the WidgetCo" as anything, and it survives intact.
        assert_eq!(run.doc.restore(), "The WidgetCo the WidgetCo is broken.");
        assert_eq!(run.doc.spans().len(), 2);
        assert_eq!(run.timings.0.len(), names.len());
    }

    #[test]
    fn stage_times_render_as_one_log_line() {
        let times = StageTimes(vec![
            StageTime {
                name: "a",
                micros: 1.25,
            },
            StageTime {
                name: "b",
                micros: 10.0,
            },
        ]);
        assert_eq!(times.to_string(), "a=1.2µs b=10.0µs");
        assert!((times.total_ms() - 0.01125).abs() < 1e-9);
    }

    /// `SCRUB_CORRUPTS_PROTECTED_BYTES`: the bytes `protect` finds in the
    /// *original* input are intact after every stage, not just at the end —
    /// the oracle is detection on the raw input, not on scrubbed text.
    #[test]
    fn protected_bytes_survive_every_stage() {
        let chain = TextChain::standard(&FormatConfig {
            enabled: true,
            rules: RulesConfig {
                spoken_punctuation: true,
                spoken_line_breaks: true,
                ..RulesConfig::default()
            },
            ..FormatConfig::default()
        });
        let ctx = FormatContext::default();
        for input in [
            "keep `a  b` exactly, um, period",
            "keep `[BLANK_AUDIO]` and `x\ty` exactly",
            "um see https://example.com/a\u{200B}b and ~/x/\u{FEFF}y.rs new line then user_id",
            "the the  create plan  for `  spaced  ` code. thank you.",
            "open ~/.cloud/x and five pm and twenty five thousand",
        ] {
            let originals: Vec<String> = TextDoc::protected(input)
                .spans_in_text_order()
                .map(|s| s.text.clone())
                .collect();
            assert!(!originals.is_empty(), "{input:?}");
            let mut doc = TextDoc::new(input);
            for stage in chain.stages() {
                stage.apply(&mut doc, &ctx);
                let out = doc.restore();
                let mut pos = 0;
                for span in &originals {
                    let at = out[pos..].find(span.as_str()).unwrap_or_else(|| {
                        panic!(
                            "after `{}`, {span:?} from {input:?} is gone: {out:?}",
                            stage.name()
                        )
                    });
                    pos += at + span.len();
                }
            }
        }
    }

    #[test]
    fn protect_runs_first() {
        assert_eq!(TextChain::default().stage_names()[0], "protect");
    }
}
