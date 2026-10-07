mod clipboard;
pub mod config;
pub mod output;
pub mod selection;

pub use config::{InjectionPolicy, OutputConfig};
pub use output::{BackendCapabilities, Injector, WaylandPortalStub, X11Injector};
