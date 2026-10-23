//! Explicitly unavailable injection adapter for API-only hosts. Never reads
//! the desktop or clipboard, even if a caller enables output in its config.
use crate::{BackendCapabilities, InjectionPolicy, Injector};
use dictate_proto::InjectionOutcome;

#[derive(Clone, PartialEq, Eq)]
pub struct Selection {
    pub text: String,
    pub window: u32,
}
impl std::fmt::Debug for Selection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Selection")
            .field("window", &self.window)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Debug)]
pub struct X11Injector;
impl X11Injector {
    pub fn new(_: &crate::OutputConfig) -> Self {
        Self
    }
    pub fn default_policy(&self) -> InjectionPolicy {
        InjectionPolicy::Off
    }
    pub fn inject_blocking(&self, _: &str, _: InjectionPolicy) -> InjectionOutcome {
        Self::unavailable()
    }
    pub fn inject_bound_blocking(
        &self,
        _: &str,
        _: InjectionPolicy,
        _: Option<Option<u32>>,
    ) -> InjectionOutcome {
        Self::unavailable()
    }
    pub fn capture_selection_blocking(&self, _: u32) -> anyhow::Result<Selection> {
        anyhow::bail!("injection unavailable on this host")
    }
    pub fn replace_selection_blocking(&self, _: &Selection, _: &str) -> InjectionOutcome {
        Self::unavailable()
    }
    fn unavailable() -> InjectionOutcome {
        InjectionOutcome::Unavailable {
            backend: "none".into(),
            reason: "API-only host".into(),
        }
    }
}
impl Injector for X11Injector {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::unavailable()
    }
    fn inject<'a>(
        &'a self,
        _: &'a str,
        _: InjectionPolicy,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = InjectionOutcome> + Send + 'a>> {
        Box::pin(async { Self::unavailable() })
    }
}
