//! API-only hosts never send desktop notifications or change the clipboard.
pub struct Notifier;
impl Notifier {
    pub fn new(_: &crate::config::NotificationConfig) -> Self {
        Self
    }
    pub fn clear_status(&mut self) {}
    pub fn recording(&mut self) {}
    pub fn transcribing(&mut self) {}
    pub fn processing(&self, _: &str) {}
    pub fn note_saved(&self, _: &str) {}
    pub fn edit_preview(&self, _: &str) {}
    pub fn edit_error(&self, _: &str) {}
    pub fn error(&self, _: &str) {}
    pub fn injection_failed(&self, _: &str) {}
    pub fn no_speech(&mut self) {}
    pub fn cancelled(&mut self) {}
    pub fn microphone_muted(&self) {}
    pub fn timer_set(&self, _: &str) {}
}
