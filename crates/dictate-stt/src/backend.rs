//! Observe the backend selected by whisper.cpp, rather than its requested
//! `use_gpu` flag. These messages are emitted by whisper_backend_init_gpu in
//! the pinned whisper-rs 0.16 / whisper.cpp build. CPU is initialized on every
//! successful context; it is the fallback when GPU discovery/init fails.
use std::cell::RefCell;
use std::ffi::{c_char, c_void, CStr};
use std::sync::Once;

use tracing::{debug, error, warn};

#[derive(Default)]
struct Evidence {
    backend: Option<String>,
    pending: String,
}

impl Evidence {
    fn observe(&mut self, fragment: &str) {
        // ggml may split a log message across callbacks. Parse complete lines.
        self.pending.push_str(fragment);
        while let Some(end) = self.pending.find('\n') {
            let line: String = self.pending.drain(..=end).collect();
            if let Some(message) = line.trim().strip_prefix("whisper_backend_init_gpu: ") {
                if message == "no GPU found" || message.starts_with("failed to initialize ") {
                    self.backend = Some("cpu".into());
                } else if let Some(device) = message
                    .strip_prefix("using ")
                    .and_then(|s| s.strip_suffix(" backend"))
                {
                    // CUDA device names are CUDA0, CUDA1, etc. CPU/host model
                    // buffers and enumerated devices alone prove nothing.
                    self.backend = Some(
                        if device.strip_prefix("CUDA").is_some_and(|id| {
                            !id.is_empty() && id.chars().all(|c| c.is_ascii_digit())
                        }) {
                            "cuda".into()
                        } else {
                            device.to_ascii_lowercase()
                        },
                    );
                }
            }
        }
        // Only diagnostic messages are expected here; bound unexpected logs.
        if self.pending.len() > 16_384 {
            self.pending.clear();
        }
    }
}

thread_local! {
    // Loads are synchronous on the provider's dedicated worker. Keeping
    // evidence thread-local prevents two providers from mixing their logs.
    static LOAD: RefCell<Option<Evidence>> = const { RefCell::new(None) };
}

unsafe extern "C" fn log_callback(level: u32, text: *const c_char, _: *mut c_void) {
    if text.is_null() {
        return;
    }
    // No panic may cross an FFI boundary, including one from a subscriber.
    let _ = std::panic::catch_unwind(|| {
        // SAFETY: whisper.cpp owns a NUL-terminated message for this callback.
        let message = unsafe { CStr::from_ptr(text) }.to_string_lossy();
        let _ = LOAD.try_with(|slot| {
            if let Ok(mut slot) = slot.try_borrow_mut() {
                if let Some(evidence) = slot.as_mut() {
                    evidence.observe(&message);
                }
            }
        });
        match whisper_rs::GGMLLogLevel::from(level) {
            whisper_rs::GGMLLogLevel::Error => error!(target: "whisper_cpp", "{}", message.trim()),
            whisper_rs::GGMLLogLevel::Warn => warn!(target: "whisper_cpp", "{}", message.trim()),
            _ => debug!(target: "whisper_cpp", "{}", message.trim()),
        }
    });
}

pub(crate) struct LoadObservation;
impl LoadObservation {
    pub(crate) fn start() -> Self {
        static INSTALL: Once = Once::new();
        INSTALL.call_once(|| {
            // SAFETY: process-lifetime callback, no user-data pointer, catches
            // unwinding; registered before any provider initializes whisper.
            unsafe {
                whisper_rs::set_log_callback(Some(log_callback), std::ptr::null_mut());
            }
        });
        LOAD.with(|slot| *slot.borrow_mut() = Some(Evidence::default()));
        Self
    }

    pub(crate) fn backend(&self) -> Option<String> {
        LOAD.with(|slot| slot.borrow().as_ref().and_then(|e| e.backend.clone()))
    }
}
impl Drop for LoadObservation {
    fn drop(&mut self) {
        LOAD.with(|slot| *slot.borrow_mut() = None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend(log: &str) -> Option<String> {
        let mut evidence = Evidence::default();
        evidence.observe(log);
        evidence.backend
    }

    #[test]
    fn requested_gpu_and_device_discovery_do_not_prove_cuda() {
        assert_eq!(backend("whisper_init_with_params_no_state: use gpu = 1\nggml_cuda_init: found 1 CUDA devices:\nwhisper_backend_init_gpu: device 0: CUDA0 (type: 1)\n"), None);
        assert_eq!(
            backend("whisper_model_load: CUDA_Host total size = 1.00 MB\n"),
            None
        );
    }

    #[test]
    fn successful_gpu_selection_reports_the_actual_device_family() {
        assert_eq!(
            backend("whisper_backend_init_gpu: using CUDA0 backend\n"),
            Some("cuda".into())
        );
        assert_eq!(
            backend("whisper_backend_init_gpu: using CUDA12 backend\n"),
            Some("cuda".into())
        );
        assert_eq!(
            backend("whisper_backend_init_gpu: using Vulkan0 backend\n"),
            Some("vulkan0".into())
        );
    }

    #[test]
    fn no_gpu_or_failed_gpu_initialization_reports_cpu() {
        assert_eq!(
            backend("whisper_backend_init_gpu: no GPU found\n"),
            Some("cpu".into())
        );
        assert_eq!(backend("whisper_backend_init_gpu: using CUDA0 backend\nwhisper_backend_init_gpu: failed to initialize CUDA0 backend\n"), Some("cpu".into()));
    }

    #[test]
    fn split_lines_and_repeated_state_initialization_use_latest_decision() {
        let mut evidence = Evidence::default();
        evidence.observe("whisper_backend_init_gpu: using CU");
        assert_eq!(evidence.backend, None);
        evidence.observe("DA0 backend\nwhisper_backend_init_gpu: no GPU found\n");
        assert_eq!(evidence.backend.as_deref(), Some("cpu"));
    }

    #[test]
    fn callbacks_on_other_threads_cannot_change_a_loads_backend() {
        let observation = LoadObservation::start();
        LOAD.with(|slot| {
            slot.borrow_mut()
                .as_mut()
                .unwrap()
                .observe("whisper_backend_init_gpu: no GPU found\n")
        });
        std::thread::spawn(|| {
            let other = LoadObservation::start();
            LOAD.with(|slot| {
                slot.borrow_mut()
                    .as_mut()
                    .unwrap()
                    .observe("whisper_backend_init_gpu: using CUDA0 backend\n")
            });
            assert_eq!(other.backend().as_deref(), Some("cuda"));
        })
        .join()
        .unwrap();
        assert_eq!(observation.backend().as_deref(), Some("cpu"));
    }
}
