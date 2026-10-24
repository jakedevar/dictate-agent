use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::{ffi::OsStrExt, net::UnixStream};
use std::path::Path;
use std::time::Instant;

pub const MAX_LINE: usize = 64 * 1024;

pub fn remaining(deadline: Instant) -> io::Result<std::time::Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "deadline expired"))
}

/// Nonblocking connect also bounds a full AF_UNIX listen backlog.
pub fn connect(path: &Path, deadline: Instant) -> io::Result<UnixStream> {
    // SAFETY: socket has no pointer arguments; the returned descriptor is owned below.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful socket() returns a fresh owned descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: fcntl acts on our live descriptor, without pointer arguments.
    if unsafe { libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let stream = UnixStream::from(fd);
    stream.set_nonblocking(true)?;
    // SAFETY: all-zero sockaddr_un is valid before its fields are filled in.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty() || bytes.contains(&0) || bytes.len() >= addr.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Unix socket path",
        ));
    }
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (dst, src) in addr.sun_path.iter_mut().zip(bytes) {
        *dst = *src as libc::c_char;
    }
    // SAFETY: addr is initialized and its length matches the pointed-to allocation.
    let result = unsafe {
        libc::connect(
            raw,
            (&addr as *const libc::sockaddr_un).cast(),
            std::mem::size_of_val(&addr) as libc::socklen_t,
        )
    };
    if result < 0 {
        let error = io::Error::last_os_error();
        // AF_UNIX EAGAIN means a full backlog, not an in-progress connection.
        // Fail open immediately rather than blocking the dictation start.
        if error.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(error);
        }
        loop {
            let mut pollfd = libc::pollfd {
                fd: stream.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            };
            let ms = remaining(deadline)?.as_millis().clamp(1, i32::MAX as u128) as i32;
            // SAFETY: pollfd points to one initialized, live pollfd.
            let n = unsafe { libc::poll(&mut pollfd, 1, ms) };
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "connect timeout"));
            }
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if let Some(error) = stream.take_error()? {
                return Err(error);
            }
            break;
        }
    }
    stream.set_nonblocking(false)?;
    Ok(stream)
}

pub fn send(
    stream: &mut UnixStream,
    value: &serde_json::Value,
    deadline: Instant,
) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    let mut offset = 0;
    while offset < bytes.len() {
        stream.set_write_timeout(Some(remaining(deadline)?))?;
        match stream.write(&bytes[offset..]) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => offset += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// A single bounded NDJSON line, with an absolute (not per-byte) deadline.
pub fn receive(stream: &mut UnixStream, deadline: Instant) -> io::Result<serde_json::Value> {
    let mut line = Vec::new();
    loop {
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        let mut byte = [0];
        match stream.read(&mut byte) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "missing reply line",
                ))
            }
            Ok(_) if byte[0] == b'\n' => return Ok(serde_json::from_slice(&line)?),
            Ok(_) if line.len() < MAX_LINE => line.push(byte[0]),
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "reply too large",
                ))
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}
