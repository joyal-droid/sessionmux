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

//! The per-user named pipe between the hook helpers and the running app.
//!
//! API:
//! ```ignore
//! pub fn pipe_name() -> String                               // \\.\pipe\Sessionmux.<user SID>
//! pub enum Request { Notify { .. }, Approve { .. } }
//! pub enum Decision { Allow, Deny { message: Option<String> } }
//! impl Server { pub fn start(on_request: Box<dyn Fn(Request, Responder) + Send>) -> Result<Server>; pub fn stop(self) }
//! impl PipeError { pub fn is_connect(&self) -> bool }                // the client never reached the app
//! impl Responder { pub fn decide(self, d: Decision) -> Result<()>; pub fn is_gone(&self) -> bool }   // dropped = the client is told nothing
//! pub fn client_send_notify(req: &Request, deadline: Instant) -> Result<()>
//! pub fn client_approve(req: &Request, deadline: Instant) -> Result<Option<Decision>>
//! ```
//!
//! The pipe is created by the running app with a security descriptor that grants the current
//! user only (`D:P(A;;GA;;;<SID>)`), in message mode, local clients only; the first instance is
//! created with `FILE_FLAG_FIRST_PIPE_INSTANCE`, so the app never serves a name somebody else
//! already holds. Every client gets its own instance (`PIPE_UNLIMITED_INSTANCES`): one accept
//! thread waits on a free instance, creates the next free instance as soon as a client
//! connects, and only then hands the client to its own reader thread, so the name always has
//! an instance waiting while the server lives. The accept thread leaves only when the server
//! stops; an instance is disconnected and closed once its client is done. Messages are newline-delimited JSON of at most [`MAX_MESSAGE`] bytes. A notify
//! request is answered with nothing; an approve request keeps its client connected until the app
//! decides, the client disconnects (the user answered in Claude) or [`APPROVE_EXPIRY`] passes.
//! `on_request` runs on the server thread, one request at a time; the app posts each request to
//! its UI thread. A decision only ever goes back to the client that asked, as
//! `{"requestId":…,"decision":"allow"|"deny","message"?:…}`.
//!
//! Every client call ends by its `deadline`: the notify acknowledgement and the wait for a
//! decision share it. Connecting waits for a busy pipe (`WaitNamedPipeW`) until the deadline,
//! and for at least [`BUSY_MIN`] even when the deadline is closer.
//!
//! Clients connect with identification-only impersonation and talk only to a server process that
//! runs as the same user.

#![allow(dead_code)]

use crate::brand;
use crate::util::log;
use crate::util::win::{self, ffi};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, LocalFree, HANDLE, HLOCAL};
use windows::Win32::Security::SECURITY_ATTRIBUTES;

/// Largest message accepted in either direction.
pub const MAX_MESSAGE: usize = 64 * 1024;
/// An approve request nobody answered is dropped after this.
pub const APPROVE_EXPIRY: Duration = Duration::from_secs(90);

/// How long a connected client may take to send its request.
const REQUEST_DEADLINE: Duration = Duration::from_secs(5);
/// How long a notify client waits for the app to take its message.
const NOTIFY_ACK_DEADLINE: Duration = Duration::from_secs(2);
/// A client waits at least this long for a busy pipe, whatever its deadline.
pub const BUSY_MIN: Duration = Duration::from_secs(1);
/// Pause before the accept thread tries again to create an instance.
const CREATE_RETRY: Duration = Duration::from_millis(500);
/// How long `Responder::decide` waits for the decision to be written.
const DECIDE_DEADLINE: Duration = Duration::from_secs(2);
/// Polling step while waiting for data, a decision or a disconnect.
const POLL: Duration = Duration::from_millis(10);
const DECISION_POLL: Duration = Duration::from_millis(50);

/// Why a pipe operation did not complete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PipeError {
    /// The app is not running (no pipe to connect to).
    NotRunning,
    /// Creating, connecting, reading or writing failed.
    Io(String),
    /// A message was oversized or not valid JSON.
    Malformed,
    /// The client of a request has disconnected, or the request expired or was never waiting.
    Gone,
    /// Not available in this build.
    NotImplemented,
    /// Every instance of the pipe stayed taken until the client's deadline.
    Busy,
    /// The pipe exists but could not be opened, or it is not served by this user.
    Connect(String),
}

impl fmt::Display for PipeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PipeError::NotRunning => write!(f, "Sessionmux is not running"),
            PipeError::Io(m) => write!(f, "{m}"),
            PipeError::Malformed => write!(f, "malformed message"),
            PipeError::Gone => write!(f, "the request is no longer waiting"),
            PipeError::NotImplemented => write!(f, "not implemented"),
            PipeError::Busy => write!(f, "the pipe stayed busy"),
            PipeError::Connect(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for PipeError {}

impl PipeError {
    /// The client never reached the app: no pipe, a pipe that stayed busy, or one it may not use.
    pub fn is_connect(&self) -> bool {
        matches!(self, PipeError::NotRunning | PipeError::Busy | PipeError::Connect(_))
    }
}

pub type Result<T> = std::result::Result<T, PipeError>;

/// `\\.\pipe\Sessionmux.<SID>` for the current user (the SID is looked up once). Empty when the
/// SID cannot be read: then no server starts and every client reports `NotRunning`.
pub fn pipe_name() -> String {
    match win::current_user_sid() {
        Some(sid) => format!(r"\\.\pipe\{}.{}", brand::NAME, sid),
        None => String::new(),
    }
}

/// One message from a hook helper. Times are unix seconds.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Request {
    /// `--notify-hook`: a `Notification` or `StopFailure` event.
    Notify {
        /// The hook event name (`Notification` or `StopFailure`).
        event: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notification_type: Option<String>,
        session_id: String,
        cwd: String,
        /// The event's message text, at most 2 KB.
        message: String,
        time: u64,
    },
    /// `--approve-hook`: a `PermissionRequest` waiting for Allow or Deny.
    Approve {
        /// Random 128-bit hex id, used once.
        #[serde(rename = "requestId")]
        request_id: String,
        session_id: String,
        cwd: String,
        tool_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mcp_server: Option<String>,
        /// What the tool is about to do, at most 4 KB.
        #[serde(rename = "input_summary")]
        summary: String,
        time: u64,
    },
}

/// The answer to an approve request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// Deny, with the user's note if any.
    Deny {
        message: Option<String>,
    },
}

// ---------------------------------------------------------------------------------------------
// Handles

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

/// Outcome of a read that did not produce a full line.
#[derive(Debug, PartialEq, Eq)]
enum ReadErr {
    /// The other end closed the pipe.
    Closed,
    Timeout,
    TooLong,
    Stopped,
    Io(String),
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

impl Pipe {
    fn peek(&self) -> std::result::Result<Peek, ReadErr> {
        let mut avail = 0u32;
        // SAFETY: `self.0` is an open pipe handle; no buffer is passed, only the count out-pointer.
        let ok = unsafe {
            ffi::PeekNamedPipe(self.0, std::ptr::null_mut(), 0, std::ptr::null_mut(), &mut avail, std::ptr::null_mut())
        };
        if ok.as_bool() {
            return Ok(if avail > 0 { Peek::Data(avail) } else { Peek::Empty });
        }
        let code = last_code();
        if is_disconnect(code) {
            Ok(Peek::Closed)
        } else {
            Err(ReadErr::Io(win::last_error()))
        }
    }

    /// Read up to `n` bytes that are known to be waiting and append them to `out`.
    fn read_into(&self, n: u32, out: &mut Vec<u8>) -> std::result::Result<(), ReadErr> {
        let mut buf = vec![0u8; n as usize];
        let mut got = 0u32;
        // SAFETY: `buf` has `n` writable bytes; `got` is a valid out-pointer; synchronous call.
        let ok = unsafe { ffi::ReadFile(self.0, buf.as_mut_ptr().cast(), n, &mut got, std::ptr::null_mut()) };
        if !ok.as_bool() {
            let code = last_code();
            if is_disconnect(code) {
                return Err(ReadErr::Closed);
            }
            // A message longer than the buffer: the rest is read by the next call.
            if code != ffi::ERROR_MORE_DATA {
                return Err(ReadErr::Io(win::last_error()));
            }
        }
        out.extend_from_slice(&buf[..(got as usize).min(buf.len())]);
        Ok(())
    }

    /// Read one newline-terminated message (without the newline) of at most [`MAX_MESSAGE`]
    /// bytes. Data that arrives in the same read after the newline is ignored. A message the
    /// other end closes without a newline still counts when it is not empty.
    fn read_line(&self, deadline: Instant, stop: Option<&AtomicBool>) -> std::result::Result<Vec<u8>, ReadErr> {
        let mut out = Vec::new();
        loop {
            if let Some(i) = out.iter().position(|&b| b == b'\n') {
                out.truncate(i);
                return if out.len() > MAX_MESSAGE { Err(ReadErr::TooLong) } else { Ok(out) };
            }
            if out.len() > MAX_MESSAGE {
                return Err(ReadErr::TooLong);
            }
            if stop.is_some_and(|s| s.load(Ordering::Relaxed)) {
                return Err(ReadErr::Stopped);
            }
            match self.peek()? {
                Peek::Data(n) => {
                    let room = (MAX_MESSAGE + 2).saturating_sub(out.len()).max(1) as u32;
                    self.read_into(n.min(room), &mut out)?;
                }
                Peek::Empty => {
                    if Instant::now() >= deadline {
                        return Err(ReadErr::Timeout);
                    }
                    thread::sleep(POLL);
                }
                Peek::Closed => {
                    return if out.is_empty() { Err(ReadErr::Closed) } else { Ok(out) };
                }
            }
        }
    }

    fn write_all(&self, mut bytes: &[u8]) -> Result<()> {
        while !bytes.is_empty() {
            let mut put = 0u32;
            let n = bytes.len().min(MAX_MESSAGE + 1) as u32;
            // SAFETY: `bytes` has at least `n` readable bytes; `put` is a valid out-pointer.
            let ok = unsafe { ffi::WriteFile(self.0, bytes.as_ptr().cast(), n, &mut put, std::ptr::null_mut()) };
            if !ok.as_bool() {
                let code = last_code();
                return Err(if is_disconnect(code) { PipeError::Gone } else { PipeError::Io(win::last_error()) });
            }
            if put == 0 {
                return Err(PipeError::Io("pipe write made no progress".into()));
            }
            bytes = &bytes[(put as usize).min(bytes.len())..];
        }
        Ok(())
    }
}

/// A security descriptor built from SDDL; freed on drop.
struct Descriptor(*mut core::ffi::c_void);

// SAFETY: the descriptor is plain memory owned by this value; it is only read by
// CreateNamedPipeW on the thread that currently owns it.
unsafe impl Send for Descriptor {}

impl Descriptor {
    /// Full access for `sid`, nothing for anyone else, no inherited entries.
    fn user_only(sid: &str) -> Result<Descriptor> {
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
            return Err(PipeError::Io(format!("pipe security descriptor: {}", win::last_error())));
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

fn create_instance(name: &[u16], sd: &Descriptor, first: bool) -> Result<Pipe> {
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
        return Err(PipeError::Io(format!("creating the pipe failed: {}", win::last_error())));
    }
    Ok(Pipe(h))
}

/// How waiting for a client on an instance ended.
#[derive(Debug, PartialEq, Eq)]
enum Accept {
    Connected,
    /// The client came and went before it was accepted; the instance can be reset and reused.
    Left,
    Failed,
}

/// Wait for a client on `pipe`.
fn accept(pipe: &Pipe) -> Accept {
    // SAFETY: `pipe.0` is a free server instance; synchronous connect, no overlapped structure.
    let ok = unsafe { ffi::ConnectNamedPipe(pipe.0, std::ptr::null_mut()) };
    if ok.as_bool() {
        return Accept::Connected;
    }
    match last_code() {
        ffi::ERROR_PIPE_CONNECTED => Accept::Connected,
        ffi::ERROR_NO_DATA => Accept::Left,
        _ => Accept::Failed,
    }
}

/// Disconnects a server instance from its client when dropped (before the handle closes), so
/// the instance never lingers half-closed.
struct Disconnect(HANDLE);

impl Drop for Disconnect {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a server instance handle that is still open (its `Pipe` is dropped
        // after this guard); disconnecting an instance without a client is harmless.
        let _ = unsafe { ffi::DisconnectNamedPipe(self.0) };
    }
}

// ---------------------------------------------------------------------------------------------
// Server

/// The running pipe server.
pub struct Server {
    name: String,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
    dispatch: Option<JoinHandle<()>>,
}

type Incoming = (Request, Responder);

impl Server {
    /// Create the pipe and start the server thread; `on_request` gets every request with the
    /// handle to answer it.
    pub fn start(on_request: Box<dyn Fn(Request, Responder) + Send>) -> Result<Server> {
        let name = pipe_name();
        if name.is_empty() {
            return Err(PipeError::Io("the user SID is not readable".into()));
        }
        Server::start_at(&name, on_request)
    }

    /// [`Server::start`] on a given pipe name.
    pub(crate) fn start_at(name: &str, on_request: Box<dyn Fn(Request, Responder) + Send>) -> Result<Server> {
        let sid = win::current_user_sid().ok_or_else(|| PipeError::Io("the user SID is not readable".into()))?;
        let sd = Descriptor::user_only(&sid)?;
        let wname = win::wide(name);
        let first = create_instance(&wname, &sd, true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<Incoming>();

        let dstop = stop.clone();
        let dispatch = thread::Builder::new()
            .name("sessionmux-pipe".into())
            .spawn(move || dispatch_loop(rx, on_request, dstop))
            .map_err(|e| PipeError::Io(format!("pipe thread: {e}")))?;

        let astop = stop.clone();
        let accept = thread::Builder::new()
            .name("sessionmux-pipe-accept".into())
            .spawn(move || accept_loop(wname, sd, first, tx, astop))
            .map_err(|e| PipeError::Io(format!("pipe thread: {e}")))?;

        log::info("pipe: listening");
        Ok(Server { name: name.to_string(), stop, accept: Some(accept), dispatch: Some(dispatch) })
    }

    /// Stop accepting clients and close the pipe; pending approve clients are told nothing.
    pub fn stop(self) {}

    fn shutdown(&mut self) {
        if self.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(accept) = self.accept.take() else { return };
        // The accept thread waits in ConnectNamedPipe: connect once ourselves to wake it.
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut woke = false;
        while Instant::now() < deadline {
            if accept.is_finished() || open_client(&self.name, deadline).is_ok() {
                woke = true;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        if woke {
            let _ = accept.join();
        }
        if let Some(d) = self.dispatch.take() {
            if woke {
                let _ = d.join();
            }
        }
        log::info("pipe: stopped");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn dispatch_loop(
    rx: mpsc::Receiver<Incoming>,
    on_request: Box<dyn Fn(Request, Responder) + Send>,
    stop: Arc<AtomicBool>,
) {
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok((req, resp)) => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let call = std::panic::AssertUnwindSafe(|| on_request(req, resp));
                if std::panic::catch_unwind(call).is_err() {
                    log::error("pipe: request handler panicked");
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn accept_loop(name: Vec<u16>, sd: Descriptor, first: Pipe, tx: mpsc::Sender<Incoming>, stop: Arc<AtomicBool>) {
    let mut free = Some(first);
    loop {
        let pipe = match free.take() {
            Some(p) => p,
            None => match create_retrying(&name, &sd, &stop) {
                Some(p) => p,
                None => return,
            },
        };
        let accepted = accept(&pipe);
        if stop.load(Ordering::Relaxed) {
            return;
        }
        match accepted {
            Accept::Connected => {}
            Accept::Left => {
                // SAFETY: `pipe.0` is our server instance; disconnecting makes it free again.
                let _ = unsafe { ffi::DisconnectNamedPipe(pipe.0) };
                free = Some(pipe);
                continue;
            }
            Accept::Failed => {
                log::warn(&format!("pipe: accepting a client failed: {}", win::last_error()));
                drop(pipe);
                if sleep_unless_stopped(CREATE_RETRY, &stop) {
                    return;
                }
                continue;
            }
        }
        // The next client's instance exists before this one is served.
        free = match create_instance(&name, &sd, false) {
            Ok(p) => Some(p),
            Err(e) => {
                log::warn(&format!("pipe: {e}"));
                None
            }
        };
        let ctx = tx.clone();
        let cstop = stop.clone();
        if let Err(e) =
            thread::Builder::new().name("sessionmux-pipe-client".into()).spawn(move || serve_client(pipe, ctx, cstop))
        {
            log::warn(&format!("pipe: client thread: {e}"));
        }
    }
}

/// A new free instance, retried until it is created; `None` once the server stops.
fn create_retrying(name: &[u16], sd: &Descriptor, stop: &AtomicBool) -> Option<Pipe> {
    loop {
        match create_instance(name, sd, false) {
            Ok(p) => return Some(p),
            Err(e) => log::warn(&format!("pipe: {e}")),
        }
        if sleep_unless_stopped(CREATE_RETRY, stop) {
            return None;
        }
    }
}

/// Sleep for `d` in short steps; true as soon as `stop` is set.
fn sleep_unless_stopped(d: Duration, stop: &AtomicBool) -> bool {
    let until = Instant::now() + d;
    while Instant::now() < until {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    stop.load(Ordering::Relaxed)
}

/// An approve request id: 16 to 64 hex digits.
fn valid_request_id(id: &str) -> bool {
    (16..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit())
}

fn serve_client(pipe: Pipe, tx: mpsc::Sender<Incoming>, stop: Arc<AtomicBool>) {
    // However this thread ends, the instance is disconnected before it is closed.
    let _disconnect = Disconnect(pipe.0);
    let line = match pipe.read_line(Instant::now() + REQUEST_DEADLINE, Some(&stop)) {
        Ok(l) => l,
        Err(ReadErr::TooLong) => {
            log::warn("pipe: oversized message refused");
            return;
        }
        Err(ReadErr::Io(e)) => {
            log::warn(&format!("pipe: read failed: {e}"));
            return;
        }
        Err(_) => return,
    };
    let req: Request = match serde_json::from_slice(&line) {
        Ok(r) => r,
        Err(_) => {
            log::warn("pipe: malformed message refused");
            return;
        }
    };
    let request_id = match &req {
        Request::Notify { .. } => {
            let _ = tx.send((req, Responder { tx: None, gone: Arc::new(AtomicBool::new(true)) }));
            return;
        }
        Request::Approve { request_id, .. } => request_id.clone(),
    };
    if !valid_request_id(&request_id) {
        log::warn("pipe: approve request with a bad id refused");
        return;
    }
    let (dtx, drx) = mpsc::channel::<(Decision, mpsc::Sender<Result<()>>)>();
    let gone = Arc::new(AtomicBool::new(false));
    // However this thread ends (decided, disconnected, expired, stopped), the responder knows.
    let _done = Done(gone.clone());
    if tx.send((req, Responder { tx: Some(dtx), gone })).is_err() {
        return;
    }
    drop(tx);
    let expires = Instant::now() + APPROVE_EXPIRY;
    loop {
        match drx.recv_timeout(DECISION_POLL) {
            Ok((decision, reply)) => {
                let r = write_decision(&pipe, &request_id, &decision);
                let ok = r.is_ok();
                let _ = reply.send(r);
                if ok {
                    // Let the client read the decision before the server end goes away.
                    // SAFETY: `pipe.0` is the connected server instance owned by this thread.
                    let _ = unsafe { ffi::FlushFileBuffers(pipe.0) };
                }
                return;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if stop.load(Ordering::Relaxed) || Instant::now() >= expires {
                    return;
                }
                match pipe.peek() {
                    Ok(Peek::Closed) | Err(_) => return,
                    Ok(_) => {}
                }
            }
        }
    }
}

/// Marks the request's client as gone when its reader thread ends.
struct Done(Arc<AtomicBool>);

impl Drop for Done {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// The decision line sent to the client.
fn decision_line(request_id: &str, decision: &Decision) -> Vec<u8> {
    let mut v = match decision {
        Decision::Allow => json!({ "requestId": request_id, "decision": "allow" }),
        Decision::Deny { message } => {
            let mut v = json!({ "requestId": request_id, "decision": "deny" });
            if let Some(m) = message {
                v["message"] = Value::String(clip(m, 2048).to_string());
            }
            v
        }
    };
    let mut bytes = serde_json::to_vec(&v).unwrap_or_default();
    if bytes.len() > MAX_MESSAGE {
        if let Some(o) = v.as_object_mut() {
            o.remove("message");
        }
        bytes = serde_json::to_vec(&v).unwrap_or_default();
    }
    bytes.push(b'\n');
    bytes
}

fn write_decision(pipe: &Pipe, request_id: &str, decision: &Decision) -> Result<()> {
    pipe.write_all(&decision_line(request_id, decision))
}

/// Answers one request on the client handle it came in on. Dropping it without deciding tells
/// the client nothing (the helper then exits silently and Claude's own prompt decides).
pub struct Responder {
    /// The reader thread of the waiting client; `None` for a notify request.
    tx: Option<mpsc::Sender<(Decision, mpsc::Sender<Result<()>>)>>,
    /// Set once nobody waits for the answer any more.
    gone: Arc<AtomicBool>,
}

impl Responder {
    /// True once the client that asked is no longer waiting: it disconnected (the user answered
    /// in Claude, or the helper gave up), the request expired or the server stopped.
    pub fn is_gone(&self) -> bool {
        self.gone.load(Ordering::SeqCst)
    }

    /// Send `decision` to the client that asked. `Err(Gone)` when that client is gone, the
    /// request expired, or it was a notify request.
    pub fn decide(self, decision: Decision) -> Result<()> {
        let Some(tx) = self.tx else { return Err(PipeError::Gone) };
        let (rtx, rrx) = mpsc::channel();
        if tx.send((decision, rtx)).is_err() {
            return Err(PipeError::Gone);
        }
        match rrx.recv_timeout(DECIDE_DEADLINE) {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(PipeError::Gone),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(PipeError::Io("writing the decision timed out".into())),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Clients

/// Open the pipe as a client. A busy pipe is waited for until `deadline`, and for at least
/// [`BUSY_MIN`]. `NotRunning` when nobody serves it, `Busy` when no instance came free in time.
fn open_client(name: &str, deadline: Instant) -> Result<Pipe> {
    if name.is_empty() {
        return Err(PipeError::NotRunning);
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
            ffi::ERROR_FILE_NOT_FOUND => return Err(PipeError::NotRunning),
            ffi::ERROR_PIPE_BUSY => {
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(PipeError::Busy);
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
            _ => return Err(PipeError::Connect(format!("opening the pipe failed: {}", win::last_error()))),
        }
    }
}

/// Open the pipe and make sure the process serving it runs as this user.
fn connect(name: &str, deadline: Instant) -> Result<Pipe> {
    let pipe = open_client(name, deadline)?;
    let mut pid = 0u32;
    // SAFETY: `pipe.0` is a connected client handle; `pid` is a valid out-pointer.
    let ok = unsafe { ffi::GetNamedPipeServerProcessId(pipe.0, &mut pid) };
    let me = win::current_user_sid();
    let server = if ok.as_bool() && pid != 0 { win::process_user_sid(pid) } else { None };
    match (me, server) {
        (Some(a), Some(b)) if a == b => Ok(pipe),
        _ => Err(PipeError::Connect("the pipe is not served by this user".into())),
    }
}

fn encode(req: &Request) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(req).map_err(|_| PipeError::Malformed)?;
    if bytes.len() > MAX_MESSAGE {
        return Err(PipeError::Malformed);
    }
    bytes.push(b'\n');
    Ok(bytes)
}

/// Send a notify request to the running app; returns once it is written, by `deadline`.
pub fn client_send_notify(req: &Request, deadline: Instant) -> Result<()> {
    client_send_notify_at(&pipe_name(), req, deadline)
}

/// [`client_send_notify`] on a given pipe name.
pub(crate) fn client_send_notify_at(name: &str, req: &Request, deadline: Instant) -> Result<()> {
    if !matches!(req, Request::Notify { .. }) {
        return Err(PipeError::Malformed);
    }
    let bytes = encode(req)?;
    let pipe = connect(name, deadline)?;
    pipe.write_all(&bytes)?;
    // The server closes its end once it has taken the message.
    let _ = pipe.read_line(deadline.min(Instant::now() + NOTIFY_ACK_DEADLINE), None);
    Ok(())
}

/// Send an approve request and wait until `deadline` at the latest for the decision.
/// `Ok(None)` = no decision in time or the app dropped the request.
pub fn client_approve(req: &Request, deadline: Instant) -> Result<Option<Decision>> {
    client_approve_at(&pipe_name(), req, deadline)
}

/// [`client_approve`] on a given pipe name.
pub(crate) fn client_approve_at(name: &str, req: &Request, deadline: Instant) -> Result<Option<Decision>> {
    let Request::Approve { request_id, .. } = req else { return Err(PipeError::Malformed) };
    let bytes = encode(req)?;
    let pipe = connect(name, deadline)?;
    pipe.write_all(&bytes)?;
    let line = match pipe.read_line(deadline, None) {
        Ok(l) => l,
        Err(ReadErr::Closed) | Err(ReadErr::Timeout) | Err(ReadErr::Stopped) => return Ok(None),
        Err(ReadErr::TooLong) => return Err(PipeError::Malformed),
        Err(ReadErr::Io(e)) => return Err(PipeError::Io(e)),
    };
    parse_decision(&line, request_id).map(Some)
}

/// The decision in a reply line, which must name `request_id`.
fn parse_decision(line: &[u8], request_id: &str) -> Result<Decision> {
    let v: Value = serde_json::from_slice(line).map_err(|_| PipeError::Malformed)?;
    if v.get("requestId").and_then(Value::as_str) != Some(request_id) {
        return Err(PipeError::Malformed);
    }
    match v.get("decision").and_then(Value::as_str) {
        Some("allow") => Ok(Decision::Allow),
        Some("deny") => {
            let message = v.get("message").and_then(Value::as_str).map(str::to_string);
            Ok(Decision::Deny { message })
        }
        _ => Err(PipeError::Malformed),
    }
}

/// `s` cut to at most `max` bytes on a character boundary.
fn clip(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn test_name() -> String {
        format!("{}.test-{}", pipe_name(), uuid::Uuid::new_v4().simple())
    }

    fn approve(id: &str) -> Request {
        Request::Approve {
            request_id: id.to_string(),
            session_id: "11111111-2222-3333-4444-555555555555".into(),
            cwd: r"C:\work\demo".into(),
            tool_name: "Bash".into(),
            mcp_server: None,
            summary: "cargo test".into(),
            time: 1_792_990_287,
        }
    }

    fn notify() -> Request {
        Request::Notify {
            event: "Notification".into(),
            notification_type: Some("idle_prompt".into()),
            session_id: "11111111-2222-3333-4444-555555555555".into(),
            cwd: r"C:\work\demo".into(),
            message: "Claude is waiting for your input".into(),
            time: 1_792_990_287,
        }
    }

    fn new_id() -> String {
        uuid::Uuid::new_v4().simple().to_string()
    }

    #[test]
    fn name_is_per_user() {
        let n = pipe_name();
        assert!(n.starts_with(r"\\.\pipe\Sessionmux.S-1-"), "{n}");
        assert_eq!(n, pipe_name());
    }

    #[test]
    fn wire_shape() {
        let v: Value = serde_json::to_value(approve("ab")).unwrap();
        assert_eq!(v["kind"], "approve");
        assert_eq!(v["requestId"], "ab");
        assert_eq!(v["input_summary"], "cargo test");
        assert!(v.get("mcp_server").is_none());
        let v: Value = serde_json::to_value(notify()).unwrap();
        assert_eq!(v["kind"], "notify");
        assert_eq!(v["notification_type"], "idle_prompt");
        let line = decision_line("ab", &Decision::Deny { message: Some("no".into()) });
        assert_eq!(line.last(), Some(&b'\n'));
        assert_eq!(parse_decision(&line[..line.len() - 1], "ab"), Ok(Decision::Deny { message: Some("no".into()) }));
        assert_eq!(parse_decision(&line[..line.len() - 1], "cd"), Err(PipeError::Malformed));
        assert!(valid_request_id(&new_id()));
        assert!(!valid_request_id("xyz"));
    }

    #[test]
    fn round_trip_notify_and_approve() {
        let name = test_name();
        let seen: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));
        let rec = seen.clone();
        let notify_gone = Arc::new(AtomicBool::new(false));
        let ng = notify_gone.clone();
        let server = Server::start_at(
            &name,
            Box::new(move |req, resp| {
                let is_approve = matches!(req, Request::Approve { .. });
                rec.lock().unwrap().push(req);
                if is_approve {
                    thread::spawn(move || {
                        assert_eq!(resp.decide(Decision::Allow), Ok(()));
                    });
                } else {
                    // A notify request has nobody waiting for an answer.
                    ng.store(resp.decide(Decision::Allow) == Err(PipeError::Gone), Ordering::SeqCst);
                }
            }),
        )
        .unwrap();

        // Only one server per name.
        assert!(Server::start_at(&name, Box::new(|_, _| {})).is_err());

        assert_eq!(client_send_notify_at(&name, &notify(), Instant::now() + Duration::from_secs(5)), Ok(()));
        let id = new_id();
        let got = client_approve_at(&name, &approve(&id), Instant::now() + Duration::from_secs(10)).unwrap();
        assert_eq!(got, Some(Decision::Allow));

        let deadline = Instant::now() + Duration::from_secs(5);
        while seen.lock().unwrap().len() < 2 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen, vec![notify(), approve(&id)]);
        assert!(notify_gone.load(Ordering::SeqCst));

        server.stop();
        assert_eq!(
            client_send_notify_at(&name, &notify(), Instant::now() + Duration::from_secs(5)),
            Err(PipeError::NotRunning)
        );
    }

    #[test]
    fn deny_carries_the_note() {
        let name = test_name();
        let server = Server::start_at(
            &name,
            Box::new(|_, resp| {
                thread::spawn(move || resp.decide(Decision::Deny { message: Some("not now".into()) }));
            }),
        )
        .unwrap();
        let got = client_approve_at(&name, &approve(&new_id()), Instant::now() + Duration::from_secs(10)).unwrap();
        assert_eq!(got, Some(Decision::Deny { message: Some("not now".into()) }));
        server.stop();
    }

    #[test]
    fn decide_after_client_left_is_gone() {
        let name = test_name();
        let (tx, rx) = mpsc::channel::<Responder>();
        let tx = Mutex::new(tx);
        let server = Server::start_at(
            &name,
            Box::new(move |_, resp| {
                let _ = tx.lock().unwrap().send(resp);
            }),
        )
        .unwrap();
        // The client gives up (as when the user answers in Claude) and disconnects.
        let got = client_approve_at(&name, &approve(&new_id()), Instant::now() + Duration::from_millis(300)).unwrap();
        assert_eq!(got, None);
        let resp = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        // The server notices the disconnect on its own, without a decide.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !resp.is_gone() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(resp.is_gone());
        assert_eq!(resp.decide(Decision::Allow), Err(PipeError::Gone));
        server.stop();
    }

    #[test]
    fn dropped_responder_tells_the_client_nothing() {
        let name = test_name();
        let server = Server::start_at(&name, Box::new(|_, resp| drop(resp))).unwrap();
        let got = client_approve_at(&name, &approve(&new_id()), Instant::now() + Duration::from_secs(5)).unwrap();
        assert_eq!(got, None);
        server.stop();
    }

    #[test]
    fn oversized_and_malformed_messages_are_refused() {
        let name = test_name();
        let calls = Arc::new(Mutex::new(0usize));
        let c = calls.clone();
        let server = Server::start_at(&name, Box::new(move |_, _| *c.lock().unwrap() += 1)).unwrap();

        let pipe = connect(&name, Instant::now() + Duration::from_secs(5)).unwrap();
        let mut big = vec![b'x'; MAX_MESSAGE + 10];
        big.push(b'\n');
        let _ = pipe.write_all(&big);
        assert_eq!(pipe.read_line(Instant::now() + Duration::from_secs(5), None), Err(ReadErr::Closed));

        let pipe = connect(&name, Instant::now() + Duration::from_secs(5)).unwrap();
        pipe.write_all(b"{\"kind\":\"approve\"}\n").unwrap();
        assert_eq!(pipe.read_line(Instant::now() + Duration::from_secs(5), None), Err(ReadErr::Closed));

        let mut bad = approve("not-hex");
        if let Request::Approve { request_id, .. } = &mut bad {
            *request_id = "zz".into();
        }
        assert_eq!(client_approve_at(&name, &bad, Instant::now() + Duration::from_secs(5)), Ok(None));

        let mut huge = approve(&new_id());
        if let Request::Approve { summary, .. } = &mut huge {
            *summary = "y".repeat(MAX_MESSAGE);
        }
        assert_eq!(client_approve_at(&name, &huge, Instant::now() + Duration::from_secs(1)), Err(PipeError::Malformed));

        assert_eq!(*calls.lock().unwrap(), 0);
        server.stop();
    }

    #[test]
    fn no_server_is_not_running() {
        let name = test_name();
        assert_eq!(
            client_send_notify_at(&name, &notify(), Instant::now() + Duration::from_secs(5)),
            Err(PipeError::NotRunning)
        );
        assert_eq!(
            client_approve_at(&name, &approve(&new_id()), Instant::now() + Duration::from_secs(1)),
            Err(PipeError::NotRunning)
        );
        assert_eq!(
            client_send_notify_at("", &notify(), Instant::now() + Duration::from_secs(5)),
            Err(PipeError::NotRunning)
        );
    }

    #[test]
    fn clip_keeps_char_boundaries() {
        assert_eq!(clip("abc", 5), "abc");
        assert_eq!(clip("a✓b", 2), "a");
        assert_eq!(clip("a✓b", 4), "a✓");
    }

    /// An approve request id that starts with `a`.
    fn allow_id() -> String {
        format!("a{}", &new_id()[1..])
    }

    /// A server that answers every approve request with Allow, or Deny for ids starting with `d`.
    fn deciding_server(name: &str) -> Server {
        Server::start_at(
            name,
            Box::new(|req, resp| {
                let Request::Approve { request_id, .. } = req else { return };
                thread::spawn(move || {
                    let d = if request_id.starts_with('d') {
                        Decision::Deny { message: Some(request_id) }
                    } else {
                        Decision::Allow
                    };
                    let _ = resp.decide(d);
                });
            }),
        )
        .unwrap()
    }

    #[test]
    fn sequential_approve_clients_each_get_their_decision() {
        let name = test_name();
        let server = deciding_server(&name);
        for _ in 0..3 {
            let got =
                client_approve_at(&name, &approve(&allow_id()), Instant::now() + Duration::from_secs(10)).unwrap();
            assert_eq!(got, Some(Decision::Allow));
        }
        let deny = format!("d{}", new_id());
        let got = client_approve_at(&name, &approve(&deny), Instant::now() + Duration::from_secs(10)).unwrap();
        assert_eq!(got, Some(Decision::Deny { message: Some(deny) }));
        server.stop();
    }

    #[test]
    fn overlapping_approve_clients_each_get_their_own_decision() {
        let name = test_name();
        let (tx, rx) = mpsc::channel::<(Request, Responder)>();
        let tx = Mutex::new(tx);
        let server = Server::start_at(
            &name,
            Box::new(move |req, resp| {
                let _ = tx.lock().unwrap().send((req, resp));
            }),
        )
        .unwrap();
        let (a, b) = (new_id(), format!("d{}", new_id()));
        let spawn_client = |id: String| {
            let name = name.clone();
            thread::spawn(move || client_approve_at(&name, &approve(&id), Instant::now() + Duration::from_secs(20)))
        };
        let ca = spawn_client(a.clone());
        let cb = spawn_client(b.clone());
        // Both requests wait at the same time before either is answered.
        let first = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let second = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        for (req, resp) in [second, first] {
            let Request::Approve { request_id, .. } = req else { panic!("not an approve request") };
            let d = if request_id == b { Decision::Deny { message: Some("b".into()) } } else { Decision::Allow };
            assert_eq!(resp.decide(d), Ok(()));
        }
        assert_eq!(ca.join().unwrap(), Ok(Some(Decision::Allow)));
        assert_eq!(cb.join().unwrap(), Ok(Some(Decision::Deny { message: Some("b".into()) })));
        server.stop();
    }

    #[test]
    fn notify_after_a_timed_out_approve_reaches_the_server() {
        let name = test_name();
        let seen: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));
        let held: Arc<Mutex<Vec<Responder>>> = Arc::new(Mutex::new(Vec::new()));
        let (rec, keep) = (seen.clone(), held.clone());
        let server = Server::start_at(
            &name,
            Box::new(move |req, resp| {
                rec.lock().unwrap().push(req);
                // Nobody decides: the approve client runs into its own deadline.
                keep.lock().unwrap().push(resp);
            }),
        )
        .unwrap();
        let id = new_id();
        let got = client_approve_at(&name, &approve(&id), Instant::now() + Duration::from_millis(400)).unwrap();
        assert_eq!(got, None);
        assert_eq!(client_send_notify_at(&name, &notify(), Instant::now() + Duration::from_secs(5)), Ok(()));
        let id2 = new_id();
        let got = client_approve_at(&name, &approve(&id2), Instant::now() + Duration::from_millis(400)).unwrap();
        assert_eq!(got, None);
        let deadline = Instant::now() + Duration::from_secs(5);
        while seen.lock().unwrap().len() < 3 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(*seen.lock().unwrap(), vec![approve(&id), notify(), approve(&id2)]);
        server.stop();
    }

    #[test]
    fn server_survives_a_client_that_sends_nothing() {
        let name = test_name();
        let server = deciding_server(&name);
        for _ in 0..2 {
            let silent = connect(&name, Instant::now() + Duration::from_secs(5)).unwrap();
            drop(silent);
        }
        // One that stays connected without sending, while another client is served.
        let idle = connect(&name, Instant::now() + Duration::from_secs(5)).unwrap();
        let got = client_approve_at(&name, &approve(&allow_id()), Instant::now() + Duration::from_secs(10)).unwrap();
        assert_eq!(got, Some(Decision::Allow));
        drop(idle);
        let got = client_approve_at(&name, &approve(&allow_id()), Instant::now() + Duration::from_secs(10)).unwrap();
        assert_eq!(got, Some(Decision::Allow));
        server.stop();
    }

    #[test]
    fn a_busy_pipe_is_waited_for_until_the_deadline() {
        let name = test_name();
        let sid = win::current_user_sid().unwrap();
        let sd = Descriptor::user_only(&sid).unwrap();
        let wname = win::wide(&name);
        // The only instance is taken by another client: the pipe is busy.
        let only = create_instance(&wname, &sd, true).unwrap();
        let other = open_client(&name, Instant::now() + Duration::from_secs(5)).unwrap();
        let (tx, rx) = mpsc::channel::<()>();
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
        assert_eq!(got.err(), Some(PipeError::Busy));
        assert!(started.elapsed() >= Duration::from_millis(1400), "{:?}", started.elapsed());
        drop((other, only));
    }
}
