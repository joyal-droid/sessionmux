// Copyright (c) 2026 joyal-droid. SPDX-License-Identifier: CPAL-1.0
//
// The contents of this file are subject to the Common Public Attribution License Version 1.0
// (the "License"); you may not use this file except in compliance with the License. You may
// obtain a copy of the License in the LICENSE file at the root of this repository or at
// https://opensource.org/license/cpal-1-0. The License is based on the Mozilla Public License
// Version 1.1 but Sections 14 and 15 have been added to cover use of software over a computer
// network and provide for limited attribution for the Original Developer. In addition, Exhibit A
// has been modified to be consistent with Exhibit B.
//
// Software distributed under the License is distributed on an "AS IS" basis, WITHOUT WARRANTY OF
// ANY KIND, either express or implied. See the License for the specific language governing rights
// and limitations under the License.
//
// The Original Code is Sessionmux. The Initial Developer of the Original Code is joyal-droid. All
// portions of the code written by joyal-droid are Copyright (c) 2026 joyal-droid. All Rights Reserved.
// Contributor(s): see the git history of this repository.
//
// Attribution (Exhibit B of the License): "Powered by Sessionmux", Copyright (c) 2026 joyal-droid,
// https://github.com/joyal-droid/sessionmux and the Sessionmux mark must stay visible in the user
// interface of every copy and derivative; see docs/attribution.md.

//! The hook socket: a Unix domain socket in Sessionmux's data folder, served to and accepted
//! from the current user only.

use std::ffi::CStr;
use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::brand;
use crate::platform::{IpcTransport, Readiness};
use crate::util::paths;

/// The longest socket path the system accepts, counting the closing NUL (`sun_path`).
const SUN_PATH_MAX: usize = 104;
/// How often a client retries a server that is not accepting yet.
const RETRY: Duration = Duration::from_millis(50);

/// The socket backend.
pub struct Os;

/// A bound socket; the socket file is removed when it is dropped.
pub struct Listener {
    inner: UnixListener,
    path: PathBuf,
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// The per-user temporary folder (`confstr(_CS_DARWIN_USER_TEMP_DIR)`), the same for every
/// process of the user whatever its environment.
fn user_temp_dir() -> Option<PathBuf> {
    let mut buf: Vec<libc::c_char> = vec![0; 1024];
    // SAFETY: `buf` is writable for `buf.len()` bytes; confstr writes at most that many,
    // NUL-terminated, and returns the size it needed.
    let n = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buf.as_mut_ptr(), buf.len()) };
    if n == 0 || n > buf.len() {
        return None;
    }
    // SAFETY: confstr NUL-terminated the value inside `buf`.
    let s = unsafe { CStr::from_ptr(buf.as_ptr()) };
    Some(PathBuf::from(s.to_string_lossy().into_owned()))
}

/// The effective user id of this process.
fn my_uid() -> libc::uid_t {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// The user id of the process at the other end of `s`.
fn peer_uid(s: &UnixStream) -> Option<libc::uid_t> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: the descriptor belongs to `s`, which is alive; both out-pointers are valid.
    let rc = unsafe { libc::getpeereid(s.as_raw_fd(), &mut uid, &mut gid) };
    (rc == 0).then_some(uid)
}

/// Writes to a closed connection fail with `EPIPE` instead of raising `SIGPIPE`.
fn no_sigpipe(s: &UnixStream) {
    let on: libc::c_int = 1;
    // SAFETY: the descriptor belongs to `s`; `on` is a live c_int of the size passed.
    let _ = unsafe {
        libc::setsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_NOSIGPIPE,
            (&on as *const libc::c_int).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
}

/// Create `dir` (mode 0700) when it is missing, then check it: a folder, not a symbolic link,
/// owned by this user. Group and other access is removed; anything else is refused. Run before
/// the folder holds the lock or the socket.
pub(super) fn secure_dir(dir: &Path) -> io::Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?
        }
        Err(e) => return Err(e),
    }
    let meta = fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("{} is not a folder", dir.display())));
    }
    if meta.uid() != my_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} belongs to another user", dir.display()),
        ));
    }
    if meta.mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// A socket served by this user, ready for use; any other peer is refused.
fn checked(s: UnixStream) -> io::Result<UnixStream> {
    if peer_uid(&s) != Some(my_uid()) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "the socket is not served by this user"));
    }
    no_sigpipe(&s);
    Ok(s)
}

/// Bytes waiting on the descriptor; None when they cannot be counted.
fn pending(fd: libc::c_int) -> Option<usize> {
    let mut n: libc::c_int = 0;
    // SAFETY: FIONREAD writes one c_int to the valid pointer passed.
    let rc = unsafe { libc::ioctl(fd, libc::FIONREAD, &mut n) };
    (rc == 0).then(|| usize::try_from(n).unwrap_or(0))
}

impl IpcTransport for Os {
    type Listener = Listener;
    type Conn = UnixStream;

    /// `<data folder>/hook.sock`; when that path is too long for a socket,
    /// `<user temporary folder>/sessionmux.sock`.
    fn endpoint() -> String {
        let p = paths::user_data().join(brand::SOCKET_FILE);
        if p.as_os_str().len() < SUN_PATH_MAX {
            return p.to_string_lossy().into_owned();
        }
        match user_temp_dir() {
            Some(d) => d.join("sessionmux.sock").to_string_lossy().into_owned(),
            None => String::new(),
        }
    }

    /// The socket's folder is checked first ([`secure_dir`]). A stale socket of this user that
    /// nobody serves is replaced; a live one makes this fail. The socket file is created readable
    /// and writable by the owner only.
    fn listen(endpoint: &str) -> io::Result<Listener> {
        if endpoint.is_empty() {
            return Err(io::Error::other("the socket has no path"));
        }
        let path = PathBuf::from(endpoint);
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            secure_dir(dir)?;
        }
        if let Ok(meta) = fs::symlink_metadata(&path) {
            if UnixStream::connect(&path).is_ok() {
                return Err(io::Error::new(io::ErrorKind::AddrInUse, "another server holds the socket"));
            }
            if !meta.file_type().is_socket() || meta.uid() != my_uid() {
                return Err(io::Error::new(io::ErrorKind::AlreadyExists, "the socket path is taken by a file"));
            }
            fs::remove_file(&path)?;
        }
        // SAFETY: umask only swaps the process file-creation mask; the old one is put back below.
        let old = unsafe { libc::umask(0o177) };
        let bound = UnixListener::bind(&path);
        // SAFETY: as above.
        unsafe { libc::umask(old) };
        Ok(Listener { inner: bound?, path })
    }

    fn accept(l: &mut Listener) -> io::Result<Option<UnixStream>> {
        let s = match l.inner.accept() {
            Ok((s, _)) => s,
            Err(e) if e.raw_os_error() == Some(libc::ECONNABORTED) => return Ok(None),
            Err(e) => return Err(e),
        };
        // A client of another user is dropped unanswered.
        Ok(checked(s).ok())
    }

    fn connect(endpoint: &str, deadline: Instant) -> io::Result<UnixStream> {
        if endpoint.is_empty() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "the socket has no path"));
        }
        let path = Path::new(endpoint);
        // Only a socket file of this user is ever connected to.
        let meta = fs::symlink_metadata(path)?;
        if !meta.file_type().is_socket() || meta.uid() != my_uid() {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "the socket is not this user's"));
        }
        loop {
            match UnixStream::connect(path) {
                Ok(s) => return checked(s),
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(e),
                // A socket file that nobody serves.
                Err(e) if e.raw_os_error() == Some(libc::ECONNREFUSED) => {
                    return Err(io::Error::new(io::ErrorKind::NotFound, e.to_string()))
                }
                Err(e) if matches!(e.raw_os_error(), Some(libc::EAGAIN) | Some(libc::EINTR)) => {
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, "the socket stayed busy"));
                    }
                    thread::sleep(RETRY);
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn readable(c: &UnixStream, timeout: Duration) -> io::Result<Readiness> {
        let fd = c.as_raw_fd();
        let until = Instant::now() + timeout;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            let ms = libc::c_int::try_from(left.as_millis()).unwrap_or(libc::c_int::MAX);
            let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            // SAFETY: `pfd` is one live pollfd for the descriptor of `c`, which is alive.
            let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
            if rc < 0 {
                let e = io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(e);
            }
            if rc == 0 {
                return Ok(Readiness::Idle);
            }
            return Ok(match pending(fd) {
                Some(n) if n > 0 => Readiness::Data(n),
                // Readable with nothing to read: the other end closed.
                _ => Readiness::Gone,
            });
        }
    }

    #[cfg(test)]
    fn test_endpoint() -> String {
        let name = format!("smx-{}.sock", uuid::Uuid::new_v4().simple());
        std::env::temp_dir().join(name).to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn endpoint_is_stable_and_fits() {
        let e = Os::endpoint();
        assert!(!e.is_empty());
        assert!(e.len() < SUN_PATH_MAX, "{e}");
        assert_ne!(Os::test_endpoint(), Os::test_endpoint());
        assert!(user_temp_dir().is_some_and(|d| d.is_absolute()));
    }

    #[test]
    fn only_one_server_per_path() {
        let name = Os::test_endpoint();
        let first = Os::listen(&name).unwrap();
        assert!(Os::listen(&name).is_err());
        drop(first);
        assert!(!Path::new(&name).exists());
        assert!(Os::listen(&name).is_ok());
        assert!(Os::listen("").is_err());
    }

    #[test]
    fn a_stale_socket_is_replaced_and_reads_as_nobody() {
        let name = Os::test_endpoint();
        let stale = UnixListener::bind(&name).unwrap();
        drop(stale); // the file stays, nobody serves it
        let err = Os::connect(&name, Instant::now() + Duration::from_secs(1)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        let l = Os::listen(&name).unwrap();
        let mode = fs::metadata(&name).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        drop(l);
    }

    #[test]
    fn the_data_folder_is_private() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("a").join("b");
        secure_dir(&dir).unwrap();
        assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        secure_dir(&dir).unwrap();
        assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(secure_dir(&link).is_err());
        let file = d.path().join("file");
        fs::write(&file, b"").unwrap();
        assert!(secure_dir(&file).is_err());
        assert!(Os::listen(&link.join("hook.sock").to_string_lossy()).is_err());
    }

    #[test]
    fn only_sockets_are_connected_to() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("hook.sock");
        fs::write(&file, b"").unwrap();
        let err = Os::connect(&file.to_string_lossy(), Instant::now()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn nobody_serving_is_not_found() {
        let err = Os::connect(&Os::test_endpoint(), Instant::now()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert_eq!(Os::connect("", Instant::now()).unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn closed_and_idle_connections_read_as_such() {
        let name = Os::test_endpoint();
        let mut l = Os::listen(&name).unwrap();
        let client = thread::spawn({
            let name = name.clone();
            move || Os::connect(&name, Instant::now() + Duration::from_secs(5)).unwrap()
        });
        let mut server = Os::accept(&mut l).unwrap().unwrap();
        let mut client = client.join().unwrap();
        assert_eq!(Os::readable(&server, Duration::ZERO).unwrap(), Readiness::Idle);
        client.write_all(b"hi\n").unwrap();
        assert_eq!(Os::readable(&server, Duration::from_secs(5)).unwrap(), Readiness::Data(3));
        let mut buf = [0u8; 8];
        assert_eq!(server.read(&mut buf).unwrap(), 3);
        assert_eq!(&buf[..3], b"hi\n");
        drop(client);
        assert_eq!(Os::readable(&server, Duration::from_secs(5)).unwrap(), Readiness::Gone);
        assert_eq!(server.read(&mut buf).unwrap(), 0);
        assert_eq!(server.write(b"x").unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    }
}
