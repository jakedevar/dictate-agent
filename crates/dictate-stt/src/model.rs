//! Reproducible GGUF model catalog and downloader.
//!
//! Catalog entries deliberately pin a Hugging Face *commit*, never `main`.
//! A completed file is SHA-256 verified before use. Interrupted pulls retain a
//! `.part` file and resume with HTTP Range; a checksum mismatch removes both
//! the completed file and partial state before one clean retry.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT_RANGES, CONTENT_RANGE, RANGE};
use sha2::{Digest, Sha256};

/// A reproducible model coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSpec {
    pub id: &'static str,
    pub repository: &'static str,
    pub revision: &'static str,
    pub filename: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
}

/// The intentionally small catalog required by this product.
///
/// `x-linked-etag` reported by Hugging Face for these LFS blobs is their
/// SHA-256. The revision is the immutable commit queried on 2026-08-19.
pub const CATALOG: &[ModelSpec] = &[
    ModelSpec {
        id: "large-v3-turbo",
        repository: "ggerganov/whisper.cpp",
        revision: "5359861c739e955e79d9a303bcbc70fb988958b1",
        filename: "ggml-large-v3-turbo.bin",
        sha256: "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
        bytes: 1_624_555_275,
    },
    ModelSpec {
        id: "tiny.en",
        repository: "ggerganov/whisper.cpp",
        revision: "5359861c739e955e79d9a303bcbc70fb988958b1",
        filename: "ggml-tiny.en.bin",
        sha256: "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f",
        bytes: 77_704_715,
    },
];

/// Return the pinned model metadata for a catalog ID.
pub fn catalog_model(id: &str) -> Option<&'static ModelSpec> {
    CATALOG.iter().find(|spec| spec.id == id)
}

/// Verified cached model summary used by `dictate model list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelStatus {
    pub spec: ModelSpec,
    pub path: PathBuf,
    pub present: bool,
    pub verified: bool,
    pub bytes_on_disk: Option<u64>,
}

/// A blocking downloader intended to run on the dedicated Whisper worker.
pub struct ModelManager {
    cache_dir: PathBuf,
    base_url: String,
    client: Client,
}

impl ModelManager {
    #[must_use]
    pub fn new(cache_dir: PathBuf) -> Self {
        Self::with_base_url(cache_dir, "https://huggingface.co")
    }

    /// Primarily useful to integration tests with a local HTTP server.
    #[must_use]
    pub fn with_base_url(cache_dir: PathBuf, base_url: impl Into<String>) -> Self {
        Self {
            cache_dir,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client: Client::builder()
                .connect_timeout(Duration::from_secs(15))
                // A production model is about 1.6 GB. Bound a stalled pull
                // without imposing a short interactive-request timeout.
                .timeout(Duration::from_secs(30 * 60))
                .build()
                .expect("reqwest client construction does not require I/O"),
        }
    }

    #[must_use]
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    #[must_use]
    pub fn path_for(&self, spec: &ModelSpec) -> PathBuf {
        self.cache_dir.join(spec.filename)
    }

    /// Report all catalog entries, hashing present files rather than trusting
    /// their names. A corrupt file is reported as such; `pull` deletes it.
    pub fn list(&self) -> Result<Vec<ModelStatus>> {
        CATALOG
            .iter()
            .map(|spec| {
                let path = self.path_for(spec);
                let metadata = fs::metadata(&path).ok();
                let present = metadata.is_some();
                let verified = present && verify_file(&path, spec.sha256)?;
                Ok(ModelStatus {
                    spec: *spec,
                    path,
                    present,
                    verified,
                    bytes_on_disk: metadata.map(|m| m.len()),
                })
            })
            .collect()
    }

    /// Verify or download an immutable catalog model.
    pub fn ensure(&self, id: &str) -> Result<PathBuf> {
        let spec = catalog_model(id).ok_or_else(|| anyhow!("unknown Whisper model '{id}'"))?;
        self.pull(spec)
    }

    /// Pull a particular pinned catalog entry, retrying once after corruption
    /// or transport failure. Existing partial bytes are resumed when the
    /// server honors Range; otherwise the server's complete response replaces
    /// the partial safely.
    pub fn pull(&self, spec: &ModelSpec) -> Result<PathBuf> {
        fs::create_dir_all(&self.cache_dir)
            .with_context(|| format!("creating model directory {}", self.cache_dir.display()))?;
        let target = self.path_for(spec);
        if target.exists() {
            if verify_file(&target, spec.sha256)? {
                return Ok(target);
            }
            fs::remove_file(&target)
                .with_context(|| format!("deleting corrupt model {}", target.display()))?;
        }

        let partial = partial_path(&target);
        let mut last_error = None;
        for attempt in 0..2 {
            match self.download_once(spec, &partial) {
                Ok(()) if verify_file(&partial, spec.sha256)? => {
                    fs::rename(&partial, &target).with_context(|| {
                        format!("committing verified model {}", target.display())
                    })?;
                    return Ok(target);
                }
                Ok(()) => {
                    let _ = fs::remove_file(&partial);
                    last_error = Some(anyhow!(
                        "SHA-256 mismatch after downloading {} (attempt {})",
                        spec.id,
                        attempt + 1
                    ));
                }
                Err(error) => {
                    // A transport failure may leave a useful partial. Keep it
                    // for Range resume on the retry, except after the final
                    // failure where an incomplete model must never be reused
                    // as a GGUF.
                    last_error = Some(error);
                }
            }
        }
        let _ = fs::remove_file(&partial);
        Err(last_error.unwrap_or_else(|| anyhow!("model pull failed without an error")))
    }

    fn download_once(&self, spec: &ModelSpec, partial: &Path) -> Result<()> {
        let offset = fs::metadata(partial).map(|m| m.len()).unwrap_or(0);
        let url = format!(
            "{}/{}/resolve/{}/{}",
            self.base_url, spec.repository, spec.revision, spec.filename
        );
        let mut request = self.client.get(url);
        if offset > 0 {
            request = request.header(RANGE, format!("bytes={offset}-"));
        }
        let mut response = request.send().context("requesting pinned GGUF")?;
        if !response.status().is_success() && response.status().as_u16() != 206 {
            bail!("model server returned HTTP {}", response.status());
        }

        let resumed = offset > 0
            && response.status().as_u16() == 206
            && response.headers().contains_key(CONTENT_RANGE);
        let _range_supported = response.headers().contains_key(ACCEPT_RANGES);
        let mut output = OpenOptions::new()
            .create(true)
            .write(true)
            .append(resumed)
            .truncate(!resumed)
            .open(partial)
            .with_context(|| format!("opening partial model {}", partial.display()))?;
        io::copy(&mut response, &mut output).context("writing model response")?;
        output.flush().context("flushing partial model")?;
        Ok(())
    }
}

fn partial_path(target: &Path) -> PathBuf {
    let mut name = target.as_os_str().to_os_string();
    name.push(".part");
    PathBuf::from(name)
}

/// Streaming SHA-256 verification, exported for offline/custom diagnostics.
pub fn verify_file(path: &Path, expected_sha256: &str) -> Result<bool> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 128];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()) == expected_sha256)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::thread;

    const TEST_SHA: &str = "b4655efd1a5b41901cc8f50b6d92472f717e05a548b7a6b6a549847d0ad559d0";

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("dictate-stt-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn catalog_is_immutable_and_has_complete_checksums() {
        for spec in CATALOG {
            assert_eq!(spec.revision.len(), 40);
            assert_eq!(spec.sha256.len(), 64);
            assert!(spec.bytes > 1_000_000);
            assert!(spec.repository.contains('/'));
        }
        assert_eq!(
            catalog_model("large-v3-turbo").unwrap().filename,
            "ggml-large-v3-turbo.bin"
        );
        assert!(catalog_model("made-up").is_none());
    }

    #[test]
    fn list_detects_a_corrupt_completed_model() {
        let dir = temp_dir("corrupt-list");
        let manager = ModelManager::new(dir.clone());
        fs::write(manager.path_for(&CATALOG[0]), b"not a GGUF").unwrap();
        let state = manager.list().unwrap();
        assert!(state[0].present);
        assert!(!state[0].verified);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn pull_resumes_partial_and_replaces_a_corrupt_target() {
        let body = Arc::new(b"dictate model test fixture".to_vec());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server_body = body.clone();
        let server = thread::spawn(move || {
            for _ in 0..1 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2048];
                let size = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..size]);
                let offset = request
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("range: bytes=")
                            .and_then(|v| v.strip_suffix('-'))
                            .map(str::to_owned)
                    })
                    .as_deref()
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0);
                let payload = &server_body[offset..];
                let status = if offset > 0 {
                    "206 Partial Content"
                } else {
                    "200 OK"
                };
                write!(stream, "HTTP/1.1 {status}\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {offset}-{} / {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", server_body.len() - 1, server_body.len(), payload.len()).unwrap();
                stream.write_all(payload).unwrap();
            }
        });
        let dir = temp_dir("resume");
        let spec = ModelSpec {
            id: "test",
            repository: "repo",
            revision: "0123456789012345678901234567890123456789",
            filename: "test.bin",
            sha256: TEST_SHA,
            bytes: body.len() as u64,
        };
        let manager = ModelManager::with_base_url(dir.clone(), format!("http://{addr}"));
        fs::write(manager.path_for(&spec), b"corrupt").unwrap();
        fs::write(partial_path(&manager.path_for(&spec)), &body[..8]).unwrap();
        let path = manager.pull(&spec).unwrap();
        assert_eq!(fs::read(path).unwrap(), *body);
        server.join().unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn checksum_verifier_rejects_wrong_content() {
        let dir = temp_dir("verify");
        let path = dir.join("model.bin");
        fs::write(&path, b"wrong").unwrap();
        assert!(!verify_file(&path, TEST_SHA).unwrap());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn pull_deletes_a_bad_download_then_retries_cleanly() {
        let body = Arc::new(b"dictate model test fixture".to_vec());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server_body = body.clone();
        let server = thread::spawn(move || {
            for response_body in [b"corrupt payload".as_slice(), server_body.as_slice()] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2048];
                let _ = stream.read(&mut request).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response_body.len()
                )
                .unwrap();
                stream.write_all(response_body).unwrap();
            }
        });
        let dir = temp_dir("retry");
        let spec = ModelSpec {
            id: "test",
            repository: "repo",
            revision: "0123456789012345678901234567890123456789",
            filename: "test.bin",
            sha256: TEST_SHA,
            bytes: body.len() as u64,
        };
        let manager = ModelManager::with_base_url(dir.clone(), format!("http://{addr}"));
        let path = manager.pull(&spec).unwrap();
        assert_eq!(fs::read(path).unwrap(), *body);
        assert!(!partial_path(&manager.path_for(&spec)).exists());
        server.join().unwrap();
        let _ = fs::remove_dir_all(dir);
    }
}
