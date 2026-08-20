use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use arboard::{Clipboard, Error as ClipboardError};
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
#[derive(Debug, Clone)]
pub struct X11Injector {
    enabled: bool,
    default_policy: InjectionPolicy,
    chunk_chars: usize,
}

impl X11Injector {
    #[must_use]
    pub fn new(config: &crate::config::OutputConfig) -> Self {
        Self {
            enabled: config.auto_type,
            default_policy: config.policy,
            chunk_chars: config.type_chunk_chars.max(1),
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

    /// Save text before changing it and restore it after both a successful and
    /// a failed paste. `ContentNotAvailable` means an empty text clipboard;
    /// it is restored as empty rather than treated as an un-restorable state.
    fn paste_transaction(&self, text: &str) -> Result<()> {
        let mut clipboard = Clipboard::new().context("opening clipboard")?;
        let saved = match clipboard.get_text() {
            Ok(value) => value,
            Err(ClipboardError::ContentNotAvailable) => String::new(),
            Err(e) => return Err(anyhow!(e)).context("saving clipboard text"),
        };
        clipboard
            .set_text(text.to_owned())
            .context("setting dictation text on clipboard")?;
        let paste_result = self.send_paste();
        std::thread::sleep(Duration::from_millis(50));
        let restore_result = clipboard
            .set_text(saved)
            .context("restoring clipboard text");
        paste_result.and(restore_result)
    }

    fn send_paste(&self) -> Result<()> {
        let mut enigo = Enigo::new(&Settings::default()).context("opening X11 input backend")?;
        enigo.key(Key::Control, Direction::Press)?;
        let result = enigo.key(Key::Unicode('v'), Direction::Click);
        // Always release Ctrl, including a failed click, to avoid a stuck modifier.
        let release = enigo.key(Key::Control, Direction::Release);
        result?;
        release?;
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
}
