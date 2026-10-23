//! Private HTTP socket custody. Walk with openat(O_NOFOLLOW), never chmod a
//! pathname, and hold a nonblocking flock through listener shutdown. Other
//! UIDs cannot replace an ancestor: only root/our UID may own it, and writable
//! shared ancestors must be sticky and root-owned. Same-UID compromise is
//! outside S33's threat model (it can already read the bearer token).
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

fn refuse(message: &str) -> io::Error {
    io::Error::new(ErrorKind::PermissionDenied, message)
}

fn private_parent(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(refuse("socket path must be absolute"));
    }
    let mut dir = File::open("/")?;
    let components: Vec<_> = path.components().collect();
    for (i, part) in components.iter().enumerate() {
        let Component::Normal(name) = part else {
            if matches!(part, Component::RootDir) {
                continue;
            }
            return Err(refuse("non-normal socket path component"));
        };
        let name = CString::new(name.as_encoded_bytes()).map_err(|_| refuse("NUL in path"))?;
        // SAFETY: valid directory descriptor and NUL-terminated component.
        let mut fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 && io::Error::last_os_error().kind() == ErrorKind::NotFound {
            // SAFETY: same descriptor/component; directories are private at creation.
            let created = unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o700) };
            if created != 0 && io::Error::last_os_error().kind() != ErrorKind::AlreadyExists {
                return Err(io::Error::last_os_error());
            }
            fd = unsafe {
                libc::openat(
                    dir.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
        }
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned an owned descriptor.
        dir = unsafe { File::from_raw_fd(fd) };
        let meta = dir.metadata()?;
        let uid = unsafe { libc::geteuid() };
        let final_dir = i == components.len() - 1;
        if final_dir {
            if meta.uid() != uid {
                return Err(refuse("socket directory is not owned by us"));
            }
            dir.set_permissions(std::fs::Permissions::from_mode(0o700))?;
        } else if (meta.uid() != 0 && meta.uid() != uid)
            || (meta.mode() & 0o022 != 0 && !(meta.uid() == 0 && meta.mode() & 0o1000 != 0))
        {
            return Err(refuse(
                "socket ancestor is writable or owned by another user",
            ));
        }
    }
    Ok(dir)
}

pub(crate) struct PrivateSocket {
    path: PathBuf,
    socket: std::os::unix::net::UnixListener,
    identity: (u64, u64),
    _parent: File,
    _lock: File,
}
impl PrivateSocket {
    pub(crate) fn bind(path: &Path) -> io::Result<Self> {
        let parent = path.parent().ok_or_else(|| refuse("no socket parent"))?;
        let parent_fd = private_parent(parent)?;
        // Parent is private and cannot be swapped by another UID after the
        // descriptor walk. O_NOFOLLOW also refuses a planted lock symlink.
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(parent.join(".api.lock"))?;
        let meta = lock.metadata()?;
        if !meta.is_file()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o077 != 0
            || meta.nlink() != 1
        {
            return Err(refuse("unsafe API socket lock file"));
        }
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(refuse("API socket directory already in use"));
        }
        match std::fs::symlink_metadata(path) {
            Ok(meta) => {
                if !meta.file_type().is_socket() || meta.uid() != unsafe { libc::geteuid() } {
                    return Err(refuse("existing API socket is not an owned socket"));
                }
                // Never unlink a listening socket, even if another implementation
                // does not participate in our directory lock.
                match std::os::unix::net::UnixStream::connect(path) {
                    Err(e) if e.kind() == ErrorKind::ConnectionRefused => {
                        std::fs::remove_file(path)?
                    }
                    _ => return Err(refuse("existing API socket is live or cannot be checked")),
                }
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        // Parent is already 0700; no other UID can access the freshly bound
        // socket before chmod. Do not change the process-wide umask.
        let socket = std::os::unix::net::UnixListener::bind(path)?;
        let meta = std::fs::symlink_metadata(path)?;
        let owned = Self {
            path: path.into(),
            socket,
            identity: (meta.dev(), meta.ino()),
            _parent: parent_fd,
            _lock: lock,
        };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        owned.socket.set_nonblocking(true)?;
        Ok(owned)
    }
    pub(crate) fn listener(&self) -> io::Result<tokio::net::UnixListener> {
        tokio::net::UnixListener::from_std(self.socket.try_clone()?)
    }
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}
impl Drop for PrivateSocket {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.file_type().is_socket() && (m.dev(), m.ino()) == self.identity)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    fn root() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let root = PathBuf::from("/tmp").canonicalize().unwrap().join(format!(
            "dct-uds-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        root
    }
    #[tokio::test]
    async fn stale_owned_socket_is_reclaimed_but_a_live_one_is_preserved() {
        let root = root();
        let path = root.join("run/api.sock");
        std::fs::create_dir(path.parent().unwrap()).unwrap();
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        let owned = PrivateSocket::bind(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            std::fs::metadata(path.parent().unwrap()).unwrap().mode() & 0o777,
            0o700
        );
        assert!(PrivateSocket::bind(&path).is_err());
        drop(owned);
        assert!(!path.exists());
        let live = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert!(PrivateSocket::bind(&path).is_err());
        assert!(path.exists());
        drop(live);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn symlinks_files_and_unsafe_ancestors_fail_closed() {
        let root = root();
        let real = root.join("real");
        std::fs::create_dir(&real).unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(PrivateSocket::bind(&link.join("run/api.sock")).is_err());
        let path = real.join("api.sock");
        std::fs::write(&path, "keep me").unwrap();
        assert!(PrivateSocket::bind(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep me");
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(root.join("victim"), &path).unwrap();
        assert!(PrivateSocket::bind(&path).is_err());
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(PrivateSocket::bind(&real.join("run/api.sock")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn shutdown_does_not_unlink_a_replacement() {
        let root = root();
        let path = root.join("api.sock");
        let owned = PrivateSocket::bind(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, "replacement").unwrap();
        drop(owned);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "replacement");
        std::fs::remove_dir_all(root).unwrap();
    }
}
