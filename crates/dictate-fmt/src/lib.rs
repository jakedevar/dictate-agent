//! Text formatting between speech recognition and injection.
//!
//! - [`text`] — the deterministic chain (S20): protected spans, the
//!   [`TextStage`] contract later stages plug into, and the built-in rules.
//! - [`grammar`] — the Ollama grammar pass (the LLM layer S21 replaces).
//! - [`text_cleanup`] — the artifact scrub still applied to LLM responses.

pub mod config;
pub mod grammar;
pub mod llm;
pub mod text;
pub mod text_cleanup;

pub use config::{FormatConfig, GrammarConfig, RulesConfig};
pub use grammar::GrammarCorrector;
pub use text::{
    ChainRun, Edit, FormatContext, ProtectedSpan, Replacement, Slot, SpanKind, SpanViolation,
    TextChain, TextDoc, TextStage,
};
