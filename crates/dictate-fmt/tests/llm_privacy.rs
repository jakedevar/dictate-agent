//! Isolated tracing subscriber: one test process prevents parallel tests from
//! racing tracing's callsite interest cache while we assert privacy of logs.
use dictate_fmt::llm::client::{BoxFuture, InstalledModel};
use dictate_fmt::llm::{
    BackendError, ChatBackend, ChatRequest, ChatResponse, LlmConfig, LlmFormatter, LlmRequest,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct RejectingBackend(Option<BackendError>);
impl ChatBackend for RejectingBackend {
    fn chat<'a>(&'a self, _: &'a ChatRequest) -> BoxFuture<'a, Result<ChatResponse, BackendError>> {
        Box::pin(async {
            if let Some(error) = &self.0 {
                return Err(error.clone());
            }
            Ok(ChatResponse {
                content: "Please check the logs.".into(),
                done_reason: Some("stop".into()),
                ..Default::default()
            })
        })
    }
    fn list_models(&self) -> BoxFuture<'_, Result<Vec<InstalledModel>, BackendError>> {
        Box::pin(async {
            Ok(vec![InstalledModel {
                name: "gemma4:e4b".into(),
                family: String::new(),
                size: 0,
            }])
        })
    }
    fn load<'a>(&'a self, _: &'a str, _: &'a str) -> BoxFuture<'a, Result<Duration, BackendError>> {
        Box::pin(async { Ok(Duration::ZERO) })
    }
}

#[tokio::test]
async fn private_rejection_logs_no_dictated_words() {
    #[derive(Clone)]
    struct LogWriter(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let log = Arc::new(Mutex::new(Vec::new()));
    let writer = LogWriter(log.clone());
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();
    let req = LlmRequest {
        private: true,
        ..LlmRequest::new("please check the logs for syntheticsecret")
    };
    for error in [
        None,
        Some(BackendError::Http {
            status: 500,
            message: "syntheticsecret".into(),
        }),
        Some(BackendError::Malformed("syntheticsecret".into())),
        Some(BackendError::Unreachable("syntheticsecret".into())),
        Some(BackendError::ModelMissing("syntheticsecret".into())),
    ] {
        let rejection_expected = error.is_none();
        let f = LlmFormatter::with_backend(
            LlmConfig {
                enabled: true,
                ..Default::default()
            },
            Arc::new(RejectingBackend(error)),
        );
        let out = f.format(&req).await;
        assert_eq!(out.text, req.text);
        assert_eq!(out.validator_rejection.is_some(), rejection_expected);
        assert!(!out.error.unwrap().contains("syntheticsecret"));
        assert!(!f.health().summary().contains("syntheticsecret"));
    }
    let logged = String::from_utf8(log.lock().unwrap().clone()).unwrap();
    assert!(logged.contains("LLM pass failed open"), "{logged}");
    assert!(!logged.contains("syntheticsecret"), "{logged}");
    assert!(!logged.contains(&req.text), "{logged}");
}
