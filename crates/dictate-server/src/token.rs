//! The bearer token: issuance, storage, rotation, verification (design §5).
//!
//! # Rules
//!
//! - 256 bits from the OS CSPRNG, as `dct1_` + 43 base64url characters.
//! - Lives in its own file, never in `config.toml` (which `get_config` serves).
//!   The file must be a regular file — not a symlink — owned by this user, with
//!   no group or other permission bits; anything else is refused, the way
//!   `ssh` refuses a readable private key.
//! - The daemon never writes it on startup. `dictated --api-token` creates it;
//!   `dictated --rotate-api-token` replaces it. Both write a fresh 0600 file
//!   beside the target and `rename` it into place.
//! - A running server re-reads the file when its identity (device, inode,
//!   size, mtime, ctime, mode, owner) changes, so rotation applies to the next
//!   request, deleting the file revokes immediately, and a file made readable
//!   by others stops working. Every failure is "reject".
//! - Comparison is of SHA-256 digests, in constant time: neither the length
//!   nor any prefix of the stored token leaks through timing.
//! - Nothing here ever logs or formats a token. [`TokenError`] names files and
//!   reasons, never contents.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::Engine as _;
use sha2::{Digest, Sha256};

/// Prefix of every token this build issues. Versions the format and lets
/// secret scanners recognize a leaked token.
pub const TOKEN_PREFIX: &str = "dct1_";
/// Random bytes in a token.
const TOKEN_BYTES: usize = 32;
/// `TOKEN_PREFIX` + unpadded base64url of `TOKEN_BYTES`.
const TOKEN_LEN: usize = TOKEN_PREFIX.len() + 43;
/// Refuse to read a token file larger than this (it is one line).
const MAX_FILE_BYTES: u64 = 256;

/// Why a token file could not be used.
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    /// No file at the path.
    #[error("no API token at {0}; create one with `dictated --api-token`")]
    Missing(PathBuf),
    /// A symlink, directory, or other non-regular file.
    #[error("{0} is not a regular file (symlinks are refused)")]
    NotRegular(PathBuf),
    /// Owned by someone else.
    #[error("{0} is owned by uid {1}, not by this user")]
    WrongOwner(PathBuf, u32),
    /// Group or other may access it.
    #[error(
        "{0} is accessible by other users (mode {1:o}); run `chmod 600` on it or rotate the token"
    )]
    Insecure(PathBuf, u32),
    /// Not a token this build issues.
    #[error("{0} does not hold a valid API token; rotate it with `dictated --rotate-api-token`")]
    Malformed(PathBuf),
    /// The OS random source failed.
    #[error("the OS random number generator failed: {0}")]
    Random(String),
    /// Any other I/O failure.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// What failed.
        source: std::io::Error,
    },
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> TokenError + '_ {
    move |source| TokenError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// A fresh token from the OS CSPRNG.
///
/// # Errors
///
/// If the OS random source fails.
pub fn generate() -> Result<String, TokenError> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::getrandom(&mut bytes).map_err(|e| TokenError::Random(e.to_string()))?;
    Ok(format!(
        "{TOKEN_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    ))
}

/// Whether `token` has the shape this build issues.
#[must_use]
pub fn is_well_formed(token: &str) -> bool {
    token.len() == TOKEN_LEN
        && token.starts_with(TOKEN_PREFIX)
        && base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&token[TOKEN_PREFIX.len()..])
            .is_ok_and(|b| b.len() == TOKEN_BYTES)
}

/// Read the token at `path`, creating one if there is none. Returns the token
/// and whether it was just created. Backs `dictated --api-token`.
///
/// # Errors
///
/// If an existing file is insecure or malformed (it is never overwritten
/// silently), or the file cannot be written.
pub fn ensure(path: &Path) -> Result<(String, bool), TokenError> {
    match read_secure(path) {
        Ok(token) => Ok((token, false)),
        Err(TokenError::Missing(_)) => {
            let token = generate()?;
            write_secure(path, &token)?;
            Ok((token, true))
        }
        Err(e) => Err(e),
    }
}

/// Replace the token at `path` with a fresh one. Backs
/// `dictated --rotate-api-token`; a running server honors it on the next
/// request.
///
/// # Errors
///
/// If the file cannot be written.
pub fn rotate(path: &Path) -> Result<String, TokenError> {
    let token = generate()?;
    write_secure(path, &token)?;
    Ok(token)
}

/// Read and validate the token file.
///
/// # Errors
///
/// Every way the file can be unusable; see [`TokenError`].
pub fn read_secure(path: &Path) -> Result<String, TokenError> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(TokenError::Missing(path.to_path_buf()))
        }
        // O_NOFOLLOW on a symlink fails with ELOOP.
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            return Err(TokenError::NotRegular(path.to_path_buf()))
        }
        Err(e) => return Err(io(path)(e)),
    };
    // Checked on the opened descriptor, so the file inspected is the file read.
    let meta = file.metadata().map_err(io(path))?;
    check_metadata(path, &meta)?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(TokenError::Malformed(path.to_path_buf()));
    }
    let mut contents = String::new();
    file.take(MAX_FILE_BYTES)
        .read_to_string(&mut contents)
        .map_err(|_| TokenError::Malformed(path.to_path_buf()))?;
    let token = contents.trim();
    if !is_well_formed(token) {
        return Err(TokenError::Malformed(path.to_path_buf()));
    }
    Ok(token.to_string())
}

fn check_metadata(path: &Path, meta: &std::fs::Metadata) -> Result<(), TokenError> {
    if !meta.file_type().is_file() {
        return Err(TokenError::NotRegular(path.to_path_buf()));
    }
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    if meta.uid() != me {
        return Err(TokenError::WrongOwner(path.to_path_buf(), meta.uid()));
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(TokenError::Insecure(path.to_path_buf(), mode));
    }
    Ok(())
}

/// Write `token` to `path`: a new 0600 file beside it, synced, then renamed
/// over it. The directory is created owner-only if it does not exist.
fn write_secure(path: &Path, token: &str) -> Result<(), TokenError> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !dir.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(io(dir))?;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "api-token".into());
    let mut suffix = [0u8; 8];
    getrandom::getrandom(&mut suffix).map_err(|e| TokenError::Random(e.to_string()))?;
    let tmp = dir.join(format!(
        ".{name}.{}.tmp",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(suffix)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&tmp)
            .map_err(io(&tmp))?;
        // The umask cannot widen 0600, but be explicit about the result.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(io(&tmp))?;
        file.write_all(format!("{token}\n").as_bytes())
            .map_err(io(&tmp))?;
        file.sync_all().map_err(io(&tmp))?;
        std::fs::rename(&tmp, path).map_err(io(path))?;
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// What identifies one version of the token file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: (i64, i64),
    // A `chmod` or `chown` changes only these, and an insecure file must stop
    // working as soon as it becomes insecure.
    ctime: (i64, i64),
    mode: u32,
    uid: u32,
}

impl Stamp {
    fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mtime: (meta.mtime(), meta.mtime_nsec()),
            ctime: (meta.ctime(), meta.ctime_nsec()),
            mode: meta.mode(),
            uid: meta.uid(),
        }
    }
}

/// The digest the server compares against, reloaded when the file changes.
struct Loaded {
    stamp: Option<Stamp>,
    digest: Option<[u8; 32]>,
}

/// What a connection keeps of the token it authenticated with: the digest,
/// never the token, and never printed.
#[derive(Clone, Copy)]
pub struct Credential([u8; 32]);

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credential(..)")
    }
}

/// The server's view of the token file.
pub struct TokenStore {
    path: PathBuf,
    loaded: Mutex<Loaded>,
}

impl std::fmt::Debug for TokenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the digest, let alone the token.
        f.debug_struct("TokenStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl TokenStore {
    /// Open the token file the server will authenticate against.
    ///
    /// # Errors
    ///
    /// If the file is missing or unusable: the API refuses to start rather
    /// than run without a credential (design §4, rule 7).
    pub fn open(path: &Path) -> Result<Self, TokenError> {
        let token = read_secure(path)?;
        let stamp = std::fs::symlink_metadata(path).ok().map(|m| Stamp::of(&m));
        Ok(Self {
            path: path.to_path_buf(),
            loaded: Mutex::new(Loaded {
                stamp,
                digest: Some(digest(&token)),
            }),
        })
    }

    /// The token file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether `presented` is the current token. Fails closed: a deleted,
    /// replaced-with-garbage, or newly insecure file rejects everything.
    pub fn verify(&self, presented: &str) -> bool {
        self.authenticate(presented).is_some()
    }

    /// A [`Credential`] for `presented` if it is the current token.
    #[must_use]
    pub fn authenticate(&self, presented: &str) -> Option<Credential> {
        let credential = Credential(digest(presented));
        self.still_valid(&credential).then_some(credential)
    }

    /// Whether `credential` is still the current token: `false` once the file
    /// is rotated, deleted, or made insecure. A connection that outlives its
    /// request (a WebSocket) asks this before every command, so revoking the
    /// token revokes the connections opened with it.
    #[must_use]
    pub fn still_valid(&self, credential: &Credential) -> bool {
        let Some(expected) = self.current() else {
            return false;
        };
        constant_time_eq(&credential.0, &expected)
    }

    /// The digest of the file as it is now, reloading if it changed.
    fn current(&self) -> Option<[u8; 32]> {
        let now = std::fs::symlink_metadata(&self.path)
            .ok()
            .map(|m| Stamp::of(&m));
        let mut loaded = self.loaded.lock().ok()?;
        if now.is_none() {
            loaded.stamp = None;
            loaded.digest = None;
            return None;
        }
        if now != loaded.stamp {
            loaded.stamp = now;
            loaded.digest = match read_secure(&self.path) {
                Ok(token) => {
                    tracing::info!(file = %self.path.display(), "API token file changed; reloaded");
                    Some(digest(&token))
                }
                Err(e) => {
                    tracing::warn!("API token unusable, rejecting every request: {e}");
                    None
                }
            };
        }
        loaded.digest
    }
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// Compare two digests without an early exit.
fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let diff = a
        .iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

/// SHA-256 fingerprint of a DER certificate, as colon-separated hex, for a
/// client to pin.
#[must_use]
pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "dictate-server-token-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_token_is_prefixed_256_bits_and_never_repeats() {
        let a = generate().unwrap();
        let b = generate().unwrap();
        assert!(a.starts_with(TOKEN_PREFIX));
        assert_eq!(a.len(), TOKEN_LEN);
        assert!(is_well_formed(&a));
        assert_ne!(a, b);
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&a[TOKEN_PREFIX.len()..])
            .unwrap();
        assert_eq!(raw.len(), 32);
    }

    #[test]
    fn malformed_tokens_are_recognized() {
        for bad in [
            "",
            "dct1_",
            "dct1_short",
            "dct2_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "dct1_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA*",
        ] {
            assert!(!is_well_formed(bad), "{bad:?}");
        }
    }

    #[test]
    fn ensure_creates_an_owner_only_file_once() {
        let dir = temp_dir("ensure");
        let path = dir.join("sub").join("api-token");
        let (first, created) = ensure(&path).unwrap();
        assert!(created);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let parent = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(parent, 0o700);
        let (again, created) = ensure(&path).unwrap();
        assert!(!created);
        assert_eq!(first, again);
        // No temp files left behind.
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_readable_token_file_is_refused() {
        let dir = temp_dir("mode");
        let path = dir.join("api-token");
        ensure(&path).unwrap();
        for mode in [0o640, 0o604, 0o660, 0o644] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(
                matches!(read_secure(&path), Err(TokenError::Insecure(_, m)) if m == mode),
                "{mode:o}"
            );
            assert!(TokenStore::open(&path).is_err());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_symlinked_token_file_is_refused() {
        let dir = temp_dir("link");
        let real = dir.join("real");
        ensure(&real).unwrap();
        let link = dir.join("api-token");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(matches!(read_secure(&link), Err(TokenError::NotRegular(_))));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_or_garbage_file_is_refused() {
        let dir = temp_dir("bad");
        let path = dir.join("api-token");
        assert!(matches!(
            TokenStore::open(&path),
            Err(TokenError::Missing(_))
        ));
        std::fs::write(&path, "hunter2\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(read_secure(&path), Err(TokenError::Malformed(_))));
        // ensure() never overwrites an unusable file silently.
        assert!(ensure(&path).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn verify_accepts_only_the_current_token() {
        let dir = temp_dir("verify");
        let path = dir.join("api-token");
        let (token, _) = ensure(&path).unwrap();
        let store = TokenStore::open(&path).unwrap();
        assert!(store.verify(&token));
        assert!(!store.verify(""));
        assert!(!store.verify(&format!("{token}x")));
        assert!(!store.verify(&token[..token.len() - 1]));
        assert!(!store.verify(&generate().unwrap()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rotation_applies_to_the_next_request_and_deletion_revokes() {
        let dir = temp_dir("rotate");
        let path = dir.join("api-token");
        let (old, _) = ensure(&path).unwrap();
        let store = TokenStore::open(&path).unwrap();
        assert!(store.verify(&old));

        let new = rotate(&path).unwrap();
        assert_ne!(old, new);
        assert!(store.verify(&new), "the rotated token works at once");
        assert!(!store.verify(&old), "the old token stops working at once");

        std::fs::remove_file(&path).unwrap();
        assert!(!store.verify(&new), "deleting the file revokes");

        // Recreated by the operator: accepted again.
        let newer = rotate(&path).unwrap();
        assert!(store.verify(&newer));

        // Made insecure while running: fails closed.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!store.verify(&newer));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// WS_TOKEN_REVOCATION_BYPASS: a credential held by a long-lived
    /// connection dies with the token it was issued for.
    #[test]
    fn a_held_credential_is_revoked_by_rotation_deletion_or_insecurity() {
        let dir = temp_dir("credential");
        let path = dir.join("api-token");
        let (token, _) = ensure(&path).unwrap();
        let store = TokenStore::open(&path).unwrap();
        assert!(store.authenticate(&generate().unwrap()).is_none());
        let held = store.authenticate(&token).expect("the current token");
        assert!(store.still_valid(&held));
        assert!(!format!("{held:?}").contains(&token));

        let rotated = rotate(&path).unwrap();
        assert!(!store.still_valid(&held), "rotation revokes it");
        let held = store.authenticate(&rotated).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(!store.still_valid(&held), "an insecure file revokes it");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(store.still_valid(&held), "the same token, secure again");
        std::fs::remove_file(&path).unwrap();
        assert!(!store.still_valid(&held), "deletion revokes it");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn errors_and_debug_output_never_carry_the_token() {
        let dir = temp_dir("leak");
        let path = dir.join("api-token");
        let (token, _) = ensure(&path).unwrap();
        let store = TokenStore::open(&path).unwrap();
        assert!(!format!("{store:?}").contains(&token));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = read_secure(&path).unwrap_err().to_string();
        assert!(!err.contains(&token[TOKEN_PREFIX.len()..]), "{err}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn constant_time_eq_is_equality() {
        let a = digest("a");
        assert!(constant_time_eq(&a, &a));
        let mut b = a;
        b[31] ^= 1;
        assert!(!constant_time_eq(&a, &b));
    }

    #[test]
    fn fingerprints_are_colon_hex_sha256() {
        let f = fingerprint(b"abc");
        assert_eq!(f.len(), 32 * 3 - 1);
        assert!(f.starts_with("BA:78:16:BF"));
    }
}
