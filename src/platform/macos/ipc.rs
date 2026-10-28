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
//!
//! The folder holding the socket must be a real folder (not a symbolic link) owned by this user
//! with no group or other access; group or other access is removed when the folder is ours, any
//! other folder is refused. When the folder's path is too long for a socket address, the socket
//! lives in the user's private temporary folder instead; the app and the hook helpers work the
//! path out the same way, whatever their environment. Both ends check that the other end runs as
//! the same user, and a request line is at most 64 KB (the protocol in `core::pipe` enforces it).

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
use crate::util::{log, paths};

/// The longest socket path the system accepts, counting the closing NUL (`sun_path`).
const SUN_PATH_MAX: usize = 104;
/// The socket's name in the user's temporary folder, used when the data folder's path is too long.
const FALLBACK_SOCKET: &str = "sessionmux.sock";
/// How often a client retries a server that is not accepting yet.
const RETRY: Duration = Duration::from_millis(50);

/// The socket backend.
pub struct Os;

/// Whether the process at the other end, running as this user id (None when it cannot be told),
/// may use the connection.
type PeerCheck = fn(Option<libc::uid_t>) -> bool;

/// A bound socket; the socket file is removed when it is dropped, unless another socket has
/// replaced it since.
#[derive(Debug)]
pub struct Listener {
    inner: UnixListener,
    path: PathBuf,
    /// The socket file's inode, to recognise it when it is removed.
    ino: u64,
    peer_ok: PeerCheck,
}

impl Drop for Listener {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path).is_ok_and(|m| m.ino() == self.ino) {
            let _ = fs::remove_file(&self.path);
        }
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

/// The socket path for the data folder `dir`: `<dir>/hook.sock`, or `<temp>/sessionmux.sock`
/// when that is too long for a socket address (104 bytes or more with the closing NUL). None
/// when neither fits. `temp` is asked only when it is needed.
fn socket_path(dir: &Path, temp: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
    let fits = |p: &Path| p.as_os_str().len() < SUN_PATH_MAX;
    let own = dir.join(brand::SOCKET_FILE);
    if fits(&own) {
        return Some(own);
    }
    temp().map(|t| t.join(FALLBACK_SOCKET)).filter(|p| fits(p))
}

/// The socket endpoint for the data folder `dir`; empty when no socket path fits.
pub(super) fn endpoint_in(dir: &Path) -> String {
    socket_path(dir, user_temp_dir).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The effective user id of this process.
fn my_uid() -> libc::uid_t {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// The real peer check: the other end runs as this process's user.
fn is_this_user(peer: Option<libc::uid_t>) -> bool {
    peer == Some(my_uid())
}

/// The user id of the process at the other end of `s`.
fn peer_uid(s: &UnixStream) -> Option<libc::uid_t> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: the descriptor belongs to `s`, which is alive; both out-pointers are valid.
    let rc = unsafe { libc::getpeereid(s.as_raw_fd(), &mut uid, &mut gid) };
    (rc == 0).then_some(uid)
}

/// The process id at the other end of `s`, for the log.
fn peer_pid(s: &UnixStream) -> Option<libc::pid_t> {
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: the descriptor belongs to `s`, which is alive; `pid` is a live pid_t and `len`
    // holds its size, so the kernel writes at most that many bytes.
    let rc = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&mut pid as *mut libc::pid_t).cast(),
            &mut len,
        )
    };
    (rc == 0).then_some(pid)
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

fn refused(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, msg)
}

/// Create `dir` (mode 0700) when it is missing, then check it: a folder, not a symbolic link,
/// owned by this user. Group and other access is removed; anything else is refused. Run before
/// the folder holds the lock or the socket.
pub(super) fn secure_dir(dir: &Path) -> io::Result<()> {
    secure_dir_for(dir, my_uid())
}

/// [`secure_dir`] for a folder that must belong to the user `owner`.
fn secure_dir_for(dir: &Path, owner: libc::uid_t) -> io::Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?
        }
        Err(e) => return Err(e),
    }
    let meta = fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() {
        return Err(refused(format!("{} is a symbolic link", dir.display())));
    }
    if !meta.file_type().is_dir() {
        return Err(refused(format!("{} is not a folder", dir.display())));
    }
    if meta.uid() != owner {
        return Err(refused(format!("{} belongs to another user", dir.display())));
    }
    if meta.mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        if fs::symlink_metadata(dir)?.mode() & 0o077 != 0 {
            return Err(refused(format!("{} stays open to other users", dir.display())));
        }
    }
    Ok(())
}

/// Bytes waiting on the descriptor; None when they cannot be counted.
fn pending(fd: libc::c_int) -> Option<usize> {
    let mut n: libc::c_int = 0;
    // SAFETY: FIONREAD writes one c_int to the valid pointer passed.
    let rc = unsafe { libc::ioctl(fd, libc::FIONREAD, &mut n) };
    (rc == 0).then(|| usize::try_from(n).unwrap_or(0))
}

/// [`IpcTransport::listen`] with the peer check `peer_ok` for every accepted client.
fn listen_with(endpoint: &str, peer_ok: PeerCheck) -> io::Result<Listener> {
    if endpoint.is_empty() {
        return Err(io::Error::other("the socket has no path"));
    }
    let path = PathBuf::from(endpoint);
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        secure_dir(dir)?;
    }
    match fs::symlink_metadata(&path) {
        Ok(meta) => {
            if !meta.file_type().is_socket() || meta.uid() != my_uid() {
                return Err(io::Error::new(io::ErrorKind::AlreadyExists, "the socket path is taken by something else"));
            }
            match UnixStream::connect(&path) {
                Ok(_) => {
                    log::warn("pipe: another server is listening on the socket; leaving it alone");
                    return Err(io::Error::new(io::ErrorKind::AddrInUse, "another server holds the socket"));
                }
                // A socket of ours that nobody serves any more (the app ended without removing it).
                Err(e) if e.raw_os_error() == Some(libc::ECONNREFUSED) => match fs::remove_file(&path) {
                    Ok(()) => log::info("pipe: replaced a stale socket"),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                },
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    // The process-wide file-creation mask is left alone, since other threads create files too;
    // the private folder keeps other users out until the socket is narrowed to its owner.
    let inner = UnixListener::bind(&path)?;
    let ino = match fs::symlink_metadata(&path) {
        Ok(m) => m.ino(),
        Err(e) => {
            let _ = fs::remove_file(&path);
            return Err(e);
        }
    };
    let listener = Listener { inner, path, ino, peer_ok };
    fs::set_permissions(&listener.path, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// [`IpcTransport::connect`] with the peer check `peer_ok` for the server.
fn connect_with(endpoint: &str, deadline: Instant, peer_ok: PeerCheck) -> io::Result<UnixStream> {
    if endpoint.is_empty() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "the socket has no path"));
    }
    let path = Path::new(endpoint);
    // Only a socket file of this user is ever connected to.
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_socket() || meta.uid() != my_uid() {
        return Err(refused("the socket is not this user's".into()));
    }
    loop {
        match UnixStream::connect(path) {
            Ok(s) => {
                if !peer_ok(peer_uid(&s)) {
                    return Err(refused("the socket is not served by this user".into()));
                }
                no_sigpipe(&s);
                return Ok(s);
            }
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

impl IpcTransport for Os {
    type Listener = Listener;
    type Conn = UnixStream;

    /// `<data folder>/hook.sock`; when that path is too long for a socket,
    /// `<user temporary folder>/sessionmux.sock`.
    fn endpoint() -> String {
        endpoint_in(&paths::user_data())
    }

    /// The socket's folder is checked first ([`secure_dir`]). A stale socket of this user that
    /// nobody serves is replaced; a live one, or anything else at the path, makes this fail. The
    /// socket file is made readable and writable by the owner only.
    fn listen(endpoint: &str) -> io::Result<Listener> {
        listen_with(endpoint, is_this_user)
    }

    /// A client that runs as another user is closed unanswered and logged with its process id.
    fn accept(l: &mut Listener) -> io::Result<Option<UnixStream>> {
        let s = match l.inner.accept() {
            Ok((s, _)) => s,
            Err(e) if matches!(e.raw_os_error(), Some(libc::ECONNABORTED) | Some(libc::EINTR)) => return Ok(None),
            Err(e) => return Err(e),
        };
        if !(l.peer_ok)(peer_uid(&s)) {
            let pid = peer_pid(&s).map_or_else(|| "unknown".to_string(), |p| p.to_string());
            log::warn(&format!("pipe: refused a client of another user (pid {pid})"));
            return Ok(None);
        }
        no_sigpipe(&s);
        Ok(Some(s))
    }

    fn connect(endpoint: &str, deadline: Instant) -> io::Result<UnixStream> {
        connect_with(endpoint, deadline, is_this_user)
    }

    /// Readable with nothing to read means the other end closed (the helper exited, for example
    /// because the user answered in Claude).
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
    use crate::core::pipe::{self, Decision, PipeError, Request, Responder, Server, MAX_MESSAGE};
    use std::io::{Read, Write};
    use std::process::{Command, Output, Stdio};
    use std::sync::mpsc;

    const SID: &str = "0d2a6f5e-6a3c-4c1e-9a43-7f1d0c2b9e11";

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    /// The endpoint of a data folder inside `d`, created by the first listen.
    fn endpoint_under(d: &tempfile::TempDir) -> String {
        endpoint_in(&d.path().join(brand::DATA_DIR_NAME))
    }

    fn refuse_everyone(_: Option<libc::uid_t>) -> bool {
        false
    }

    fn approve(summary: &str) -> Request {
        Request::Approve {
            request_id: uuid::Uuid::new_v4().simple().to_string(),
            session_id: SID.into(),
            cwd: "/Users/me/demo".into(),
            tool_name: "Bash".into(),
            mcp_server: None,
            summary: summary.into(),
            time: 1_792_990_287,
        }
    }

    fn notify() -> Request {
        Request::Notify {
            event: "Notification".into(),
            notification_type: Some("idle_prompt".into()),
            session_id: SID.into(),
            cwd: "/Users/me/demo".into(),
            message: "Claude is waiting for your input".into(),
            time: 1_792_990_287,
        }
    }

    /// A server on `name` that hands every request and its responder to the returned receiver.
    fn collecting_server(name: &str) -> (Server, mpsc::Receiver<(Request, Responder)>) {
        let (tx, rx) = mpsc::channel();
        let server = Server::start_at(
            name,
            Box::new(move |req, resp| {
                let _ = tx.send((req, resp));
            }),
        )
        .unwrap();
        (server, rx)
    }

    fn wait_gone(resp: &Responder) -> bool {
        let until = soon();
        while Instant::now() < until {
            if resp.is_gone() {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn endpoint_is_stable_and_fits() {
        let e = Os::endpoint();
        assert!(!e.is_empty());
        assert_eq!(e, Os::endpoint());
        assert!(e.len() < SUN_PATH_MAX, "{e}");
        assert_ne!(Os::test_endpoint(), Os::test_endpoint());
        assert!(user_temp_dir().is_some_and(|d| d.is_absolute()));
    }

    #[test]
    fn a_path_too_long_for_a_socket_moves_to_the_temporary_folder() {
        let temp = || Some(PathBuf::from("/private/var/folders/xy/abc/T"));
        let sized = |total: usize| {
            let name = "x".repeat(total - "/".len() - "/".len() - brand::SOCKET_FILE.len());
            PathBuf::from("/").join(name)
        };
        // 103 bytes and the closing NUL fit exactly.
        let fits = sized(103);
        assert_eq!(socket_path(&fits, temp).unwrap(), fits.join(brand::SOCKET_FILE));
        assert_eq!(fits.join(brand::SOCKET_FILE).as_os_str().len(), 103);
        let long = sized(104);
        assert_eq!(socket_path(&long, temp).unwrap(), PathBuf::from("/private/var/folders/xy/abc/T/sessionmux.sock"));
        assert_eq!(socket_path(&long, || None), None);
        assert_eq!(socket_path(&long, || Some(sized(200))), None);
        assert_eq!(
            socket_path(Path::new("/short"), || panic!("not asked when the path fits")).unwrap(),
            Path::new("/short/hook.sock")
        );

        // The real temporary folder is short enough, and both sides of the socket agree on it.
        let real = endpoint_in(&sized(150));
        assert!(real.ends_with("/sessionmux.sock"), "{real}");
        assert!(real.len() < SUN_PATH_MAX);
        assert_eq!(real, endpoint_in(&sized(150)));
    }

    #[test]
    fn only_one_server_per_path() {
        let name = Os::test_endpoint();
        let first = Os::listen(&name).unwrap();
        assert_eq!(Os::listen(&name).unwrap_err().kind(), io::ErrorKind::AddrInUse);
        drop(first);
        assert!(!Path::new(&name).exists());
        let second = Os::listen(&name).unwrap();
        assert!(Os::listen("").is_err());
        drop(second);
    }

    #[test]
    fn a_stale_socket_is_replaced_and_reads_as_nobody() {
        let name = Os::test_endpoint();
        let stale = UnixListener::bind(&name).unwrap();
        drop(stale); // the file stays, nobody serves it
        assert!(Path::new(&name).exists());
        let err = Os::connect(&name, soon()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        let l = Os::listen(&name).unwrap();
        let mode = fs::metadata(&name).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        drop(l);
        assert!(!Path::new(&name).exists());
    }

    #[test]
    fn a_dropped_listener_leaves_a_newer_socket_alone() {
        let name = Os::test_endpoint();
        let old = Os::listen(&name).unwrap();
        fs::remove_file(&name).unwrap();
        let new = Os::listen(&name).unwrap();
        drop(old);
        assert!(Path::new(&name).exists());
        drop(new);
        assert!(!Path::new(&name).exists());
    }

    #[test]
    fn something_else_at_the_socket_path_is_left_alone() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("hook.sock");
        fs::write(&file, b"keep").unwrap();
        let err = Os::listen(&file.to_string_lossy()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&file).unwrap(), b"keep");
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
        assert_eq!(secure_dir(&link).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        let file = d.path().join("file");
        fs::write(&file, b"").unwrap();
        assert_eq!(secure_dir(&file).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert!(Os::listen(&link.join("hook.sock").to_string_lossy()).is_err());
        assert!(!dir.join("hook.sock").exists(), "nothing is created through the link");
    }

    #[test]
    fn another_users_folder_is_refused_and_left_as_it_is() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("shared");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        let other = my_uid().wrapping_add(1);
        let err = secure_dir_for(&dir, other).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("another user"), "{err}");
        assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o755);
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
    fn the_peer_check_compares_user_ids() {
        assert!(is_this_user(Some(my_uid())));
        assert!(!is_this_user(Some(my_uid().wrapping_add(1))));
        assert!(!is_this_user(None));
    }

    #[test]
    fn a_client_of_another_user_is_closed_unanswered() {
        let d = tempfile::tempdir().unwrap();
        let name = endpoint_under(&d);
        let mut l = listen_with(&name, refuse_everyone).unwrap();
        let client = thread::spawn({
            let name = name.clone();
            move || {
                let mut c = Os::connect(&name, soon()).unwrap();
                let _ = c.write_all(b"{\"kind\":\"show\"}\n");
                let mut buf = [0u8; 8];
                c.read(&mut buf).unwrap_or(0)
            }
        });
        assert!(Os::accept(&mut l).unwrap().is_none());
        assert_eq!(client.join().unwrap(), 0, "the refused client reads a close");
    }

    #[test]
    fn a_server_of_another_user_is_not_used() {
        let d = tempfile::tempdir().unwrap();
        let name = endpoint_under(&d);
        let _l = Os::listen(&name).unwrap();
        let err = connect_with(&name, soon(), refuse_everyone).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(Os::connect(&name, soon()).is_ok());
    }

    #[test]
    fn closed_and_idle_connections_read_as_such() {
        let name = Os::test_endpoint();
        let mut l = Os::listen(&name).unwrap();
        let client = thread::spawn({
            let name = name.clone();
            move || Os::connect(&name, soon()).unwrap()
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

    #[test]
    fn notify_round_trip() {
        let d = tempfile::tempdir().unwrap();
        let name = endpoint_under(&d);
        let (server, rx) = collecting_server(&name);
        pipe::client_send_notify_at(&name, &notify(), soon()).unwrap();
        let (req, resp) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(req, notify());
        assert!(resp.is_gone(), "nobody waits for a notify answer");
        assert_eq!(resp.decide(Decision::Allow), Err(PipeError::Gone));
        let mode = fs::metadata(d.path().join(brand::DATA_DIR_NAME)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        server.stop();
        assert!(!Path::new(&name).exists());
    }

    #[test]
    fn approve_round_trip_allow_and_deny() {
        let d = tempfile::tempdir().unwrap();
        let name = endpoint_under(&d);
        let (server, rx) = collecting_server(&name);
        for decision in
            [Decision::Allow, Decision::Deny { message: Some("not now".into()) }, Decision::Deny { message: None }]
        {
            let req = approve("cargo test");
            let client = thread::spawn({
                let (name, req) = (name.clone(), req.clone());
                move || pipe::client_approve_at(&name, &req, soon())
            });
            let (got, resp) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(got, req);
            assert!(!resp.is_gone());
            resp.decide(decision.clone()).unwrap();
            assert_eq!(client.join().unwrap(), Ok(Some(decision)));
        }
        server.stop();
    }

    #[test]
    fn an_approve_past_its_deadline_ends_quietly_on_both_sides() {
        let d = tempfile::tempdir().unwrap();
        let name = endpoint_under(&d);
        let (server, rx) = collecting_server(&name);
        let started = Instant::now();
        let answer = pipe::client_approve_at(&name, &approve("sleep 1"), Instant::now() + Duration::from_millis(600));
        assert_eq!(answer, Ok(None), "no decision in time");
        assert!(started.elapsed() < Duration::from_secs(3));
        let (_, resp) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(wait_gone(&resp), "the app learns that the helper gave up");
        assert_eq!(resp.decide(Decision::Allow), Err(PipeError::Gone));
        server.stop();
    }

    #[test]
    fn a_helper_that_disconnects_reads_as_gone() {
        let d = tempfile::tempdir().unwrap();
        let name = endpoint_under(&d);
        let (server, rx) = collecting_server(&name);
        let mut line = serde_json::to_vec(&approve("ls")).unwrap();
        line.push(b'\n');
        let mut helper = Os::connect(&name, soon()).unwrap();
        helper.write_all(&line).unwrap();
        let (_, resp) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(!resp.is_gone());
        // The user answered in Claude: Claude ends the helper.
        drop(helper);
        assert!(wait_gone(&resp));
        assert_eq!(resp.decide(Decision::Allow), Err(PipeError::Gone));
        server.stop();
    }

    #[test]
    fn messages_over_64_kb_are_refused() {
        let d = tempfile::tempdir().unwrap();
        let name = endpoint_under(&d);
        let (server, rx) = collecting_server(&name);

        let mut conn = Os::connect(&name, soon()).unwrap();
        let mut big = vec![b'x'; MAX_MESSAGE + 1];
        big.push(b'\n');
        let _ = conn.write_all(&big);
        let mut buf = [0u8; 16];
        assert_eq!(conn.read(&mut buf).unwrap_or(0), 0, "the server closes without answering");

        assert_eq!(
            pipe::client_approve_at(&name, &approve(&"y".repeat(MAX_MESSAGE)), soon()),
            Err(PipeError::Malformed)
        );

        // A request just under the limit still arrives.
        let near = approve(&"z".repeat(MAX_MESSAGE - 1024));
        let client = thread::spawn({
            let (name, near) = (name.clone(), near.clone());
            move || pipe::client_approve_at(&name, &near, soon())
        });
        let (got, resp) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(got, near);
        resp.decide(Decision::Allow).unwrap();
        assert_eq!(client.join().unwrap(), Ok(Some(Decision::Allow)));
        assert!(rx.try_recv().is_err(), "the oversized message never reached the app");
        server.stop();
    }

    #[test]
    fn show_reaches_the_server_as_one_line() {
        let d = tempfile::tempdir().unwrap();
        let name = endpoint_under(&d);
        let mut l = Os::listen(&name).unwrap();
        let client = thread::spawn({
            let name = name.clone();
            move || pipe::client_show_at(&name, soon())
        });
        let mut conn = Os::accept(&mut l).unwrap().unwrap();
        let mut got = Vec::new();
        let mut byte = [0u8; 1];
        while conn.read(&mut byte).unwrap() == 1 {
            got.push(byte[0]);
            if byte[0] == b'\n' {
                break;
            }
        }
        assert_eq!(got, b"{\"kind\":\"show\"}\n");
        drop(conn);
        assert_eq!(client.join().unwrap(), Ok(()));
        assert_eq!(
            pipe::client_show_at(&endpoint_under(&tempfile::tempdir().unwrap()), soon()),
            Err(PipeError::NotRunning)
        );
    }

    // -----------------------------------------------------------------------------------------
    // The hook helpers, run as the built executable.

    /// The built executable next to this test binary, or a release build of it; None (with the
    /// reason printed) when neither has been built.
    fn built_exe() -> Option<PathBuf> {
        let me = std::env::current_exe().ok()?;
        let profile = me.parent()?.parent()?;
        let target = profile.parent()?;
        let found = [
            profile.join(brand::NAME),
            target.join("release").join(brand::NAME),
            target.join("debug").join(brand::NAME),
        ]
        .into_iter()
        .find(|p| p.is_file());
        if found.is_none() {
            eprintln!("skipped: no built {} executable next to the tests (run cargo build first)", brand::NAME);
        }
        found
    }

    /// Run the executable with `arg`, `input` on stdin and `home` as the home folder.
    fn run_helper(exe: &Path, home: &Path, arg: &str, input: &[u8]) -> Output {
        let mut child = Command::new(exe)
            .arg(arg)
            .env("HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let _ = stdin.write_all(input);
        drop(stdin);
        child.wait_with_output().unwrap()
    }

    #[test]
    fn hook_helpers_exit_0_with_no_app_and_with_garbage() {
        let Some(exe) = built_exe() else { return };
        let home = tempfile::tempdir().unwrap();
        let permission = serde_json::json!({
            "session_id": SID, "transcript_path": "x.jsonl", "cwd": "/Users/me/demo",
            "permission_mode": "default", "hook_event_name": "PermissionRequest",
            "tool_name": "Bash", "tool_input": { "command": "ls" },
        })
        .to_string();
        let notification = serde_json::json!({
            "session_id": SID, "cwd": "/Users/me/demo", "hook_event_name": "Notification",
            "notification_type": "idle_prompt", "message": "Claude is waiting for your input",
        })
        .to_string();
        let inputs: [&[u8]; 6] =
            [b"", b"{}", b"not json", b"\xff\xfe\x00garbage", permission.as_bytes(), notification.as_bytes()];
        for arg in ["--notify-hook", "--approve-hook"] {
            for input in inputs {
                let started = Instant::now();
                let out = run_helper(&exe, home.path(), arg, input);
                let shown = String::from_utf8_lossy(input);
                assert_eq!(
                    out.status.code(),
                    Some(0),
                    "{arg} with {shown:?}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                assert!(
                    out.stdout.is_empty(),
                    "{arg} with {shown:?} printed {:?}",
                    String::from_utf8_lossy(&out.stdout)
                );
                assert!(started.elapsed() < Duration::from_secs(10), "{arg} with {shown:?} took too long");
            }
        }
        // The helpers never start the app or take its lock.
        assert!(!home
            .path()
            .join("Library/Application Support")
            .join(brand::DATA_DIR_NAME)
            .join(brand::LOCK_FILE)
            .exists());
    }

    #[test]
    fn remove_hooks_on_a_temporary_home() {
        use crate::core::hooks;
        let Some(exe) = built_exe() else { return };
        let exe = fs::canonicalize(&exe).unwrap();
        let home = tempfile::tempdir().unwrap();
        let settings = home.path().join(".claude").join("settings.json");
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        let other = serde_json::json!({
            "theme": "dark",
            "hooks": { "Notification": [ { "matcher": "", "hooks": [ { "type": "command", "command": "/usr/bin/true" } ] } ] },
        });
        fs::write(&settings, serde_json::to_vec_pretty(&other).unwrap()).unwrap();
        hooks::install(&settings, &hooks::plan(&exe)).unwrap();
        assert!(hooks::status(&settings, &exe).installed);

        let out = run_helper(&exe, home.path(), "--remove-hooks", b"");
        assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(!hooks::status(&settings, &exe).installed);
        let after: serde_json::Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
        assert_eq!(after["theme"], "dark");
        assert_eq!(after["hooks"]["Notification"][0]["hooks"][0]["command"], "/usr/bin/true");
        assert!(after["hooks"].get("PermissionRequest").is_none());

        // A second run, and a run with no settings file at all, change nothing and still exit 0.
        let before = fs::read(&settings).unwrap();
        assert_eq!(run_helper(&exe, home.path(), "--remove-hooks", b"").status.code(), Some(0));
        assert_eq!(fs::read(&settings).unwrap(), before);
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(run_helper(&exe, empty.path(), "--remove-hooks", b"").status.code(), Some(0));
        assert!(!empty.path().join(".claude").exists());
    }
}
