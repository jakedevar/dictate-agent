//! A minimal protocol client for the control socket.
//!
//! Speaks only [`dictate_proto`]: NDJSON in, NDJSON out, responses
//! demultiplexed from the events that share the connection.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use dictate_proto::{
    ClientInfo, ClientKind, Command, CommandResult, Event, Hello, Message, ProtoError, RequestId,
    ServerHello,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;

/// Environment variable that overrides the socket location.
pub const SOCKET_ENV: &str = "DICTATE_SOCKET";

/// Resolve the daemon's control socket.
///
/// # Duplicated on purpose
///
/// This mirrors `dictated::paths::RuntimePaths::from_env`, which is the source
/// of truth. The CLI does **not** depend on the daemon crate: doing so would
/// drag `dictate-core` → `dictate-stt` → whisper.cpp and CUDA into a binary
/// that a keybinding runs on every dictation, trading a hundred milliseconds
/// of startup for ten lines of shared code. The socket path is a documented,
/// stable interface — like a well-known port — and `DICTATE_SOCKET` covers
/// anything unusual.
#[must_use]
pub fn socket_path() -> PathBuf {
    if let Some(explicit) = std::env::var_os(SOCKET_ENV) {
        return PathBuf::from(explicit);
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let uid = std::env::var("UID").unwrap_or_else(|_| "user".into());
            PathBuf::from(format!("/tmp/dictate-agent-{uid}"))
        });
    runtime.join("dictate-agent").join("dictated.sock")
}

/// A connected, handshaken client.
pub struct Client {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: OwnedWriteHalf,
    next_id: u64,
    pending: std::collections::VecDeque<Event>,
    /// What the daemon told us it is and what it will let us do.
    pub hello: ServerHello,
}

impl Client {
    /// Connect and handshake.
    ///
    /// # Errors
    ///
    /// If the daemon is not running, or refuses the protocol version.
    pub async fn connect(path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(path).await.with_context(|| {
            format!(
                "no daemon listening on {} — is dictated running? \
                 (systemctl --user status dictated)",
                path.display()
            )
        })?;
        let (read, write) = stream.into_split();
        let mut client = Self {
            reader: BufReader::new(read),
            writer: write,
            next_id: 1,
            pending: std::collections::VecDeque::new(),
            // Replaced by the handshake below; never observed unset.
            hello: ServerHello {
                protocol_version: dictate_proto::PROTOCOL_VERSION,
                supported_versions: Vec::new(),
                server: dictate_proto::ServerInfo::new("", ""),
                capabilities: dictate_proto::Capabilities::default(),
            },
        };

        let hello = Hello::new(ClientInfo {
            version: Some(env!("CARGO_PKG_VERSION").into()),
            ..ClientInfo::new("dictate", ClientKind::Cli)
        });
        match client.request(Command::Handshake(hello)).await? {
            CommandResult::Handshake(h) => client.hello = *h,
            other => bail!("expected a handshake response, got {}", other.name()),
        }
        Ok(client)
    }

    /// Connect to the default socket.
    ///
    /// # Errors
    ///
    /// As [`Client::connect`].
    pub async fn connect_default() -> Result<Self> {
        Self::connect(&socket_path()).await
    }

    async fn send(&mut self, message: &Message) -> Result<()> {
        let line = message.to_ndjson_line()?;
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn read_message(&mut self) -> Result<Message> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;
        if n == 0 {
            bail!("the daemon closed the connection");
        }
        // An unknown event or result degrades to `Unknown` rather than failing
        // — the compatibility rule for a client talking to a newer daemon.
        serde_json::from_str(&line).with_context(|| format!("unreadable frame: {line}"))
    }

    /// Send a command and wait for its response, buffering events meanwhile.
    ///
    /// # Errors
    ///
    /// The daemon's error, or a transport failure.
    pub async fn request(&mut self, command: Command) -> Result<CommandResult> {
        match self.try_request(command).await? {
            Ok(result) => Ok(result),
            Err(e) => bail!("{} ({})", e.message, e.code.as_str()),
        }
    }

    /// As [`Client::request`], but surfacing the protocol error rather than
    /// flattening it — the caller may want to react to a specific code.
    ///
    /// # Errors
    ///
    /// Transport failures only; a protocol error is the `Ok(Err(..))` arm.
    pub async fn try_request(
        &mut self,
        command: Command,
    ) -> Result<Result<CommandResult, ProtoError>> {
        let id = RequestId::Number(self.next_id);
        self.next_id += 1;
        self.send(&Message::request(id.clone(), command)).await?;

        loop {
            match self.read_message().await? {
                Message::Response(r) if r.id == id => {
                    return Ok(match r.outcome {
                        dictate_proto::Outcome::Result(v) => Ok(v),
                        dictate_proto::Outcome::Error(e) => Err(e),
                    })
                }
                Message::Event(e) => self.pending.push_back(e.event),
                Message::Response(_) => continue,
                Message::Request(_) => bail!("the daemon sent a request"),
            }
        }
    }

    /// The next event, from the buffer or the socket.
    ///
    /// # Errors
    ///
    /// If the connection drops.
    pub async fn next_event(&mut self) -> Result<Event> {
        if let Some(e) = self.pending.pop_front() {
            return Ok(e);
        }
        loop {
            match self.read_message().await? {
                Message::Event(e) => return Ok(e.event),
                _ => continue,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize env mutation: these tests share one process environment.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn the_socket_path_matches_the_daemons_documented_location() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: guarded by ENV_LOCK; no other thread reads the environment
        // concurrently in this test binary.
        unsafe {
            std::env::remove_var(SOCKET_ENV);
            std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
        }
        assert_eq!(
            socket_path(),
            PathBuf::from("/run/user/1000/dictate-agent/dictated.sock"),
            "this must stay in step with dictated::paths::RuntimePaths::from_env"
        );
    }

    #[test]
    fn the_socket_can_be_overridden() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            std::env::set_var(SOCKET_ENV, "/tmp/custom.sock");
        }
        assert_eq!(socket_path(), PathBuf::from("/tmp/custom.sock"));
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            std::env::remove_var(SOCKET_ENV);
        }
    }

    #[test]
    fn a_missing_runtime_dir_falls_back_to_a_per_user_tmp_path() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            std::env::remove_var(SOCKET_ENV);
            std::env::remove_var("XDG_RUNTIME_DIR");
            std::env::set_var("UID", "1000");
        }
        assert_eq!(
            socket_path(),
            PathBuf::from("/tmp/dictate-agent-1000/dictate-agent/dictated.sock")
        );
    }

    #[test]
    fn the_socket_is_never_the_python_daemons_pid_file() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let path = socket_path();
        assert!(!path.to_string_lossy().contains(".config"));
        assert!(path.ends_with("dictated.sock"));
    }
}
