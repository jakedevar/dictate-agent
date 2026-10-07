mod clipboard;
pub mod config;
pub mod output;
pub mod selection;

pub use config::{InjectionPolicy, OutputConfig};
pub use output::{BackendCapabilities, Injector, WaylandPortalStub, X11Injector};

/// Capture the X11 input window for stop-time delivery binding.
pub use clipboard::focused_window;
