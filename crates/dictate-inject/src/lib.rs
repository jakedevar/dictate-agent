#[cfg(target_os = "linux")]
mod clipboard;
pub mod config;
pub mod output;
#[cfg(target_os = "linux")]
pub mod selection;

pub use config::{InjectionPolicy, OutputConfig};
pub use output::{BackendCapabilities, Injector, WaylandPortalStub, X11Injector};

/// Capture the X11 input window for stop-time delivery binding.
#[cfg(target_os = "linux")]
pub use clipboard::focused_window;

#[cfg(not(target_os = "linux"))]
#[path = "selection_unavailable.rs"]
pub mod selection;
#[cfg(not(target_os = "linux"))]
pub fn focused_window() -> Option<u32> {
    None
}
