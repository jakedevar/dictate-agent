use notify_rust::{Notification, Timeout, Urgency};
use tracing::error;

pub struct Notifier {
    enabled: bool,
    timeout_ms: u32,
    app_name: String,
    /// Notification ID for the active status indicator (recording/transcribing).
    /// Used so each status replaces the previous one, then gets closed when done.
    status_id: Option<u32>,
}

impl Notifier {
    pub fn new(config: &crate::config::NotificationConfig) -> Self {
        Self {
            enabled: config.enabled,
            timeout_ms: config.timeout_ms,
            app_name: "Dictate Agent".into(),
            status_id: None,
        }
    }

    /// Send a brief standalone notification (1.25s default).
    fn notify(&self, title: &str, message: &str, icon: &str, timeout_ms: Option<u32>) {
        if !self.enabled {
            return;
        }
        let timeout = timeout_ms.unwrap_or(self.timeout_ms);
        if let Err(e) = Notification::new()
            .appname(&self.app_name)
            .summary(title)
            .body(message)
            .icon(icon)
            .timeout(Timeout::Milliseconds(timeout))
            .urgency(Urgency::Normal)
            .show()
        {
            error!("Notification failed: {}", e);
        }
    }

    /// Show a persistent status notification that replaces any previous status.
    /// Stays visible until replaced by another status or explicitly closed.
    fn show_status(&mut self, title: &str, message: &str, icon: &str) {
        if !self.enabled {
            return;
        }

        let mut n = Notification::new();
        n.appname(&self.app_name)
            .summary(title)
            .body(message)
            .icon(icon)
            .timeout(Timeout::Never)
            .urgency(Urgency::Normal);

        // Replace the previous status notification if one exists
        if let Some(id) = self.status_id {
            n.id(id);
        }

        match n.show() {
            Ok(handle) => {
                self.status_id = Some(handle.id());
            }
            Err(e) => {
                error!("Notification failed: {}", e);
            }
        }
    }

    /// Close the active status notification (recording/transcribing).
    pub fn clear_status(&mut self) {
        if let Some(id) = self.status_id.take() {
            // Replace with a 1ms empty notification to dismiss it instantly
            let _ = Notification::new()
                .appname(&self.app_name)
                .id(id)
                .summary("")
                .body("")
                .timeout(Timeout::Milliseconds(1))
                .urgency(Urgency::Low)
                .show();
        }
    }

    /// Recording started — persistent, replaced by transcribing or closed on cancel.
    pub fn recording(&mut self) {
        self.show_status("Recording...", "Speak now", "media-record");
    }

    /// Transcription in progress — replaces the recording notification, persistent.
    pub fn transcribing(&mut self) {
        self.show_status(
            "Transcribing...",
            "Processing audio",
            "media-playback-start",
        );
    }

    /// Local model processing — 1.25s auto-dismiss.
    pub fn processing(&self, model: &str) {
        self.notify(
            "Processing...",
            &format!("Using {}", model),
            "system-run",
            None,
        );
    }

    /// Error — 10s display, auto-copies the full error message to clipboard.
    pub fn error(&self, message: &str) {
        if !self.enabled {
            return;
        }

        // Auto-copy full error message to clipboard
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let _ = clipboard.set_text(message);
        }

        let display = if message.len() > 100 {
            &message[..100]
        } else {
            message
        };
        if let Err(e) = Notification::new()
            .appname(&self.app_name)
            .summary("Error")
            .body(display)
            .icon("dialog-error")
            .timeout(Timeout::Milliseconds(10000))
            .urgency(Urgency::Normal)
            .show()
        {
            error!("Notification failed: {}", e);
        }
    }

    /// No speech detected — clears status, then 1.25s auto-dismiss.
    pub fn no_speech(&mut self) {
        self.clear_status();
        self.notify(
            "No Speech",
            "No speech detected in recording",
            "dialog-warning",
            None,
        );
    }

    /// Recording cancelled — clears status, then 1.25s auto-dismiss.
    pub fn cancelled(&mut self) {
        self.clear_status();
        self.notify("Cancelled", "Recording discarded", "dialog-cancel", None);
    }

    /// Timer successfully set — 1.25s auto-dismiss.
    pub fn timer_set(&self, message: &str) {
        self.notify("Timer Set", message, "alarm-symbolic", None);
    }
}
