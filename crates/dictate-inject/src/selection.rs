//! Selection-only editing. Copy is verified before paste; failures never fall
//! back to typing, deleting text, or leaving a response in the clipboard.
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use arboard::{Clipboard, GetExtLinux, LinuxClipboardKind};
use dictate_proto::{ErrorCode, InjectMethod, InjectionOutcome, ProtoError};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt;

use crate::clipboard::PasteSelection;
use crate::X11Injector;

/// A copy of selected text bound to its input window. Never logged.
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

/// Snapshot focus without activating, raising, or discovering window titles.
pub fn focused_window() -> Result<u32> {
    let (conn, _) = x11rb::connect(None)?;
    let focus = conn.get_input_focus()?.reply()?.focus;
    if focus <= 1 {
        bail!("no application has input focus");
    }
    Ok(focus)
}

fn check_focus(window: u32) -> Result<()> {
    if focused_window()? != window {
        bail!("edit destination changed; selection left untouched");
    }
    Ok(())
}

/// Copy from the active widget, using an ownership marker to distinguish a
/// fresh Ctrl+C from stale clipboard contents. PRIMARY is only accepted when
/// confirmed by that copy: a stale PRIMARY from another widget is unsafe.
fn copy_selection(
    clipboard: &mut Clipboard,
    marker: &PasteSelection,
    window: u32,
) -> (Result<String>, Option<u32>) {
    let mut copied_owner = None;
    let result = (|| {
        check_focus(window)?;
        let (probe, _) = x11rb::connect(None)?;
        let primary_owner = probe
            .get_selection_owner(x11rb::protocol::xproto::AtomEnum::PRIMARY.into())?
            .reply()?
            .owner;
        let client_mask = !probe.setup().resource_id_mask;
        let primary =
            if primary_owner != 0 && (primary_owner & client_mask) == (window & client_mask) {
                clipboard
                    .get()
                    .clipboard(LinuxClipboardKind::Primary)
                    .text()
                    .ok()
            } else {
                None
            };
        if !marker.send_copy(window)? {
            bail!("edit destination changed; selection left untouched");
        }
        let (conn, _) = x11rb::connect(None)?;
        let atom = conn.intern_atom(false, b"CLIPBOARD")?.reply()?.atom;
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            check_focus(window)?;
            let owner = conn.get_selection_owner(atom)?.reply()?.owner;
            let client_mask = !conn.setup().resource_id_mask;
            if owner != 0
                && owner != marker.window()
                && (owner & client_mask) == (window & client_mask)
            {
                copied_owner = Some(owner);
                break;
            }
            if Instant::now() >= deadline {
                bail!("no selected text was copied");
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let copied = clipboard.get_text().context("reading selected text")?;
        if copied.trim().is_empty() {
            bail!("selection is empty");
        }
        if copied.len() > 65_536 {
            bail!("selection exceeds 64 KiB edit limit");
        }
        if Some(conn.get_selection_owner(atom)?.reply()?.owner) != copied_owner {
            bail!("clipboard changed while reading selection");
        }
        check_focus(window)?;
        if primary.as_ref().is_some_and(|text| text != &copied) {
            bail!("PRIMARY and focused copy disagree; selection left untouched");
        }
        Ok(copied)
    })();
    // Caller owns restoration even on every error above.
    (result, copied_owner)
}

fn capture_selection(window: u32) -> Result<Selection> {
    check_focus(window)?;
    let mut clipboard = Clipboard::new()?;
    let mut marker = PasteSelection::new("")?;
    let (owner, saved) = marker.snapshot()?;
    marker.claim(owner)?;
    let (result, copied_owner) = copy_selection(&mut clipboard, &marker, window);
    let restored = marker.restore_after_copy(saved, copied_owner);
    marker.retain();
    restored?;
    result.map(|text| Selection { text, window })
}

impl X11Injector {
    pub fn capture_selection_blocking(&self, window: u32) -> Result<Selection> {
        if !self.enabled || self.default_policy() != crate::InjectionPolicy::Paste {
            bail!("selection editing requires enabled clipboard-paste injection");
        }
        let _guard = self
            .clipboard
            .lock()
            .map_err(|_| anyhow!("clipboard lock poisoned"))?;
        capture_selection(window)
    }

    /// One paste, after verifying the same widget still has the same selected
    /// bytes. No select-all, delete, focus change, retry, or typing fallback.
    pub fn replace_selection_blocking(
        &self,
        selection: &Selection,
        text: &str,
    ) -> InjectionOutcome {
        let result = self.replace_selection(selection, text);
        match result {
            Ok(()) => InjectionOutcome::Injected {
                method: InjectMethod::Paste,
                chars: text.chars().count() as u32,
            },
            Err(error) => InjectionOutcome::Failed {
                error: ProtoError::new(ErrorCode::InjectionFailed, error.to_string()),
            },
        }
    }

    fn replace_selection(&self, selection: &Selection, text: &str) -> Result<()> {
        if !self.enabled || self.default_policy() != crate::InjectionPolicy::Paste {
            bail!("selection editing requires enabled clipboard-paste injection");
        }
        if text.trim().is_empty() {
            bail!("empty edit output rejected");
        }
        // Allocate/validate the paste payload before sending any input.
        let mut paste = PasteSelection::new(text)?;
        let _guard = self
            .clipboard
            .lock()
            .map_err(|_| anyhow!("clipboard lock poisoned"))?;
        let current = capture_selection(selection.window)?;
        if current != *selection {
            bail!("selected text changed; edit discarded");
        }
        check_focus(selection.window)?;
        let (owner, saved) = paste.snapshot()?;
        paste.claim(owner)?;
        match paste.send_paste(selection.window) {
            Ok(false) => {
                // The atomic focus check prevented all keys.
                paste.restore(saved)?;
                paste.retain();
                bail!("edit destination changed; selection left untouched");
            }
            sent => {
                // A failed send may still have queued Ctrl+V. Never retry.
                let result = sent.and_then(|_| paste.transfer_to(selection.window));
                let restored = paste.restore(saved);
                if result.is_ok() {
                    if let Err(error) = restored {
                        tracing::warn!("edit pasted but clipboard restoration failed: {error}");
                    }
                    paste.retain();
                } else {
                    // Preserve the prior clipboard and deny delayed requests
                    // from the EDIT client, rather than pasting stale contents.
                    paste.retain_aborted_edit(selection.window);
                    restored?;
                }
                result
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InjectionPolicy, OutputConfig};

    #[test]
    fn disabled_or_nonpaste_backends_never_read_or_replace_selections() {
        for (auto_type, policy) in [
            (false, InjectionPolicy::Paste),
            (true, InjectionPolicy::Off),
            (true, InjectionPolicy::Type),
        ] {
            let injector = X11Injector::new(&OutputConfig {
                auto_type,
                policy,
                ..Default::default()
            });
            assert!(injector.capture_selection_blocking(42).is_err());
            assert!(!injector
                .replace_selection_blocking(
                    &Selection {
                        text: "synthetic selection".into(),
                        window: 42
                    },
                    "replacement"
                )
                .did_inject());
        }
    }
}
