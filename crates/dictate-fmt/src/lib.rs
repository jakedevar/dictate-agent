//! Text formatting between speech recognition and injection.
//!
//! - [`text`] — the deterministic chain (S20): protected spans, the
//!   [`TextStage`] contract later stages plug into, and the built-in rules.
//! - [`llm`] — the context-aware LLM formatting layer (S21).
//! - [`text_cleanup`] — the model-artifact scrub applied to LOCAL-route answers.

pub mod config;
pub mod llm;
pub mod text;
pub mod text_cleanup;

pub use config::{FormatConfig, GrammarConfig, RulesConfig};
pub use text::{
    ChainRun, Edit, FormatContext, ProtectedSpan, Replacement, Slot, SpanKind, SpanViolation,
    TextChain, TextDoc, TextStage,
};
