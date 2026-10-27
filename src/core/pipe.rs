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

//! The per-user channel between the hook helpers and the running app: the protocol, its framing
//! and the server and clients, over the system's transport (`platform::ipc`: a named pipe on
//! Windows, a Unix socket on macOS).
//!
//! API:
//! ```ignore
//! pub fn endpoint() -> String                                // \\.\pipe\Sessionmux.<user SID> on Windows
//! pub enum Request { Notify { .. }, Approve { .. } }
//! pub enum Decision { Allow, Deny { message: Option<String> } }
//! impl Server { pub fn start(on_request: Box<dyn Fn(Request, Responder) + Send>) -> Result<Server>; pub fn stop(self) }
//! impl Server { pub fn start_with_show(on_request, on_show: Box<dyn Fn() + Send>) -> Result<Server> }
//! impl PipeError { pub fn is_connect(&self) -> bool }                // the client never reached the app
//! impl Responder { pub fn decide(self, d: Decision) -> Result<()>; pub fn is_gone(&self) -> bool }   // dropped = the client is told nothing
//! pub fn client_send_notify(req: &Request, deadline: Instant) -> Result<()>
//! pub fn client_approve(req: &Request, deadline: Instant) -> Result<Option<Decision>>
//! pub fn client_show(deadline: Instant) -> Result<()>
//! ```
//!
//! Only the current user can serve or reach the endpoint, and a client talks only to a server
//! that runs as the same user (see the transport). One accept thread waits for clients and hands
//! each to its own reader thread; it leaves only when the server stops. Messages are
//! newline-delimited JSON of at most [`MAX_MESSAGE`] bytes. A notify request is answered with
//! nothing; an approve request keeps its client connected until the app decides, the client
//! disconnects (the user answered in Claude) or [`APPROVE_EXPIRY`] passes. A show request
//! (`{"kind":"show"}`, sent by a second launch) asks the app to show its panel and is answered
//! with nothing; a server started without `on_show` ignores it. `on_request` and `on_show` run on
//! the server thread, one request at a time; the app posts each request to its UI thread. A
//! decision only ever goes back to the client that asked, as
//! `{"requestId":…,"decision":"allow"|"deny","message"?:…}`.
//!
//! Every client call ends by its `deadline`: the notify acknowledgement and the wait for a
//! decision share it. Connecting waits for a busy endpoint until the deadline, and for at least
//! [`BUSY_MIN`] even when the deadline is closer.

#![allow(dead_code)]

use crate::platform::{ipc, IpcTransport, Readiness};
use crate::util::log;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fmt;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Largest message accepted in either direction.
pub const MAX_MESSAGE: usize = 64 * 1024;
/// An approve request nobody answered is dropped after this.
pub const APPROVE_EXPIRY: Duration = Duration::from_secs(90);

/// How long a connected client may take to send its request.
const REQUEST_DEADLINE: Duration = Duration::from_secs(5);
/// How long a notify client waits for the app to take its message.
const NOTIFY_ACK_DEADLINE: Duration = Duration::from_secs(2);
/// A client waits at least this long for a busy endpoint, whatever its deadline.
pub const BUSY_MIN: Duration = Duration::from_secs(1);
/// Pause before the accept thread tries again after a failure.
const CREATE_RETRY: Duration = Duration::from_millis(500);
/// How long `Responder::decide` waits for the decision to be written.
const DECIDE_DEADLINE: Duration = Duration::from_secs(2);
/// Polling step while waiting for data, a decision or a disconnect.
const POLL: Duration = Duration::from_millis(10);
const DECISION_POLL: Duration = Duration::from_millis(50);

/// The system's transport.
type Transport = ipc::Os;
/// One connected client of the transport.
type Conn = <Transport as IpcTransport>::Conn;

/// Why a pipe operation did not complete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PipeError {
    /// The app is not running (no endpoint to connect to).
    NotRunning,
    /// Creating, connecting, reading or writing failed.
    Io(String),
    /// A message was oversized or not valid JSON.
    Malformed,
    /// The client of a request has disconnected, or the request expired or was never waiting.
    Gone,
    /// Not available in this build.
    NotImplemented,
    /// The endpoint stayed busy until the client's deadline.
    Busy,
    /// The endpoint exists but could not be opened, or it is not served by this user.
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
    /// The client never reached the app: no endpoint, one that stayed busy, or one it may not use.
    pub fn is_connect(&self) -> bool {
        matches!(self, PipeError::NotRunning | PipeError::Busy | PipeError::Connect(_))
    }
}

pub type Result<T> = std::result::Result<T, PipeError>;

/// The current user's endpoint (on Windows `\\.\pipe\Sessionmux.<SID>`, the SID looked up once).
/// Empty when it cannot be worked out: then no server starts and every client reports
/// `NotRunning`.
pub fn endpoint() -> String {
    Transport::endpoint()
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

/// The line a second launch sends to ask the running app to show its panel.
const SHOW_LINE: &[u8] = b"{\"kind\":\"show\"}\n";

/// True for a show request: a JSON object whose `kind` is `show`.
fn is_show(line: &[u8]) -> bool {
    serde_json::from_slice::<Value>(line).ok().and_then(|v| v.get("kind")?.as_str().map(|k| k == "show")) == Some(true)
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
// Framing

/// Outcome of a read that did not produce a full line.
#[derive(Debug, PartialEq, Eq)]
enum ReadErr {
    /// The other end closed the connection.
    Closed,
    Timeout,
    TooLong,
    Stopped,
    Io(String),
}

/// Read up to `n` bytes that are known to be waiting and append them to `out`.
fn read_into(c: &mut Conn, n: usize, out: &mut Vec<u8>) -> std::result::Result<(), ReadErr> {
    let mut buf = vec![0u8; n];
    match c.read(&mut buf) {
        Ok(0) => Err(ReadErr::Closed),
        Ok(got) => {
            out.extend_from_slice(&buf[..got.min(buf.len())]);
            Ok(())
        }
        Err(e) => Err(ReadErr::Io(e.to_string())),
    }
}

/// Read one newline-terminated message (without the newline) of at most [`MAX_MESSAGE`] bytes.
/// Data that arrives in the same read after the newline is ignored. A message the other end
/// closes without a newline still counts when it is not empty.
fn read_line(c: &mut Conn, deadline: Instant, stop: Option<&AtomicBool>) -> std::result::Result<Vec<u8>, ReadErr> {
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
        let wait = POLL.min(deadline.saturating_duration_since(Instant::now()));
        match Transport::readable(c, wait) {
            Ok(Readiness::Data(n)) => {
                let room = (MAX_MESSAGE + 2).saturating_sub(out.len()).max(1);
                read_into(c, n.clamp(1, room), &mut out)?;
            }
            Ok(Readiness::Idle) => {
                if Instant::now() >= deadline {
                    return Err(ReadErr::Timeout);
                }
            }
            Ok(Readiness::Gone) => {
                return if out.is_empty() { Err(ReadErr::Closed) } else { Ok(out) };
            }
            Err(e) => return Err(ReadErr::Io(e.to_string())),
        }
    }
}

fn write_all(c: &mut Conn, mut bytes: &[u8]) -> Result<()> {
    while !bytes.is_empty() {
        match c.write(bytes) {
            Ok(0) => return Err(PipeError::Io("pipe write made no progress".into())),
            Ok(put) => bytes = &bytes[put.min(bytes.len())..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => return Err(PipeError::Gone),
            Err(e) => return Err(PipeError::Io(e.to_string())),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Server

/// The running server.
pub struct Server {
    name: String,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
    dispatch: Option<JoinHandle<()>>,
}

/// What a reader thread hands to the dispatch thread.
enum Incoming {
    Hook(Request, Responder),
    Show,
}

type OnRequest = Box<dyn Fn(Request, Responder) + Send>;
type OnShow = Box<dyn Fn() + Send>;

impl Server {
    /// Start serving the current user's endpoint; `on_request` gets every request with the handle
    /// to answer it. Show requests are ignored.
    pub fn start(on_request: OnRequest) -> Result<Server> {
        Server::start_at(&endpoint(), on_request)
    }

    /// [`Server::start`], and `on_show` runs for every show request.
    pub fn start_with_show(on_request: OnRequest, on_show: OnShow) -> Result<Server> {
        Server::serve(&endpoint(), on_request, Some(on_show))
    }

    /// [`Server::start`] on a given endpoint.
    pub(crate) fn start_at(name: &str, on_request: OnRequest) -> Result<Server> {
        Server::serve(name, on_request, None)
    }

    fn serve(name: &str, on_request: OnRequest, on_show: Option<OnShow>) -> Result<Server> {
        let listener = Transport::listen(name).map_err(|e| PipeError::Io(e.to_string()))?;
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<Incoming>();

        let dstop = stop.clone();
        let dispatch = thread::Builder::new()
            .name("sessionmux-pipe".into())
            .spawn(move || dispatch_loop(rx, on_request, on_show, dstop))
            .map_err(|e| PipeError::Io(format!("pipe thread: {e}")))?;

        let astop = stop.clone();
        let accept = thread::Builder::new()
            .name("sessionmux-pipe-accept".into())
            .spawn(move || accept_loop(listener, tx, astop))
            .map_err(|e| PipeError::Io(format!("pipe thread: {e}")))?;

        log::info("pipe: listening");
        Ok(Server { name: name.to_string(), stop, accept: Some(accept), dispatch: Some(dispatch) })
    }

    /// Stop accepting clients and close the endpoint; pending approve clients are told nothing.
    pub fn stop(self) {}

    fn shutdown(&mut self) {
        if self.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(accept) = self.accept.take() else { return };
        // The accept thread waits for a client: connect once ourselves to wake it.
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut woke = false;
        while Instant::now() < deadline {
            if accept.is_finished() || Transport::connect(&self.name, deadline).is_ok() {
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

fn dispatch_loop(rx: mpsc::Receiver<Incoming>, on_request: OnRequest, on_show: Option<OnShow>, stop: Arc<AtomicBool>) {
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(incoming) => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let handled = match incoming {
                    Incoming::Hook(req, resp) => {
                        let call = std::panic::AssertUnwindSafe(|| on_request(req, resp));
                        std::panic::catch_unwind(call).is_ok()
                    }
                    Incoming::Show => match &on_show {
                        Some(f) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_ok(),
                        None => {
                            log::debug("pipe: show request ignored");
                            true
                        }
                    },
                };
                if !handled {
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

fn accept_loop(mut listener: <Transport as IpcTransport>::Listener, tx: mpsc::Sender<Incoming>, stop: Arc<AtomicBool>) {
    loop {
        let accepted = Transport::accept(&mut listener);
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let conn = match accepted {
            Ok(Some(c)) => c,
            // The client came and went before it was accepted.
            Ok(None) => continue,
            Err(e) => {
                log::warn(&format!("pipe: {e}"));
                if sleep_unless_stopped(CREATE_RETRY, &stop) {
                    return;
                }
                continue;
            }
        };
        let ctx = tx.clone();
        let cstop = stop.clone();
        if let Err(e) =
            thread::Builder::new().name("sessionmux-pipe-client".into()).spawn(move || serve_client(conn, ctx, cstop))
        {
            log::warn(&format!("pipe: client thread: {e}"));
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

fn serve_client(mut conn: Conn, tx: mpsc::Sender<Incoming>, stop: Arc<AtomicBool>) {
    let line = match read_line(&mut conn, Instant::now() + REQUEST_DEADLINE, Some(&stop)) {
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
        Err(_) if is_show(&line) => {
            let _ = tx.send(Incoming::Show);
            return;
        }
        Err(_) => {
            log::warn("pipe: malformed message refused");
            return;
        }
    };
    let request_id = match &req {
        Request::Notify { .. } => {
            let _ = tx.send(Incoming::Hook(req, Responder { tx: None, gone: Arc::new(AtomicBool::new(true)) }));
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
    if tx.send(Incoming::Hook(req, Responder { tx: Some(dtx), gone })).is_err() {
        return;
    }
    drop(tx);
    let expires = Instant::now() + APPROVE_EXPIRY;
    loop {
        match drx.recv_timeout(DECISION_POLL) {
            Ok((decision, reply)) => {
                let r = write_decision(&mut conn, &request_id, &decision);
                let ok = r.is_ok();
                let _ = reply.send(r);
                if ok {
                    // Let the client read the decision before the server end goes away.
                    let _ = conn.flush();
                }
                return;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if stop.load(Ordering::Relaxed) || Instant::now() >= expires {
                    return;
                }
                match Transport::readable(&conn, Duration::ZERO) {
                    Ok(Readiness::Gone) | Err(_) => return,
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

fn write_decision(conn: &mut Conn, request_id: &str, decision: &Decision) -> Result<()> {
    write_all(conn, &decision_line(request_id, decision))
}

/// Answers one request on the connection it came in on. Dropping it without deciding tells the
/// client nothing (the helper then exits silently and Claude's own prompt decides).
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

/// Connect to `name` as a client and make sure the server runs as this user. `NotRunning` when
/// nobody serves it, `Busy` when it stayed busy until `deadline` (waited for at least
/// [`BUSY_MIN`]).
fn connect(name: &str, deadline: Instant) -> Result<Conn> {
    if name.is_empty() {
        return Err(PipeError::NotRunning);
    }
    Transport::connect(name, deadline).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => PipeError::NotRunning,
        io::ErrorKind::TimedOut => PipeError::Busy,
        _ => PipeError::Connect(e.to_string()),
    })
}

fn encode(req: &Request) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(req).map_err(|_| PipeError::Malformed)?;
    if bytes.len() > MAX_MESSAGE {
        return Err(PipeError::Malformed);
    }
    bytes.push(b'\n');
    Ok(bytes)
}

/// Write `bytes` and wait until the server has taken them (it closes its end), by `deadline`.
fn send_and_wait_close(name: &str, bytes: &[u8], deadline: Instant) -> Result<()> {
    let mut conn = connect(name, deadline)?;
    write_all(&mut conn, bytes)?;
    // The server closes its end once it has taken the message.
    let _ = read_line(&mut conn, deadline.min(Instant::now() + NOTIFY_ACK_DEADLINE), None);
    Ok(())
}

/// Send a notify request to the running app; returns once it is written, by `deadline`.
pub fn client_send_notify(req: &Request, deadline: Instant) -> Result<()> {
    client_send_notify_at(&endpoint(), req, deadline)
}

/// [`client_send_notify`] on a given endpoint.
pub(crate) fn client_send_notify_at(name: &str, req: &Request, deadline: Instant) -> Result<()> {
    if !matches!(req, Request::Notify { .. }) {
        return Err(PipeError::Malformed);
    }
    let bytes = encode(req)?;
    send_and_wait_close(name, &bytes, deadline)
}

/// Ask the running app to show its panel; returns once the request is written, by `deadline`.
pub fn client_show(deadline: Instant) -> Result<()> {
    client_show_at(&endpoint(), deadline)
}

/// [`client_show`] on a given endpoint.
pub(crate) fn client_show_at(name: &str, deadline: Instant) -> Result<()> {
    send_and_wait_close(name, SHOW_LINE, deadline)
}

/// Send an approve request and wait until `deadline` at the latest for the decision.
/// `Ok(None)` = no decision in time or the app dropped the request.
pub fn client_approve(req: &Request, deadline: Instant) -> Result<Option<Decision>> {
    client_approve_at(&endpoint(), req, deadline)
}

/// [`client_approve`] on a given endpoint.
pub(crate) fn client_approve_at(name: &str, req: &Request, deadline: Instant) -> Result<Option<Decision>> {
    let Request::Approve { request_id, .. } = req else { return Err(PipeError::Malformed) };
    let bytes = encode(req)?;
    let mut conn = connect(name, deadline)?;
    write_all(&mut conn, &bytes)?;
    let line = match read_line(&mut conn, deadline, None) {
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

/// A fresh endpoint of the current user's that nothing serves yet.
#[cfg(test)]
pub(crate) fn test_endpoint() -> String {
    Transport::test_endpoint()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    fn test_name() -> String {
        test_endpoint()
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
    fn endpoint_is_stable() {
        assert!(!endpoint().is_empty());
        assert_eq!(endpoint(), endpoint());
        assert_ne!(test_name(), test_name());
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
    fn show_wire_shape() {
        assert_eq!(SHOW_LINE.last(), Some(&b'\n'));
        let v: Value = serde_json::from_slice(SHOW_LINE).unwrap();
        assert_eq!(v, json!({ "kind": "show" }));
        assert!(is_show(&SHOW_LINE[..SHOW_LINE.len() - 1]));
        assert!(is_show(br#"{"kind":"show","extra":1}"#));
        // Never a hook request, and nothing else reads as one.
        assert!(serde_json::from_slice::<Request>(SHOW_LINE).is_err());
        assert!(!is_show(br#"{"kind":"notify"}"#));
        assert!(!is_show(br#"{"kind":"SHOW"}"#));
        assert!(!is_show(br#""show""#));
        assert!(!is_show(b"show"));
    }

    #[test]
    fn show_reaches_on_show_only() {
        let name = test_name();
        let shows = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(AtomicUsize::new(0));
        let (s, r) = (shows.clone(), requests.clone());
        let server = Server::serve(
            &name,
            Box::new(move |_, _| {
                r.fetch_add(1, Ordering::SeqCst);
            }),
            Some(Box::new(move || {
                s.fetch_add(1, Ordering::SeqCst);
            })),
        )
        .unwrap();
        assert_eq!(client_show_at(&name, Instant::now() + Duration::from_secs(5)), Ok(()));
        assert_eq!(client_show_at(&name, Instant::now() + Duration::from_secs(5)), Ok(()));
        let deadline = Instant::now() + Duration::from_secs(5);
        while shows.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(shows.load(Ordering::SeqCst), 2);
        // The server still serves hook requests afterwards.
        assert_eq!(client_send_notify_at(&name, &notify(), Instant::now() + Duration::from_secs(5)), Ok(()));
        let deadline = Instant::now() + Duration::from_secs(5);
        while requests.load(Ordering::SeqCst) < 1 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(shows.load(Ordering::SeqCst), 2);
        server.stop();
        assert_eq!(client_show_at(&name, Instant::now() + Duration::from_secs(5)), Err(PipeError::NotRunning));
    }

    #[test]
    fn show_without_a_handler_is_ignored() {
        let name = test_name();
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let server = Server::start_at(
            &name,
            Box::new(move |_, _| {
                c.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .unwrap();
        assert_eq!(client_show_at(&name, Instant::now() + Duration::from_secs(5)), Ok(()));
        let id = new_id();
        let got = client_approve_at(&name, &approve(&id), Instant::now() + Duration::from_millis(300)).unwrap();
        assert_eq!(got, None);
        let deadline = Instant::now() + Duration::from_secs(5);
        while calls.load(Ordering::SeqCst) < 1 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        // Only the approve request reached `on_request`.
        thread::sleep(Duration::from_millis(100));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        server.stop();
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

        let mut conn = connect(&name, Instant::now() + Duration::from_secs(5)).unwrap();
        let mut big = vec![b'x'; MAX_MESSAGE + 10];
        big.push(b'\n');
        let _ = write_all(&mut conn, &big);
        assert_eq!(read_line(&mut conn, Instant::now() + Duration::from_secs(5), None), Err(ReadErr::Closed));

        let mut conn = connect(&name, Instant::now() + Duration::from_secs(5)).unwrap();
        write_all(&mut conn, b"{\"kind\":\"approve\"}\n").unwrap();
        assert_eq!(read_line(&mut conn, Instant::now() + Duration::from_secs(5), None), Err(ReadErr::Closed));

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
        assert_eq!(client_show_at(&name, Instant::now() + Duration::from_secs(1)), Err(PipeError::NotRunning));
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
}
