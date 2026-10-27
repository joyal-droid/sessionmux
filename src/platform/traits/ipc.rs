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

//! The per-user byte stream between the hook helpers and the running app, and the
//! single-instance guard. Framing and the protocol live in `core::pipe`; a backend only moves
//! bytes and checks who is at the other end.

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

/// What is waiting on a connection right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    /// This many bytes can be read without blocking (at least 1).
    Data(usize),
    /// Nothing yet; the other end is still connected.
    Idle,
    /// The other end closed the connection.
    Gone,
}

/// The transport under `core::pipe`. An endpoint is a string the backend understands (a pipe
/// name on Windows, a socket path on macOS); the empty string is an endpoint nobody serves.
pub trait IpcTransport {
    /// A listening endpoint, moved to the accept thread.
    type Listener: Send;
    /// One connected client, from either side. Reads return `Ok(0)` once the other end has
    /// closed; writes to a closed connection fail with `io::ErrorKind::BrokenPipe`. Dropping a
    /// connection closes it.
    type Conn: Read + Write + Send;

    /// The current user's endpoint; empty when it cannot be worked out (then no server starts
    /// and every client finds nobody).
    fn endpoint() -> String;

    /// Start serving `endpoint` for the current user only. Fails when another server already
    /// holds it.
    fn listen(endpoint: &str) -> io::Result<Self::Listener>;

    /// Wait for the next client. `Ok(None)` when a client came and went before it was accepted
    /// (the caller simply waits again). A client that runs as another user is never returned.
    fn accept(l: &mut Self::Listener) -> io::Result<Option<Self::Conn>>;

    /// Connect to `endpoint`, waiting for a busy endpoint until `deadline`, and make sure the
    /// server runs as the current user. `io::ErrorKind::NotFound` when nobody serves it,
    /// `io::ErrorKind::TimedOut` when it stayed busy until the deadline; anything else is a
    /// server the client may not use.
    fn connect(endpoint: &str, deadline: Instant) -> io::Result<Self::Conn>;

    /// What is waiting on `c`, waiting up to `timeout` for data or a close (`Duration::ZERO`
    /// looks once).
    fn readable(c: &Self::Conn, timeout: Duration) -> io::Result<Readiness>;

    /// A fresh endpoint of the current user's that nothing serves yet.
    #[cfg(test)]
    fn test_endpoint() -> String;
}

/// One running app per user session.
pub trait SingleInstance {
    /// Held for as long as this process is the running app; dropping it lets the next one start.
    type Guard;

    /// The guard, or `None` when another instance already runs. When the guard itself cannot be
    /// set up, the app runs unguarded (the failure is logged) rather than not at all.
    fn acquire() -> Option<Self::Guard>;

    /// Ask the running instance to show its panel; true when the request was handed over.
    fn notify_existing() -> bool;

    /// True while some instance holds the guard. Never takes it.
    fn running() -> bool;
}
