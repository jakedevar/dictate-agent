use notify_rust::{Notification, Timeout};
use tracing::error;

pub struct Notifier {
    enabled: bool,
    timeout_ms: u32,
    app_name: String,
}

impl Notifier {
    pub fn new(config: &crate::config::NotificationConfig) -> Self {
        Self {
            enabled: config.enabled,
            timeout_ms: config.timeout_ms,
            app_name: "Dictate Agent".into(),
        }
    }

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
            .show()
        {
            error!("Notification failed: {}", e);
        }
    }

    pub fn recording(&self) {
        self.notify("Recording...", "Speak now", "media-record", Some(30000));
    }

    pub fn transcribing(&self) {
        self.notify(
            "Transcribing...",
            "Processing audio",
            "media-playback-start",
            Some(30000),
        );
    }

    pub fn processing(&self, model: &str) {
        self.notify(
            "Processing...",
            &format!("Using {}", model),
            "system-run",
            Some(30000),
        );
    }

    pub fn done(&self, text: &str) {
        let display = if text.len() > 100 {
            &text[..100]
        } else {
            text
        };
        self.notify("Done", display, "dialog-ok", None);
    }

    pub fn error(&self, message: &str) {
        let display = if message.len() > 100 {
            &message[..100]
        } else {
            message
        };
        self.notify("Error", display, "dialog-error", Some(5000));
    }

    pub fn no_speech(&self) {
        self.notify(
            "No Speech",
            "No speech detected in recording",
            "dialog-warning",
            Some(2000),
        );
    }

    pub fn cancelled(&self) {
        self.notify(
            "Cancelled",
            "Recording discarded",
            "dialog-cancel",
            Some(2000),
        );
    }

    pub fn timer_set(&self, message: &str) {
        self.notify("Timer Set", message, "alarm-symbolic", None);
    }
}
