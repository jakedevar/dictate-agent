//! Per-stage latency accounting.
//!
//! The project is held to a ≤1.0s p50 end-to-end budget, and S12 is obligated
//! to record real turbo-CUDA numbers through these types. Timings are therefore
//! a required part of every [`Event::Final`](crate::Event::Final) and every
//! [`Transcript`](crate::Transcript), not an optional diagnostic — a stage that
//! forgets to report is visible as [`StageTiming::NotReported`] rather than
//! silently reading as zero.

use serde::{Deserialize, Serialize};

open_str_enum! {
    /// Why a pipeline stage did not run.
    ///
    /// These are the analytics that make the latency budget actionable: knowing
    /// the LLM pass was skipped *because the utterance was below the word
    /// threshold* is a different signal from it being skipped because Ollama
    /// was down.
    pub enum SkipReason {
        /// Turned off in configuration.
        Disabled => "disabled",
        /// The utterance was shorter than the configured minimum word count.
        BelowMinWords => "below_min_words",
        /// The route does not use this stage, e.g. the LLM formatting pass is
        /// not run for a `timer` utterance.
        RouteNotEligible => "route_not_eligible",
        /// This build or backend does not implement the stage.
        NotSupported => "not_supported",
        /// VAD found no speech in the captured audio.
        NoSpeechDetected => "no_speech_detected",
        /// The connection's negotiated capabilities do not permit the stage.
        /// A LAN client is not allowed to inject text into the host's desktop.
        NotPermitted => "not_permitted",
        /// The stage's dependency was unreachable and the pipeline failed open.
        DependencyUnavailable => "dependency_unavailable",
        /// The session was cancelled before this stage was reached.
        Cancelled => "cancelled",
    }
}

/// The outcome and cost of a single pipeline stage.
///
/// The four variants exist to keep three situations that a bare `f64` would
/// conflate distinguishable:
///
/// - a stage that ran and genuinely cost ~0ms (the pure-Rust rules layer) is
///   [`StageTiming::Ran`] with a near-zero `ms`;
/// - a stage that was deliberately not run is [`StageTiming::Skipped`], and
///   carries *why*;
/// - a stage that was attempted, burned wall-clock, and then failed open is
///   [`StageTiming::Failed`] — its time still counts against the budget and
///   must not disappear from the accounting;
/// - a stage the reporting peer said nothing about is
///   [`StageTiming::NotReported`], which is also the `Default`, so a field
///   added by a future protocol revision reads as "no data" on an older peer
///   rather than as a fabricated zero.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum StageTiming {
    /// The stage ran to completion.
    Ran {
        /// Wall-clock duration in milliseconds. Fractional: the rules layer is
        /// routinely sub-millisecond and rounding it to 0 would hide it.
        ms: f64,
    },

    /// The stage was deliberately not run.
    Skipped {
        /// Why.
        reason: SkipReason,
    },

    /// The stage was attempted and failed. Its cost still counts.
    Failed {
        /// Wall-clock duration burned before the failure.
        ms: f64,
        /// Human-readable detail, when available.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    /// The peer did not report this stage. Also the [`Default`], so a stage
    /// added by a future protocol revision reads as "no data" on an older peer
    /// rather than as a fabricated zero.
    #[default]
    NotReported,
}

impl StageTiming {
    /// Construct a completed stage from a millisecond measurement.
    #[must_use]
    pub fn ran(ms: f64) -> Self {
        Self::Ran { ms }
    }

    /// Construct a skipped stage.
    #[must_use]
    pub fn skipped(reason: SkipReason) -> Self {
        Self::Skipped { reason }
    }

    /// Wall-clock cost of this stage, if any was incurred.
    ///
    /// `Ran` and `Failed` both report time; `Skipped` and `NotReported` return
    /// `None` — which is the distinction this type exists to preserve. Callers
    /// summing a budget should treat `None` as "no cost", but callers charting
    /// stage coverage must not confuse it with `Some(0.0)`.
    #[must_use]
    pub fn elapsed_ms(&self) -> Option<f64> {
        match self {
            Self::Ran { ms } | Self::Failed { ms, .. } => Some(*ms),
            Self::Skipped { .. } | Self::NotReported => None,
        }
    }

    /// Whether the stage executed, successfully or not.
    #[must_use]
    pub fn did_run(&self) -> bool {
        matches!(self, Self::Ran { .. } | Self::Failed { .. })
    }
}

/// Per-stage latency breakdown for one dictation session.
///
/// Every field defaults to [`StageTiming::NotReported`], so a peer that only
/// fills in the stages it actually owns still produces a valid, honest record.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StageTimings {
    /// Audio capture, including the flush after the stop trigger.
    #[serde(default)]
    pub capture: StageTiming,
    /// Voice-activity gating and silence trimming (S11).
    #[serde(default)]
    pub vad: StageTiming,
    /// Speech-to-text inference (S12).
    #[serde(default)]
    pub stt: StageTiming,
    /// The deterministic rules/corrections layer (S20).
    #[serde(default)]
    pub fmt_rules: StageTiming,
    /// The LLM formatting pass (S21). Routinely
    /// [`StageTiming::Skipped`] — that is the intended fast path, not a defect.
    #[serde(default)]
    pub fmt_llm: StageTiming,
    /// Delivery into the target application (S13).
    #[serde(default)]
    pub inject: StageTiming,

    /// End-to-end wall clock from trigger to terminal state, in milliseconds.
    ///
    /// Reported separately rather than derived: it includes scheduling and
    /// queueing time that belongs to no single stage, so it is normally
    /// *greater* than the sum of the stages. The gap is itself the signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_ms: Option<f64>,

    /// Duration of the captured audio, in milliseconds.
    ///
    /// Needed to compute a real-time factor; without it the stage numbers
    /// cannot be compared across utterances of different lengths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_ms: Option<f64>,
}

impl StageTimings {
    /// Sum of every stage that actually incurred cost.
    ///
    /// Skipped and unreported stages contribute nothing.
    #[must_use]
    pub fn measured_ms(&self) -> f64 {
        self.stages()
            .iter()
            .filter_map(|(_, t)| t.elapsed_ms())
            .sum()
    }

    /// Real-time factor: processing time divided by audio duration.
    ///
    /// Below 1.0 means the pipeline is faster than real time. `None` when
    /// either the total or the audio duration is unreported, or the audio
    /// duration is zero.
    #[must_use]
    pub fn real_time_factor(&self) -> Option<f64> {
        let audio = self.audio_ms?;
        if audio <= 0.0 {
            return None;
        }
        Some(self.total_ms? / audio)
    }

    /// Every stage paired with its canonical wire name, in pipeline order.
    #[must_use]
    pub fn stages(&self) -> [(&'static str, &StageTiming); 6] {
        [
            ("capture", &self.capture),
            ("vad", &self.vad),
            ("stt", &self.stt),
            ("fmt_rules", &self.fmt_rules),
            ("fmt_llm", &self.fmt_llm),
            ("inject", &self.inject),
        ]
    }

    /// The slowest stage that actually ran, for regression triage.
    #[must_use]
    pub fn slowest_stage(&self) -> Option<(&'static str, f64)> {
        self.stages()
            .iter()
            .filter_map(|(n, t)| t.elapsed_ms().map(|ms| (*n, ms)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The constraint this type exists for: a stage that did not run must be
    /// distinguishable from a stage that took no measurable time.
    #[test]
    fn zero_duration_is_distinct_from_not_run() {
        let ran_free = StageTiming::ran(0.0);
        let skipped = StageTiming::skipped(SkipReason::BelowMinWords);
        let silent = StageTiming::NotReported;

        assert_ne!(ran_free, skipped);
        assert_ne!(ran_free, silent);
        assert_ne!(skipped, silent);

        assert_eq!(ran_free.elapsed_ms(), Some(0.0));
        assert_eq!(skipped.elapsed_ms(), None);
        assert_eq!(silent.elapsed_ms(), None);

        assert!(ran_free.did_run());
        assert!(!skipped.did_run());
        assert!(!silent.did_run());

        // ...and the distinction survives the wire.
        for t in [&ran_free, &skipped, &silent] {
            let json = serde_json::to_string(t).unwrap();
            assert_eq!(&serde_json::from_str::<StageTiming>(&json).unwrap(), t);
        }
    }

    #[test]
    fn failed_stage_still_counts_its_cost() {
        // The fail-open LLM case: Ollama hung for 3s, we fell through to the
        // rules-only output. Those 3 seconds are in the user's latency budget.
        let t = StageTiming::Failed {
            ms: 3000.0,
            error: Some("connection refused".into()),
        };
        assert_eq!(t.elapsed_ms(), Some(3000.0));
        assert!(t.did_run());
    }

    #[test]
    fn default_timings_are_all_unreported() {
        let t = StageTimings::default();
        for (name, stage) in t.stages() {
            assert_eq!(stage, &StageTiming::NotReported, "{name}");
        }
        assert_eq!(t.measured_ms(), 0.0);
        assert_eq!(t.real_time_factor(), None);
        assert_eq!(t.slowest_stage(), None);
    }

    #[test]
    fn measured_sum_ignores_skipped_stages() {
        let t = StageTimings {
            capture: StageTiming::ran(80.0),
            vad: StageTiming::ran(20.0),
            stt: StageTiming::ran(420.0),
            fmt_rules: StageTiming::ran(0.4),
            fmt_llm: StageTiming::skipped(SkipReason::BelowMinWords),
            inject: StageTiming::ran(60.0),
            total_ms: Some(600.0),
            audio_ms: Some(4000.0),
        };
        assert!((t.measured_ms() - 580.4).abs() < 1e-9);
        assert_eq!(t.slowest_stage(), Some(("stt", 420.0)));
        assert_eq!(t.real_time_factor(), Some(0.15));
    }

    #[test]
    fn real_time_factor_guards_against_zero_audio() {
        let t = StageTimings {
            total_ms: Some(100.0),
            audio_ms: Some(0.0),
            ..Default::default()
        };
        assert_eq!(t.real_time_factor(), None);
    }

    /// Forward-compat: an older peer reading a payload that omits stages must
    /// see NotReported, never a fabricated zero.
    #[test]
    fn omitted_stages_deserialize_as_not_reported() {
        let json = r#"{"stt":{"status":"ran","ms":300.0}}"#;
        let t: StageTimings = serde_json::from_str(json).unwrap();
        assert_eq!(t.stt, StageTiming::ran(300.0));
        assert_eq!(t.capture, StageTiming::NotReported);
        assert_eq!(t.fmt_llm, StageTiming::NotReported);
        assert_eq!(t.total_ms, None);
    }

    #[test]
    fn unknown_skip_reason_round_trips() {
        let json = r#"{"status":"skipped","reason":"quantum_decoherence"}"#;
        let t: StageTiming = serde_json::from_str(json).unwrap();
        assert_eq!(
            t,
            StageTiming::Skipped {
                reason: SkipReason::Unknown("quantum_decoherence".into())
            }
        );
        assert_eq!(serde_json::to_string(&t).unwrap(), json);
    }
}
