//! Selection-only editing. Copy is verified before paste; failures never fall
//! back to typing, deleting text, or leaving a response in the clipboard.
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use arboard::{Clipboard, GetExtLinux, LinuxClipboardKind};
use dictate_proto::{ErrorCode, InjectMethod, InjectionOutcome, ProtoError};
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt;

use crate::clipboard::{PasteSelection, Snapshot};
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
fn copy_selection(clipboard: &mut Clipboard, window: u32) -> Result<String> {
    check_focus(window)?;
    let (probe, _) = x11rb::connect(None)?;
    let primary_owner = probe
        .get_selection_owner(x11rb::protocol::xproto::AtomEnum::PRIMARY.into())?
        .reply()?
        .owner;
    let client_mask = !probe.setup().resource_id_mask;
    let primary = if primary_owner != 0 && (primary_owner & client_mask) == (window & client_mask) {
        clipboard
            .get()
            .clipboard(LinuxClipboardKind::Primary)
            .text()
            .ok()
    } else {
        None
    };
    let marker = PasteSelection::new("")?;
    marker.claim()?;
    let result = (|| {
        check_focus(window)?;
        let mut keys = Enigo::new(&Settings::default()).context("opening copy input")?;
        keys.key(Key::Control, Direction::Press)?;
        let copy = keys.key(Key::Unicode('c'), Direction::Click);
        let release = keys.key(Key::Control, Direction::Release);
        copy?;
        release?;
        let (conn, _) = x11rb::connect(None)?;
        let atom = conn.intern_atom(false, b"CLIPBOARD")?.reply()?.atom;
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            check_focus(window)?;
            let owner = conn.get_selection_owner(atom)?.reply()?.owner;
            let client_mask = !conn.setup().resource_id_mask;
            if owner != 0
                && owner != marker.owner()
                && (owner & client_mask) == (window & client_mask)
            {
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
        check_focus(window)?;
        if primary.as_ref().is_some_and(|text| text != &copied) {
            bail!("PRIMARY and focused copy disagree; selection left untouched");
        }
        Ok(copied)
    })();
    // Caller owns restoration even on every error above.
    result
}

impl X11Injector {
    pub fn capture_selection_blocking(&self, window: u32) -> Result<Selection> {
        if !self.enabled || self.default_policy() != crate::InjectionPolicy::Paste {
            bail!("selection editing requires enabled clipboard-paste injection");
        }
        let mut guard = self
            .clipboard
            .lock()
            .map_err(|_| anyhow!("clipboard lock poisoned"))?;
        if guard.is_none() {
            *guard = Some(Clipboard::new()?);
        }
        let clipboard = guard.as_mut().expect("initialized above");
        let saved = Snapshot::capture(clipboard)?;
        let result = copy_selection(clipboard, window);
        saved.restore(clipboard)?;
        result.map(|text| Selection { text, window })
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
        let paste = PasteSelection::new(text)?;
        let mut guard = self
            .clipboard
            .lock()
            .map_err(|_| anyhow!("clipboard lock poisoned"))?;
        if guard.is_none() {
            *guard = Some(Clipboard::new()?);
        }
        let clipboard = guard.as_mut().expect("initialized above");
        let saved = Snapshot::capture(clipboard)?;
        let result = (|| {
            let current = copy_selection(clipboard, selection.window)?;
            if current != selection.text {
                bail!("selected text changed; edit discarded");
            }
            check_focus(selection.window)?;
            paste.claim()?;
            check_focus(selection.window)?;
            self.send_paste()?;
            paste.transfer_to(text, selection.window)
        })();
        let restored = saved.restore(clipboard);
        match (result, restored) {
            (Ok(()), Err(error)) => {
                // Delivery has already occurred. Never retry or claim that the
                // selection was unchanged after a restoration-only failure.
                tracing::warn!("edit pasted but clipboard restoration failed: {error}");
                Ok(())
            }
            (result, Ok(())) => result,
            (Err(error), Err(restore)) => {
                Err(anyhow!("{error}; clipboard restoration failed: {restore}"))
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
