use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use dictate_proto::{ErrorCode, InjectMethod, InjectionOutcome, ProtoError, SkipReason};
use enigo::{Enigo, Keyboard, Settings};
use tracing::{info, warn};

use crate::config::InjectionPolicy;

/// Runtime facts that policy resolution needs. A backend must never select
/// paste merely because configuration asked for it: paste is only safe when
/// the old clipboard can be restored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendCapabilities {
    pub backend: String,
    pub clipboard_save_restore: bool,
    pub direct_typing: bool,
    pub needs_user_consent: bool,
}

impl BackendCapabilities {
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            backend: "none".into(),
            clipboard_save_restore: false,
            direct_typing: false,
            needs_user_consent: false,
        }
    }
}

/// Resolve a requested policy against actual backend capabilities.
#[must_use]
pub fn resolve_policy(requested: InjectionPolicy, caps: &BackendCapabilities) -> InjectionPolicy {
    match requested {
        InjectionPolicy::Off => InjectionPolicy::Off,
        InjectionPolicy::Paste if caps.clipboard_save_restore => InjectionPolicy::Paste,
        InjectionPolicy::Paste | InjectionPolicy::Type if caps.direct_typing => {
            InjectionPolicy::Type
        }
        InjectionPolicy::Paste | InjectionPolicy::Type => InjectionPolicy::Off,
    }
}

/// Split direct typing at Unicode scalar boundaries. The limit is clamped so a
/// zero configuration can never loop forever.
#[must_use]
pub fn chunk_text(text: &str, max_chars: usize) -> Vec<&str> {
    let limit = max_chars.max(1);
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut count = 0;
    for (byte, _) in text.char_indices() {
        if count == limit {
            chunks.push(&text[start..byte]);
            start = byte;
            count = 0;
        }
        count += 1;
    }
    if start < text.len() {
        chunks.push(&text[start..]);
    }
    chunks
}

/// The future is intentional. Wayland portals may return `AwaitingConsent`,
/// so callers must not bake in a synchronous result model.
pub trait Injector: Send + Sync + 'static {
    fn capabilities(&self) -> BackendCapabilities;

    fn inject<'a>(
        &'a self,
        text: &'a str,
        requested: InjectionPolicy,
    ) -> Pin<Box<dyn Future<Output = InjectionOutcome> + Send + 'a>>;
}

/// X11 implementation. It has no user-consent flow, but shares the async
/// contract required by Wayland portal backends.
#[derive(Clone)]
pub struct X11Injector {
    pub(crate) enabled: bool,
    default_policy: InjectionPolicy,
    chunk_chars: usize,
    // Serialize clipboard transactions; selection service threads retain owners.
    pub(crate) clipboard: Arc<Mutex<()>>,
}

impl std::fmt::Debug for X11Injector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11Injector")
            .field("enabled", &self.enabled)
            .field("default_policy", &self.default_policy)
            .field("chunk_chars", &self.chunk_chars)
            .finish_non_exhaustive()
    }
}

impl X11Injector {
    #[must_use]
    pub fn new(config: &crate::config::OutputConfig) -> Self {
        Self {
            enabled: config.auto_type,
            default_policy: config.policy,
            chunk_chars: config.type_chunk_chars.max(1),
            clipboard: Arc::new(Mutex::new(())),
        }
    }

    #[must_use]
    pub fn default_policy(&self) -> InjectionPolicy {
        self.default_policy
    }

    /// Blocking X11 transaction, exposed for the daemon adapter to put on its
    /// blocking pool. The trait method remains async for portal compatibility.
    pub fn inject_blocking(&self, text: &str, requested: InjectionPolicy) -> InjectionOutcome {
        self.inject_bound_blocking(text, requested, None)
    }

    /// `Some(Some(window))` binds delivery to stop-time focus; `Some(None)` means
    /// focus could not be captured and requires clipboard-only delivery.
    /// This additive API also supports selection-replacement callers.
    pub fn inject_bound_blocking(
        &self,
        text: &str,
        requested: InjectionPolicy,
        destination: Option<Option<u32>>,
    ) -> InjectionOutcome {
        if !self.enabled {
            return InjectionOutcome::Skipped {
                reason: SkipReason::Disabled,
            };
        }
        if requested == InjectionPolicy::Off {
            return InjectionOutcome::Skipped {
                reason: SkipReason::Disabled,
            };
        }
        let text = text.trim();
        if text.is_empty() {
            return InjectionOutcome::Skipped {
                reason: SkipReason::NoSpeechDetected,
            };
        }
        let caps = self.capabilities();
        let policy = resolve_policy(requested, &caps);
        let chars = text.chars().count() as u32;
        let result = match policy {
            InjectionPolicy::Paste => self.paste_transaction(text, destination),
            InjectionPolicy::Type => {
                if destination.is_some_and(|expected| {
                    expected.is_none() || expected != crate::clipboard::focused_window()
                }) {
                    self.copy_for_focus_change(text)
                } else {
                    self.type_chunks(text, destination)
                        .map(|()| InjectMethod::Keystroke)
                }
            }
            InjectionPolicy::Off => Err(anyhow!("no safe injection method is available")),
        };
        match result {
            Ok(method) => {
                info!(?method, chars, "injected text");
                InjectionOutcome::Injected { method, chars }
            }
            Err(error) => InjectionOutcome::Failed {
                error: ProtoError::new(ErrorCode::InjectionFailed, error.to_string()),
            },
        }
    }

    fn copy_for_focus_change(&self, text: &str) -> Result<InjectMethod> {
        let paste = crate::clipboard::PasteSelection::new(text)?;
        let owner = paste.owner()?;
        paste.claim(owner)?;
        paste.retain();
        Err(anyhow!("Dictation copied: focus changed"))
    }

    /// Never restore or retry after an uncertain key send or transfer timeout.
    /// Keeping the dictation owned prevents a delayed Ctrl+V from pasting the
    /// previous clipboard. A concurrent copy always retains ownership.
    fn paste_transaction(
        &self,
        text: &str,
        destination: Option<Option<u32>>,
    ) -> Result<InjectMethod> {
        let _guard = self
            .clipboard
            .lock()
            .map_err(|_| anyhow!("clipboard lock poisoned"))?;
        let mut paste = crate::clipboard::PasteSelection::new(text)?;
        let focused = paste.focus()?;
        let expected = destination.unwrap_or(focused);
        if expected.is_none() || focused != expected {
            return self.copy_for_focus_change(text);
        }
        let (owner, saved) = paste.snapshot()?;
        paste.claim(owner)?;
        // Claim precedes the final focus check and the key send. Once this
        // function attempts a send, even an error may represent queued keys.
        let result = paste.send_paste(expected.unwrap()).and_then(|sent| {
            if !sent {
                return Err(anyhow!("Dictation copied: focus changed"));
            }
            paste.transfer_to(expected.unwrap())
        });
        if result.is_ok() {
            if let Err(error) = paste.restore(saved) {
                warn!("text pasted; clipboard restoration failed: {error}");
            }
        }
        paste.retain();
        result.map(|()| InjectMethod::Paste)
    }

    fn type_chunks(&self, text: &str, destination: Option<Option<u32>>) -> Result<()> {
        let _guard = self
            .clipboard
            .lock()
            .map_err(|_| anyhow!("clipboard lock poisoned"))?;
        let mut enigo = Enigo::new(&Settings::default()).context("opening X11 input backend")?;
        for chunk in chunk_text(text, self.chunk_chars) {
            if destination.is_some_and(|expected| {
                expected.is_none() || expected != crate::clipboard::focused_window()
            }) {
                return self.copy_for_focus_change(text).map(|_| ());
            }
            enigo.text(chunk)?;
        }
        Ok(())
    }
}

impl Injector for X11Injector {
    fn capabilities(&self) -> BackendCapabilities {
        if !self.enabled || std::env::var_os("DISPLAY").is_none() {
            return BackendCapabilities::unavailable();
        }
        BackendCapabilities {
            backend: "x11".into(),
            clipboard_save_restore: true,
            direct_typing: true,
            needs_user_consent: false,
        }
    }

    fn inject<'a>(
        &'a self,
        text: &'a str,
        requested: InjectionPolicy,
    ) -> Pin<Box<dyn Future<Output = InjectionOutcome> + Send + 'a>> {
        Box::pin(async move { self.inject_blocking(text, requested) })
    }
}

/// Contract stub for a future Wayland portal implementation.
///
/// It is intentionally not selected by the daemon yet. Its purpose is to keep
/// the backend boundary honest: a portal request can be accepted while still
/// awaiting a user decision, which is neither success nor failure.
#[derive(Debug, Clone, Copy, Default)]
pub struct WaylandPortalStub;

impl Injector for WaylandPortalStub {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            backend: "portal".into(),
            clipboard_save_restore: false,
            direct_typing: true,
            needs_user_consent: true,
        }
    }

    fn inject<'a>(
        &'a self,
        text: &'a str,
        requested: InjectionPolicy,
    ) -> Pin<Box<dyn Future<Output = InjectionOutcome> + Send + 'a>> {
        Box::pin(async move {
            if requested == InjectionPolicy::Off {
                return InjectionOutcome::Skipped {
                    reason: SkipReason::Disabled,
                };
            }
            if text.trim().is_empty() {
                return InjectionOutcome::Skipped {
                    reason: SkipReason::NoSpeechDetected,
                };
            }
            InjectionOutcome::AwaitingConsent {
                backend: "portal".into(),
                consent_id: None,
            }
        })
    }
}

/// Tool availability probe for the future Wayland adapter. KDE lacks the
/// virtual-keyboard protocol `wtype` relies on, so never advertise it there.
#[must_use]
pub fn wayland_tool_chain<F>(desktop: &str, exists: F) -> Vec<&'static str>
where
    F: Fn(&str) -> bool,
{
    let kde = desktop.to_ascii_lowercase().contains("kde")
        || desktop.to_ascii_lowercase().contains("plasma");
    ["wtype", "kwtype", "dotool", "ydotool"]
        .into_iter()
        .filter(|tool| !(*tool == "wtype" && kde) && exists(tool))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_policy_falls_back_to_type_without_clipboard_restore() {
        let caps = BackendCapabilities {
            backend: "wayland".into(),
            clipboard_save_restore: false,
            direct_typing: true,
            needs_user_consent: true,
        };
        assert_eq!(
            resolve_policy(InjectionPolicy::Paste, &caps),
            InjectionPolicy::Type
        );
    }

    #[test]
    fn off_policy_does_not_fall_back_to_the_clipboard() {
        let config = crate::config::OutputConfig {
            policy: InjectionPolicy::Off,
            ..Default::default()
        };
        assert!(matches!(
            X11Injector::new(&config).inject_blocking("do not inject", InjectionPolicy::Off),
            InjectionOutcome::Skipped {
                reason: SkipReason::Disabled
            }
        ));
    }

    #[test]
    fn chunking_preserves_unicode_and_never_splits_scalars() {
        assert_eq!(chunk_text("a❤️b", 2), vec!["a❤", "️b"]);
        assert_eq!(chunk_text("abc", 0), vec!["a", "b", "c"]);
    }

    #[test]
    fn kde_wayland_gates_wtype_but_keeps_other_tools() {
        assert_eq!(
            wayland_tool_chain("KDE Plasma", |tool| matches!(tool, "wtype" | "kwtype")),
            vec!["kwtype"]
        );
        assert_eq!(
            wayland_tool_chain("GNOME", |tool| tool == "wtype"),
            vec!["wtype"]
        );
    }

    #[tokio::test]
    async fn wayland_stub_reports_pending_consent_without_claiming_delivery() {
        let stub = WaylandPortalStub;
        let caps = stub.capabilities();
        assert!(caps.needs_user_consent);
        assert_eq!(
            resolve_policy(InjectionPolicy::Paste, &caps),
            InjectionPolicy::Type
        );

        let outcome = stub.inject("needs consent", InjectionPolicy::Paste).await;
        assert!(matches!(
            outcome,
            InjectionOutcome::AwaitingConsent {
                ref backend,
                consent_id: None
            } if backend == "portal"
        ));
        assert!(!outcome.is_settled());
    }
}
