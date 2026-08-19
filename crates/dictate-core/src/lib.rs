//! The dictation engine: state machine, session orchestration, and the
//! pipeline that turns a held hotkey into text in somebody's editor.
//!
//! This crate owns *behavior*. It has no socket, no signal handler, and no
//! command-line parsing — those belong to `dictated`, which hosts this engine
//! behind the [`dictate_proto`] control plane. Keeping the split sharp is what
//! lets the whole state machine, including every cancellation path, be tested
//! without a daemon, a device, or a GPU.
//!
//! # The pieces
//!
//! | Module | Responsibility |
//! |---|---|
//! | [`engine`] | serializes every command through one task and owns the session slot |
//! | [`pipeline`] | runs one session from `Recording` to a terminal state |
//! | [`session`] | session identity, ownership rules, and the one state-transition point |
//! | [`cancel`] | cancellation with an explicit commit point for the irreversible stage |
//! | [`event_bus`] | fan-out of events to every subscribed client |
//! | [`ports`] | the seams the pipeline runs against, with production and test implementations |
//!
//! # Where the concurrency is decided
//!
//! Only [`engine`] mutates daemon state, and only from its own task. Every
//! other module is either pure, owned by a single session task, or a
//! `Send + Sync` port. The awkward questions this slice exists to answer —
//! racing starts, a cancel mid-injection, a client disappearing — are all
//! decided in one file, by one thread, in mailbox order.

pub mod cancel;
pub mod config;
pub mod engine;
pub mod event_bus;
pub mod local_executor;
pub mod notify;
pub mod pipeline;
pub mod ports;
pub mod router;
pub mod session;
pub mod timer;

pub use cancel::{CancelToken, CancelVerdict};
pub use config::Config;
pub use engine::{resolve_options, Engine, EngineHandle, ToggleOutcome};
pub use event_bus::EventBus;
pub use pipeline::{Pipeline, PipelineOutcome, ResolvedOptions};
pub use session::{Actor, ClientId, ClientIdGen, SessionHandle, SessionOwner};
