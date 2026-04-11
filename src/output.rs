use anyhow::Result;
use arboard::Clipboard;
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use tracing::{error, info};

pub struct OutputHandler {
    auto_type: bool,
}

impl OutputHandler {
    pub fn new(config: &crate::config::OutputConfig) -> Self {
        Self {
            auto_type: config.auto_type,
        }
    }

    /// Type text by setting clipboard and simulating Ctrl+V.
    /// Saves and restores the previous clipboard content.
    /// Returns true on success.
    pub fn type_text(&self, text: &str) -> bool {
        if text.trim().is_empty() || !self.auto_type {
            return false;
        }

        let result = self.type_text_inner(text);
        match &result {
            Ok(()) => {
                info!("Typed {} characters", text.len());
                true
            }
            Err(e) => {
                error!("Failed to type text: {}", e);
                false
            }
        }
    }

    fn type_text_inner(&self, text: &str) -> Result<()> {
        let mut clipboard = Clipboard::new()?;

        // Save current clipboard (best effort) — matches output.py:41-49
        let saved = clipboard.get_text().ok();

        // Set clipboard to our text
        clipboard.set_text(text.trim())?;

        // Simulate Ctrl+V paste — matches output.py:57-60 (xdotool key --clearmodifiers ctrl+v)
        let mut enigo = Enigo::new(&Settings::default())?;
        enigo.key(Key::Control, Direction::Press)?;
        enigo.key(Key::Unicode('v'), Direction::Click)?;
        enigo.key(Key::Control, Direction::Release)?;

        // Wait for paste to land — matches output.py:63 (time.sleep(0.05))
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Restore previous clipboard (best effort) — matches output.py:66-73
        if let Some(saved) = saved {
            let _ = clipboard.set_text(&saved);
        }

        Ok(())
    }
}
