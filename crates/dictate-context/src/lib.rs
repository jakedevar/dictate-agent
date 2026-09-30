//! Bounded focus capture and immutable, per-session profile resolution.
//! X11 I/O lives on a dedicated thread; absent context is an ordinary value.
mod profiles;
mod x11;

pub use dictate_proto::ResolvedProfile;
pub use profiles::{ContextConfig, Profile};
pub use x11::X11Context;

use std::sync::{Arc, Mutex};

/// A synthetic or captured window. No field is persisted by this crate.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WindowInfo {
    pub instance: Option<String>,
    pub class: Option<String>,
    pub title: Option<String>,
    pub pid: Option<u32>,
    pub process_name: Option<String>,
}

/// Implementations must bound capture time. Missing focus is never an error.
pub trait ContextProvider: Send + Sync + 'static {
    fn capture(&self) -> Option<WindowInfo>;
}

/// Wayland/headless baseline. Compositor adapters are deliberately deferred.
#[derive(Debug, Default)]
pub struct NoContext;
impl ContextProvider for NoContext {
    fn capture(&self) -> Option<WindowInfo> {
        None
    }
}

/// Mutable, synthetic test double; tests can move focus after session start.
#[derive(Debug, Default)]
pub struct TestContext(Mutex<Option<WindowInfo>>);
impl TestContext {
    pub fn new(window: Option<WindowInfo>) -> Self {
        Self(Mutex::new(window))
    }
    pub fn set(&self, window: Option<WindowInfo>) {
        *self.0.lock().expect("test context poisoned") = window;
    }
}
impl ContextProvider for TestContext {
    fn capture(&self) -> Option<WindowInfo> {
        self.0.lock().expect("test context poisoned").clone()
    }
}

/// Owns the validated profiles and one reusable provider.
pub struct ContextEngine {
    config: ContextConfig,
    provider: Arc<dyn ContextProvider>,
}
impl ContextEngine {
    pub fn new(config: ContextConfig, provider: Arc<dyn ContextProvider>) -> Self {
        Self { config, provider }
    }
    pub fn from_config(config: ContextConfig) -> Self {
        let provider: Arc<dyn ContextProvider> = match std::env::var("DISPLAY") {
            Ok(display) if !display.is_empty() && config.enabled => {
                Arc::new(X11Context::new(display))
            }
            _ => Arc::new(NoContext),
        };
        Self::new(config, provider)
    }
    pub fn disabled() -> Self {
        Self::new(
            ContextConfig {
                enabled: false,
                ..Default::default()
            },
            Arc::new(NoContext),
        )
    }
    /// Caller-supplied app IDs never trigger host focus or title discovery.
    /// Uploads/remote callers must pass `live = false`, even on an X11 host.
    pub fn resolve(&self, app: Option<&str>, live: bool) -> ResolvedProfile {
        if !self.config.enabled {
            return ResolvedProfile::default();
        }
        let window = match app {
            Some(app) => Some(WindowInfo {
                class: Some(app.to_owned()),
                ..Default::default()
            }),
            None if live => self.provider.capture(),
            None => None,
        };
        self.config.resolve(window.as_ref())
    }
}
