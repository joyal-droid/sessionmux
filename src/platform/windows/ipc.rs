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

//! The hook pipe's transport: a per-user named pipe `\\.\pipe\Sessionmux.<user SID>`.
//!
//! The pipe is created with a security descriptor that grants the current user only
//! (`D:P(A;;GA;;;<SID>)`), in message mode, local clients only; the first instance is created
//! with `FILE_FLAG_FIRST_PIPE_INSTANCE`, so the app never serves a name somebody else already
//! holds. Every client gets its own instance (`PIPE_UNLIMITED_INSTANCES`): `accept` waits on a
//! free instance and creates the next free instance as soon as a client connects, so the name
//! always has an instance waiting while the server lives. A server instance is disconnected
//! before it is closed.
//!
//! Connecting waits for a busy pipe (`WaitNamedPipeW`) until the deadline, and for at least
//! [`BUSY_MIN`] even when the deadline is closer. Clients connect with identification-only
//! impersonation and talk only to a server process that runs as the same user.

use super::win::{self, ffi};
use crate::brand;
use crate::core::pipe::{BUSY_MIN, MAX_MESSAGE};
use crate::platform::{IpcTransport, Readiness};
use crate::util::log;
use std::io::{self, Read, Write};
use std::thread;
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, LocalFree, HANDLE, HLOCAL};
use windows::Win32::Security::SECURITY_ATTRIBUTES;

/// Polling step while waiting for data, a disconnect or a free instance.
const POLL: Duration = Duration::from_millis(10);

/// The named-pipe backend.
pub struct Os;

/// One end of a pipe; closed on drop.
struct Pipe(HANDLE);

// SAFETY: a pipe handle is a kernel object reference usable from any thread; each `Pipe` is owned
// by exactly one thread at a time and closed once.
unsafe impl Send for Pipe {}

impl Drop for Pipe {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a handle this `Pipe` owns; it is closed exactly once here.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// A connected pipe end. A server instance is disconnected from its client when dropped
/// (before the handle closes), so the instance never lingers half-closed.
pub struct Conn {
    pipe: Pipe,
    server: bool,
}

impl Drop for Conn {
    fn drop(&mut self) {
        if self.server {
            // SAFETY: `self.pipe.0` is a server instance handle that is still open (the `Pipe`
            // is dropped after this); disconnecting an instance without a client is harmless.
            let _ = unsafe { ffi::DisconnectNamedPipe(self.pipe.0) };
        }
    }
}

/// What is waiting on a pipe right now.
enum Peek {
    Data(u32),
    Empty,
    Closed,
}

fn last_code() -> u32 {
    // SAFETY: GetLastError reads the calling thread's last-error value; no preconditions.
    unsafe { GetLastError() }.0
}

fn is_disconnect(code: u32) -> bool {
    matches!(code, ffi::ERROR_BROKEN_PIPE | ffi::ERROR_NO_DATA | ffi::ERROR_PIPE_NOT_CONNECTED)
}

fn peek(pipe: &Pipe) -> io::Result<Peek> {
    let mut avail = 0u32;
    // SAFETY: `pipe.0` is an open pipe handle; no buffer is passed, only the count out-pointer.
    let ok = unsafe {
        ffi::PeekNamedPipe(pipe.0, std::ptr::null_mut(), 0, std::ptr::null_mut(), &mut avail, std::ptr::null_mut())
    };
    if ok.as_bool() {
        return Ok(if avail > 0 { Peek::Data(avail) } else { Peek::Empty });
    }
    if is_disconnect(last_code()) {
        Ok(Peek::Closed)
    } else {
        Err(io::Error::other(win::last_error()))
    }
}

impl Read for Conn {
    /// Reads what is waiting, at most one message; `Ok(0)` once the other end closed.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = buf.len().min(u32::MAX as usize) as u32;
        let mut got = 0u32;
        // SAFETY: `buf` has at least `n` writable bytes; `got` is a valid out-pointer; synchronous.
        let ok = unsafe { ffi::ReadFile(self.pipe.0, buf.as_mut_ptr().cast(), n, &mut got, std::ptr::null_mut()) };
        if !ok.as_bool() {
            let code = last_code();
            if is_disconnect(code) {
                return Ok(0);
            }
            // A message longer than the buffer: the rest is read by the next call.
            if code != ffi::ERROR_MORE_DATA {
                return Err(io::Error::other(win::last_error()));
            }
        }
        Ok((got as usize).min(buf.len()))
    }
}

impl Write for Conn {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut put = 0u32;
        let n = bytes.len().min(MAX_MESSAGE + 1) as u32;
        // SAFETY: `bytes` has at least `n` readable bytes; `put` is a valid out-pointer.
        let ok = unsafe { ffi::WriteFile(self.pipe.0, bytes.as_ptr().cast(), n, &mut put, std::ptr::null_mut()) };
        if !ok.as_bool() {
            let code = last_code();
            return Err(if is_disconnect(code) {
                io::Error::from(io::ErrorKind::BrokenPipe)
            } else {
                io::Error::other(win::last_error())
            });
        }
        Ok((put as usize).min(bytes.len()))
    }

    /// Waits until the other end has read everything written.
    fn flush(&mut self) -> io::Result<()> {
        // SAFETY: `self.pipe.0` is a connected pipe handle owned by this value.
        if unsafe { ffi::FlushFileBuffers(self.pipe.0) }.as_bool() {
            Ok(())
        } else {
            Err(io::Error::other(win::last_error()))
        }
    }
}

/// A security descriptor built from SDDL; freed on drop.
struct Descriptor(*mut core::ffi::c_void);

// SAFETY: the descriptor is plain memory owned by this value; it is only read by
// CreateNamedPipeW on the thread that currently owns it.
unsafe impl Send for Descriptor {}

impl Descriptor {
    /// Full access for `sid`, nothing for anyone else, no inherited entries.
    fn user_only(sid: &str) -> io::Result<Descriptor> {
        let sddl = win::wide(&format!("D:P(A;;GA;;;{sid})"));
        let mut sd: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated and outlives the call; `sd` receives a LocalAlloc'd
        // descriptor that is freed in Drop.
        let ok = unsafe {
            ffi::ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                ffi::SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        if !ok.as_bool() || sd.is_null() {
            return Err(io::Error::other(format!("pipe security descriptor: {}", win::last_error())));
        }
        Ok(Descriptor(sd))
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: `self.0` was allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW
        // and is freed exactly once here.
        let _ = unsafe { LocalFree(HLOCAL(self.0)) };
    }
}

fn create_instance(name: &[u16], sd: &Descriptor, first: bool) -> io::Result<Pipe> {
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: false.into(),
    };
    let open_mode = ffi::PIPE_ACCESS_DUPLEX | if first { ffi::FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
    let pipe_mode = ffi::PIPE_TYPE_MESSAGE | ffi::PIPE_READMODE_MESSAGE | ffi::PIPE_REJECT_REMOTE_CLIENTS;
    let buf = MAX_MESSAGE as u32 + 2;
    // SAFETY: `name` is NUL-terminated; `sa` and the descriptor it points to outlive the call.
    let h = unsafe {
        ffi::CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            open_mode,
            pipe_mode,
            ffi::PIPE_UNLIMITED_INSTANCES,
            buf,
            buf,
            0,
            &sa,
        )
    };
    if h.is_invalid() {
        return Err(io::Error::other(format!("creating the pipe failed: {}", win::last_error())));
    }
    Ok(Pipe(h))
}

/// A served pipe name: its descriptor and the instance waiting for the next client.
pub struct Listener {
    name: Vec<u16>,
    sd: Descriptor,
    free: Option<Pipe>,
}

/// Open the pipe as a client. A busy pipe is waited for until `deadline`, and for at least
/// [`BUSY_MIN`]. `NotFound` when nobody serves it, `TimedOut` when no instance came free in time.
fn open_client(name: &str, deadline: Instant) -> io::Result<Pipe> {
    if name.is_empty() {
        return Err(io::Error::from(io::ErrorKind::NotFound));
    }
    let wname = win::wide(name);
    let until = deadline.max(Instant::now() + BUSY_MIN);
    loop {
        // SAFETY: `wname` is NUL-terminated and outlives the call; no security attributes or
        // template; the returned handle is owned by the `Pipe`.
        let h = unsafe {
            ffi::CreateFileW(
                PCWSTR(wname.as_ptr()),
                ffi::GENERIC_READ | ffi::GENERIC_WRITE,
                0,
                std::ptr::null(),
                ffi::OPEN_EXISTING,
                ffi::SECURITY_SQOS_PRESENT | ffi::SECURITY_IDENTIFICATION,
                HANDLE::default(),
            )
        };
        if !h.is_invalid() {
            return Ok(Pipe(h));
        }
        match last_code() {
            ffi::ERROR_FILE_NOT_FOUND => return Err(io::Error::from(io::ErrorKind::NotFound)),
            ffi::ERROR_PIPE_BUSY => {
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "the pipe stayed busy"));
                }
                // At least 1 ms: 0 would mean the pipe's default time-out.
                let ms = left.as_millis().clamp(1, u128::from(u32::MAX - 1)) as u32;
                // SAFETY: `wname` is NUL-terminated; the call only waits.
                let freed = unsafe { ffi::WaitNamedPipeW(PCWSTR(wname.as_ptr()), ms) };
                if !freed.as_bool() {
                    // Timed out, or returned at once: try again shortly, never in a tight loop.
                    thread::sleep(POLL.min(left));
                }
            }
            _ => return Err(io::Error::other(format!("opening the pipe failed: {}", win::last_error()))),
        }
    }
}

impl IpcTransport for Os {
    type Listener = Listener;
    type Conn = Conn;

    /// `\\.\pipe\Sessionmux.<SID>` for the current user (the SID is looked up once).
    fn endpoint() -> String {
        match win::current_user_sid() {
            Some(sid) => format!(r"\\.\pipe\{}.{}", brand::NAME, sid),
            None => String::new(),
        }
    }

    fn listen(endpoint: &str) -> io::Result<Listener> {
        let sid = win::current_user_sid().ok_or_else(|| io::Error::other("the user SID is not readable"))?;
        if endpoint.is_empty() {
            return Err(io::Error::other("the pipe has no name"));
        }
        let sd = Descriptor::user_only(&sid)?;
        let name = win::wide(endpoint);
        let first = create_instance(&name, &sd, true)?;
        Ok(Listener { name, sd, free: Some(first) })
    }

    fn accept(l: &mut Listener) -> io::Result<Option<Conn>> {
        let pipe = match l.free.take() {
            Some(p) => p,
            None => create_instance(&l.name, &l.sd, false)?,
        };
        // SAFETY: `pipe.0` is a free server instance; synchronous connect, no overlapped structure.
        let ok = unsafe { ffi::ConnectNamedPipe(pipe.0, std::ptr::null_mut()) };
        if !ok.as_bool() {
            match last_code() {
                ffi::ERROR_PIPE_CONNECTED => {}
                ffi::ERROR_NO_DATA => {
                    // The client came and went: reset the instance and keep it for the next one.
                    // SAFETY: `pipe.0` is our server instance; disconnecting makes it free again.
                    let _ = unsafe { ffi::DisconnectNamedPipe(pipe.0) };
                    l.free = Some(pipe);
                    return Ok(None);
                }
                _ => return Err(io::Error::other(format!("accepting a client failed: {}", win::last_error()))),
            }
        }
        // The next client's instance exists before this one is served.
        l.free = match create_instance(&l.name, &l.sd, false) {
            Ok(p) => Some(p),
            Err(e) => {
                log::warn(&format!("pipe: {e}"));
                None
            }
        };
        Ok(Some(Conn { pipe, server: true }))
    }

    fn connect(endpoint: &str, deadline: Instant) -> io::Result<Conn> {
        let pipe = open_client(endpoint, deadline)?;
        let mut pid = 0u32;
        // SAFETY: `pipe.0` is a connected client handle; `pid` is a valid out-pointer.
        let ok = unsafe { ffi::GetNamedPipeServerProcessId(pipe.0, &mut pid) };
        let me = win::current_user_sid();
        let server = if ok.as_bool() && pid != 0 { win::process_user_sid(pid) } else { None };
        match (me, server) {
            (Some(a), Some(b)) if a == b => Ok(Conn { pipe, server: false }),
            _ => Err(io::Error::new(io::ErrorKind::PermissionDenied, "the pipe is not served by this user")),
        }
    }

    fn readable(c: &Conn, timeout: Duration) -> io::Result<Readiness> {
        let until = Instant::now() + timeout;
        loop {
            match peek(&c.pipe)? {
                Peek::Data(n) => return Ok(Readiness::Data(n as usize)),
                Peek::Closed => return Ok(Readiness::Gone),
                Peek::Empty => {
                    let left = until.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Ok(Readiness::Idle);
                    }
                    thread::sleep(POLL.min(left));
                }
            }
        }
    }

    #[cfg(test)]
    fn test_endpoint() -> String {
        format!("{}.test-{}", Os::endpoint(), uuid::Uuid::new_v4().simple())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_per_user() {
        let n = Os::endpoint();
        assert!(n.starts_with(r"\\.\pipe\Sessionmux.S-1-"), "{n}");
        assert_eq!(n, Os::endpoint());
        assert_ne!(Os::test_endpoint(), Os::test_endpoint());
    }

    #[test]
    fn only_one_server_per_name() {
        let name = Os::test_endpoint();
        let first = Os::listen(&name).unwrap();
        assert!(Os::listen(&name).is_err());
        drop(first);
        assert!(Os::listen(&name).is_ok());
        assert!(Os::listen("").is_err());
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

    #[test]
    fn a_busy_pipe_is_waited_for_until_the_deadline() {
        let name = Os::test_endpoint();
        let sid = win::current_user_sid().unwrap();
        let sd = Descriptor::user_only(&sid).unwrap();
        let wname = win::wide(&name);
        // The only instance is taken by another client: the pipe is busy.
        let only = create_instance(&wname, &sd, true).unwrap();
        let other = open_client(&name, Instant::now() + Duration::from_secs(5)).unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let freed = thread::spawn(move || {
            // A free instance turns up only after a few seconds.
            thread::sleep(Duration::from_millis(3300));
            let next = create_instance(&wname, &sd, false).unwrap();
            let _ = rx.recv_timeout(Duration::from_secs(10));
            drop(next);
        });
        let started = Instant::now();
        let got = open_client(&name, Instant::now() + Duration::from_secs(10));
        assert!(got.is_ok(), "{:?} after {:?}", got.err(), started.elapsed());
        let _ = tx.send(());
        freed.join().unwrap();
        drop((other, only));
        // Busy until the deadline: a clear error once it passes, not before.
        let only = create_instance(&win::wide(&name), &Descriptor::user_only(&sid).unwrap(), true).unwrap();
        let other = open_client(&name, Instant::now() + Duration::from_secs(5)).unwrap();
        let started = Instant::now();
        let got = open_client(&name, Instant::now() + Duration::from_millis(1500));
        assert_eq!(got.err().map(|e| e.kind()), Some(io::ErrorKind::TimedOut));
        assert!(started.elapsed() >= Duration::from_millis(1400), "{:?}", started.elapsed());
        drop((other, only));
    }
}
