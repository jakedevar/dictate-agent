use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Once};

use anyhow::{anyhow, Context, Result};
use arboard::Clipboard;
use dictate_proto::{ErrorCode, InjectMethod, InjectionOutcome, ProtoError, SkipReason};
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use tracing::{error, info, warn};

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
    // Serialize transactions and keep restored X11 selection ownership alive.
    pub(crate) clipboard: Arc<Mutex<Option<Clipboard>>>,
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
            clipboard: Arc::new(Mutex::new(None)),
        }
    }

    #[must_use]
    pub fn default_policy(&self) -> InjectionPolicy {
        self.default_policy
    }

    /// Blocking X11 transaction, exposed for the daemon adapter to put on its
    /// blocking pool. The trait method remains async for portal compatibility.
    pub fn inject_blocking(&self, text: &str, requested: InjectionPolicy) -> InjectionOutcome {
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
            InjectionPolicy::Paste => self.paste_transaction(text).map(|()| InjectMethod::Paste),
            InjectionPolicy::Type => self.type_chunks(text).map(|()| InjectMethod::Keystroke),
            InjectionPolicy::Off => Err(anyhow!("no safe injection method is available")),
        };
        match result {
            Ok(method) => {
                info!(?method, chars, "injected text");
                InjectionOutcome::Injected { method, chars }
            }
            Err(e) if policy == InjectionPolicy::Paste && caps.direct_typing => {
                // A clipboard can disappear between probing and use. Preserve
                // delivery by immediately trying the safe non-clipboard route.
                warn!("clipboard injection failed ({e}); falling back to direct typing");
                match self.type_chunks(text) {
                    Ok(()) => InjectionOutcome::Injected {
                        method: InjectMethod::Keystroke,
                        chars,
                    },
                    Err(type_error) => self.clipboard_failure(
                        text,
                        anyhow!("paste failed: {e}; direct typing failed: {type_error}"),
                    ),
                }
            }
            Err(e) => self.clipboard_failure(text, e),
        }
    }

    fn clipboard_failure(&self, text: &str, error: anyhow::Error) -> InjectionOutcome {
        // Last-resort delivery: leave the transcript in the clipboard and make
        // the failure visible to the core notifier. Do not claim injection.
        let clipboard_note = Clipboard::new().and_then(|mut c| c.set_text(text.to_owned()));
        if let Err(clipboard_error) = clipboard_note {
            error!("injection failed ({error}); clipboard fallback also failed: {clipboard_error}");
        } else {
            error!("injection failed; transcript copied to clipboard: {error}");
        }
        InjectionOutcome::Failed {
            error: ProtoError::new(
                ErrorCode::InjectionFailed,
                format!("injection failed; text copied to clipboard when possible: {error}"),
            ),
        }
    }

    /// Snapshot before taking ownership; serve the text until the application
    /// requests it, then restore. A successful transfer must never be retried
    /// through typing, even if restoration fails.
    fn paste_transaction(&self, text: &str) -> Result<()> {
        static FORMAT_WARNING: Once = Once::new();
        FORMAT_WARNING.call_once(|| warn!(
            "clipboard backup preserves text, HTML with text, or image pixels; arbitrary X11 targets (including file lists and mixed image/text) are not preserved"
        ));
        let mut guard = self
            .clipboard
            .lock()
            .map_err(|_| anyhow!("clipboard lock poisoned"))?;
        if guard.is_none() {
            *guard = Some(Clipboard::new().context("opening clipboard")?);
        }
        let clipboard = guard.as_mut().expect("initialized above");
        let saved = crate::clipboard::Snapshot::capture(clipboard)?;
        let paste = crate::clipboard::PasteSelection::new(text)?;
        paste.claim()?;
        let paste_result = self.send_paste().and_then(|()| paste.transfer(text));
        let restore_result = saved.restore(clipboard);
        settle_paste(paste_result, restore_result)
    }

    pub(crate) fn send_paste(&self) -> Result<()> {
        let mut enigo = Enigo::new(&Settings::default()).context("opening X11 input backend")?;
        enigo.key(Key::Control, Direction::Press)?;
        let result = enigo.key(Key::Unicode('v'), Direction::Click);
        // Always release Ctrl, including a failed click, to avoid a stuck modifier.
        let release = enigo.key(Key::Control, Direction::Release);
        result?;
        if let Err(error) = release {
            // The click may already have reached the application. Continue the
            // selection handshake rather than risking duplicate delivery.
            warn!("Ctrl+V was sent, but releasing Ctrl failed: {error}");
        }
        Ok(())
    }

    fn type_chunks(&self, text: &str) -> Result<()> {
        let mut enigo = Enigo::new(&Settings::default()).context("opening X11 input backend")?;
        for chunk in chunk_text(text, self.chunk_chars) {
            enigo.text(chunk)?;
        }
        Ok(())
    }
}

// An error here enables the typing fallback. Once delivery succeeded, only
// warn about restore errors: returning an error would duplicate the text.
fn settle_paste(paste_result: Result<()>, restore_result: Result<()>) -> Result<()> {
    match (paste_result, restore_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(restore_error)) => {
            warn!("text was pasted, but clipboard restoration failed: {restore_error}");
            Ok(())
        }
        (Err(paste_error), Ok(())) => Err(paste_error),
        (Err(paste_error), Err(restore_error)) => Err(anyhow!(
            "paste failed: {paste_error}; clipboard restoration also failed: {restore_error}"
        )),
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
    fn successful_paste_restore_failure_never_enables_duplicate_typing() {
        assert!(settle_paste(Ok(()), Err(anyhow!("restore unavailable"))).is_ok());
    }

    #[test]
    fn failed_paste_enables_fallback_and_preserves_restore_failure_details() {
        assert_eq!(
            settle_paste(Err(anyhow!("no paste")), Ok(()))
                .unwrap_err()
                .to_string(),
            "no paste"
        );
        let error = settle_paste(Err(anyhow!("no paste")), Err(anyhow!("no restore")))
            .unwrap_err()
            .to_string();
        assert!(error.contains("no paste") && error.contains("no restore"));
    }

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
