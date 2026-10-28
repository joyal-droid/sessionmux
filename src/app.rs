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

//! Application state and orchestration.
//!
//! Owns `AppState` in an `Rc<RefCell<>>` on the UI thread (thread-local) and implements every
//! `window.sessionmux` method with its validation and results: the 3 s poll thread + diffing, the active-account logic, bubble / tray / hotkey events, the
//! panel lifecycle (blur-hide unless pinned, destroyed 30 s after hiding), settings side effects
//! and quit cleanup.
//!
//! Re-entrancy rules (the reason for the shape of this file):
//! - Every entry point (panel IPC, bubble events, poll results, worker results) is *posted* to
//!   the UI thread through `platform::EventLoop::post`, so it runs from the top-level loop.
//! - `Panel::create` pumps messages and `Tray::handle` runs a modal menu loop: neither is ever
//!   called while `AppState` is borrowed. The tray lives in its own `Rc<RefCell<Tray>>` so
//!   menu updates that arrive during the modal loop are applied afterwards (`tray_dirty`).
//! - Blocking work (process enumeration, launching, focusing, killing, session file copies)
//!   runs on `std::thread::spawn` and posts its result back.
//!
//! Smoke mode (`SMOKE`): launch / focus / kill / MCP sync / start-with-Windows / shell opens are
//! short-circuited; no hotkey, no tray icon, no poll thread, and the sign-in relay never touches
//! the `claude://` handler and never begins.
//!
//! Sign-in routing: opening an extra account that is not signed in starts a
//! `core::relay` 1.5 s after its window is up; `TimerId::SignIn` (every `relay::POLL_MS`) keeps our
//! `claude://` handler in place and ends the relay when the profile signs in, its Claude exits or
//! the relay times out; a restore that waits for the user's answer keeps the timer until it
//! settles. Every handler change (take, check, restore) runs on the relay worker, one at a time
//! in the order asked, because it may wait for a system prompt; the UI thread only applies the
//! results. Quitting restores the handler where changes ask nobody, and otherwise leaves the
//! restore to the next start. A link the system hands to Sessionmux (`PlatformEvent::OpenUrl`)
//! goes to the account signing in, else to Main. IPC `getSignIn` / `cancelSignIn` /
//! `restoreLinkHandler`, event `signin-changed`; `removeChanges` takes out the hooks, the link
//! handler and the login item.
//!
//! Tamper response (cosmetic, always on, no setting): a debugger attaching (checked 3 s after
//! start, then every 5 s on the poll thread, once per attach), a forged task message on the
//! event loop, or a `--relay` launch whose command line shows argument injection (both arrive as
//! `PlatformEvent::Gesture`) plays `ui::gesture` beside the bubble, each kind at most once per
//! 10 s (the debugger kind has its own limiter), logged as `tamper: <reason>`. Never in smoke
//! mode, never while exiting, never for a debugger in the first 3 s (`gesture::armed`).
//!
//! Waiting sessions, approvals and plan limits: the hook helpers reach the app over
//! `core::pipe` (started at init, outside smoke mode); an approve request is kept in `approvals`
//! (with its `Responder`) until the user decides in the panel, answers in Claude or it expires
//! (90 s); `waiting` holds the sessions that wait for the user (30 min at most), `limited` the
//! accounts that hit their plan limit. Each poll rebuilds, on a worker, an index of the running
//! accounts' sessions by CLI session id (the id the helpers send) and scans the tail of every
//! running session's transcript that changed since the last scan for a limit hit. With
//! `autoHandover` on, a limit signal hands one running session of that account over (once per
//! limit window). Approve requests are only shown while `approveFromPanel` is on; otherwise the
//! helper is told nothing and Claude's own prompt decides. IPC `listApprovals` /
//! `decideApproval`, `listWaiting`, `handOverSession`, `getHooksStatus` / `previewHooks` /
//! `installHooks` / `removeHooks`.
//!
//! Entry points used by main.rs: `init(hidden)`, `show_panel()`, `toggle_panel()`, `quit()`,
//! `shutdown()`. Everything the system reports reaches [`on_event`]. smoke.rs uses the `smoke_*`
//! helpers at the end.

use crate::brand;
use crate::core::accounts::{Account, AccountsStore, ApiError, Discovered, Point, Settings};
use crate::core::claude_app::{self, Located, Proc};
use crate::core::hooks;
use crate::core::limits::{self, LimitHit, LimitKind, LimitSource, Limited};
use crate::core::pipe::{self, Decision, Request, Responder, Server};
use crate::core::relay::{self, Relay};
use crate::core::sessions::{
    pick_handover_target, valid_session_id, AccountView, SessionAccount, Sessions, TransferResult,
};
use crate::platform::{
    self, Autostart, BubbleHost, Closer, Debugger, Desktop, EventLoop, Focus, FocusResult, ForwardError, GestureHost,
    HotkeyHost, Launcher, Misc, Os, PlatformEvent, PrivacyPane, Processes, Rect, Shell, TimerId, TrayHost, UrlHandler,
    WindowId, Windows,
};
use crate::platform::{bubble::Bubble, tray::Tray};
use crate::ui::bubble::{BubbleAccount, BubbleEvent, BubbleMode, BubbleState};
use crate::ui::gesture::{self, Reason};
use crate::ui::ipc::{IpcHandler, IpcRequest};
use crate::ui::panel::Panel;
use crate::ui::tray::{TrayEvent, TrayMenu};
use crate::util::{json as ujson, log, paths};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime};

/// Smoke mode: launch / focus / kill are short-circuited.
pub static SMOKE: AtomicBool = AtomicBool::new(false);
/// Smoke mode on the demo fixture: the home folder is the fixture's, so the hook status may be read.
static SMOKE_FIXTURE: AtomicBool = AtomicBool::new(false);
/// Set once quit was requested (stops the poll thread).
static QUITTING: AtomicBool = AtomicBool::new(false);

/// Id of the global hotkey.
pub const HOTKEY_ID: u32 = 1;

const POLL_MS: u64 = 3000;
/// First debugger check after start, then the interval between checks.
const DEBUGGER_FIRST: Duration = Duration::from_secs(3);
const DEBUGGER_EVERY: Duration = Duration::from_secs(5);
const FOCUS_WAIT_MS: u64 = 15_000;
const PIN_MS: u32 = 10_000;
const BLUR_TOGGLE_GRACE: Duration = Duration::from_millis(300);
const PANEL_DESTROY_MS: u32 = 30_000;
const TRAY_MAX_ACCOUNTS: usize = 3;
const OPEN_ALL_STAGGER: Duration = Duration::from_millis(400);
/// macOS: the panel stays up while an open from it runs, and a failed open from anywhere shows
/// the panel with its reason (Claude taking the activation would otherwise hide the panel first).
const SURFACE_OPEN_FAILURES: bool = cfg!(target_os = "macos");

/// Claude registers its `claude://` handler at startup: begin the relay this long after the
/// account's window is up, so our value is the one that stays.
const SIGNIN_BEGIN_DELAY: Duration = Duration::from_millis(1500);
/// A relay whose account was never seen running for this long ends ("process gone").
const SIGNIN_NEVER_RUNNING: Duration = Duration::from_secs(30);
/// Quitting waits this long for the relay worker to give the handler back.
const EXIT_RESTORE_WAIT: Duration = Duration::from_secs(5);
/// One-shot trim after startup: the first D2D render and the tray pull in system DLL pages
/// (d2d1, D3D10Warp, shell32...) that sit in the working set until trimmed.
const STARTUP_TRIM_MS: u32 = 2500;

const TABS: [&str; 3] = ["accounts", "sessions", "settings"];

/// At most this many approve requests are kept; further ones are left to Claude's own prompt.
const MAX_APPROVALS: usize = 50;
/// A session stops counting as waiting after this long without news (seconds).
const WAITING_EXPIRY_SECS: u64 = 30 * 60;
/// Transcript writes this soon after a waiting signal are part of the same turn (seconds).
const PROGRESS_GRACE_SECS: u64 = 3;
/// `agent_completed` this soon after a limit hit is the end of the same turn (seconds).
const COMPLETED_GRACE_SECS: u64 = 10;
/// A limit signal for a session the index does not know yet is kept this long (seconds).
const UNMAPPED_LIMIT_SECS: u64 = 60;
/// With an unknown reset time, automatic hand-over happens at most once per this window (seconds).
const HANDOVER_QUIET_SECS: u64 = 30 * 60;
/// How long a hand-over waits for the target's Claude to exit before opening it again.
const RESTART_WAIT: Duration = Duration::from_secs(10);

pub fn is_smoke() -> bool {
    SMOKE.load(Ordering::Relaxed)
}

/// One approve request from `--approve-hook`, shown as a card until it is answered or gone.
pub struct PendingApproval {
    /// The helper's one-shot request id.
    pub id: String,
    /// Empty until the session is found in the index.
    pub account_id: String,
    pub account_name: String,
    /// The Code session (record id), once found in the index.
    pub session_id: Option<String>,
    /// Claude Code's session id, as the helper sent it.
    pub cli_session_id: String,
    pub session_title: Option<String>,
    pub cwd: String,
    pub tool_name: String,
    pub mcp_server: Option<String>,
    /// What the tool is about to do (built by the helper, at most 4 KB).
    pub summary: String,
    /// Unix seconds.
    pub received_at: u64,
    /// Answers the helper; `None` once answered.
    pub responder: Option<Responder>,
}

/// Why a session waits for the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitingKind {
    /// A permission prompt is open.
    Permission,
    /// Claude finished and waits for the next prompt.
    Idle,
    /// Claude asked the user something.
    NeedsInput,
}

impl WaitingKind {
    /// The panel's name for it.
    pub fn as_str(self) -> &'static str {
        match self {
            WaitingKind::Permission => "permission",
            WaitingKind::Idle => "idle",
            WaitingKind::NeedsInput => "needs_input",
        }
    }
}

/// A session waiting for the user, and since when (unix seconds).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Waiting {
    pub kind: WaitingKind,
    pub since: u64,
}

/// A Code session of a running account, found by the CLI session id the hook helpers send.
#[derive(Clone, Debug, PartialEq)]
struct IndexedSession {
    account_id: String,
    /// The Code session (record id).
    session_id: String,
    title: String,
    running: bool,
    last_activity_at: u64,
}

/// What the last scan saw of one transcript.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TranscriptSeen {
    mtime: SystemTime,
    /// Its newest reply was a limit hit.
    hit: bool,
}

/// The last automatic hand-over of an account: the limit window it was for, and when.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HandoverMark {
    until: Option<u64>,
    at: u64,
}

pub struct AppState {
    pub store: AccountsStore,
    /// `claude://` sign-in relay; handler-free in smoke / overridden user data.
    relay: Relay,
    pub located: Option<Located>,
    pub sessions: Sessions,
    pub bubble: Option<Bubble>,
    pub panel: Option<Panel>,
    pub tray: Option<Rc<RefCell<Tray>>>,
    pub last_accounts: Vec<Account>,
    /// Active account id (the foreground account, else the last one opened).
    pub last_active: Option<String>,
    pub pinned_until: Option<Instant>,

    settings: Settings,
    procs: Vec<Proc>,
    fg_pid: Option<u32>,
    last_opened: Option<String>,
    last_accounts_json: String,
    last_discovered: Vec<Discovered>,
    last_discovered_json: Option<String>,
    last_settings_json: String,
    last_active_json: String,
    /// The Claude process list as last logged.
    last_procs_log: String,
    last_bubble_state: Option<BubbleState>,
    last_blur_hide: Option<Instant>,
    panel_creating: bool,
    tray_dirty: bool,
    poll_wake: Option<mpsc::Sender<()>>,
    page_errors: Vec<String>,
    /// The relay's account was seen running since the relay began (so "not running" = gone).
    relay_seen_running: bool,
    /// When this session began (or resumed) the current relay.
    relay_started: Option<Instant>,
    /// Last `ensure()` error, so a persistent failure is logged once.
    relay_last_error: Option<String>,
    /// The relay worker's queue (started on first use).
    relay_jobs: Option<mpsc::Sender<RelayJob>>,
    /// Relay jobs queued or running; the sign-in timer skips its check while one is.
    relay_busy: u32,
    /// The last link for the account signing in went to Main instead.
    relay_forward_failed: bool,
    /// It went there because the system did not allow Sessionmux to send it.
    relay_automation_denied: bool,
    /// When this instance started (the debugger trigger ignores the first seconds).
    started: Instant,
    /// Forged-message and bad-link triggers acted on at most once per 10 s.
    tamper_limit: gesture::Limiter,
    /// The debugger trigger has its own limiter, so an attach is never swallowed by a recent
    /// forged message or bad link (the poll thread already reports it once per attach).
    debugger_limit: gesture::Limiter,
    /// Approve requests waiting for the user, oldest first.
    approvals: Vec<PendingApproval>,
    /// Sessions waiting for the user, by (account id, session id); the account id is empty and
    /// the session id is Claude Code's own until the session is found in the index.
    waiting: HashMap<(String, String), Waiting>,
    /// Accounts that hit their plan limit, by account id.
    limited: HashMap<String, Limited>,
    /// The hook helpers' pipe (not in smoke mode).
    pipe: Option<Server>,
    /// Code sessions of the running accounts by CLI session id, rebuilt on every poll.
    index: HashMap<String, IndexedSession>,
    /// An index rebuild is running on a worker.
    index_busy: bool,
    /// Transcripts of running sessions as last scanned for a limit hit.
    transcripts: HashMap<PathBuf, TranscriptSeen>,
    /// Limit signals for sessions the index does not know yet: (CLI session id, hit, received).
    unmapped_limits: Vec<(String, LimitHit, u64)>,
    /// Last automatic hand-over per account.
    handover_marks: HashMap<String, HandoverMark>,
    /// Sessions (record ids) being handed over right now.
    handovers: HashSet<String>,
}

thread_local! {
    static APP: RefCell<Option<Rc<RefCell<AppState>>>> = const { RefCell::new(None) };
}

/// The UI thread's app state, if initialised (None on other threads).
pub fn app() -> Option<Rc<RefCell<AppState>>> {
    APP.with(|a| a.borrow().clone())
}

/// Run `f` with the app state borrowed mutably. None if not initialised or already borrowed
/// (which the posting discipline above should make impossible; logged).
fn with_state<R>(f: impl FnOnce(&mut AppState) -> R) -> Option<R> {
    let app = app()?;
    let r = match app.try_borrow_mut() {
        Ok(mut st) => Some(f(&mut st)),
        Err(_) => {
            log::warn("app state busy; request dropped");
            None
        }
    };
    r
}

/// Run `f` on the UI thread, from any thread.
fn post(f: impl FnOnce() + Send + 'static) {
    <Os as EventLoop>::post(Box::new(f));
}

fn set_timer(id: TimerId, ms: u32) {
    <Os as EventLoop>::set_timer(id, ms);
}

fn kill_timer(id: TimerId) {
    <Os as EventLoop>::kill_timer(id);
}

/// One unit of work for the relay worker.
type RelayJob = Box<dyn FnOnce() + Send>;

/// The relay worker: runs its jobs one at a time, in the order they were queued, until the queue
/// is dropped.
fn start_relay_worker() -> Option<mpsc::Sender<RelayJob>> {
    let (tx, rx) = mpsc::channel::<RelayJob>();
    let spawned = std::thread::Builder::new().name("sessionmux-relay".into()).spawn(move || {
        for job in rx {
            job();
        }
    });
    match spawned {
        Ok(_) => Some(tx),
        Err(e) => {
            log::error(&format!("could not start the relay worker: {e}"));
            None
        }
    }
}

/// Queue `job` on the relay worker, starting it when needed. False when it could not be queued.
fn queue_relay_job(st: &mut AppState, job: RelayJob) -> bool {
    if st.relay_jobs.is_none() {
        st.relay_jobs = start_relay_worker();
    }
    let Some(tx) = st.relay_jobs.as_ref() else { return false };
    if tx.send(job).is_err() {
        log::error("relay worker is gone; the handler change was dropped");
        st.relay_jobs = None;
        return false;
    }
    true
}

/// Run `work` on the relay worker, then `then` on the UI thread with the app state. Handler
/// changes may wait for a system prompt, so they never run on the UI thread; one worker runs them
/// in the order asked, so a sign-in's take, checks and restore never overlap.
fn relay_job<T: Send + 'static>(
    st: &mut AppState,
    work: impl FnOnce(&Relay) -> T + Send + 'static,
    then: impl FnOnce(&mut AppState, T) + Send + 'static,
) {
    let rl = st.relay.clone();
    let job: RelayJob = Box::new(move || {
        let r = work(&rl);
        post(move || {
            with_state(|st| {
                st.relay_busy = st.relay_busy.saturating_sub(1);
                then(st, r);
            });
        });
    });
    if queue_relay_job(st, job) {
        st.relay_busy += 1;
    }
}

/// Run `work` on the relay worker after the jobs already queued and wait up to `limit` for its
/// result. Only for quitting, where nothing else would apply the result. None when it did not
/// finish in time.
fn relay_job_wait<T: Send + 'static>(
    st: &mut AppState,
    work: impl FnOnce(&Relay) -> T + Send + 'static,
    limit: Duration,
) -> Option<T> {
    let (tx, rx) = mpsc::sync_channel(1);
    let rl = st.relay.clone();
    let job: RelayJob = Box::new(move || {
        let _ = tx.send(work(&rl));
    });
    if !queue_relay_job(st, job) {
        return None;
    }
    rx.recv_timeout(limit).ok()
}

/// Run `work` on a worker thread, then `then(result)` on the UI thread.
fn spawn_then<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static, then: impl FnOnce(T) + Send + 'static) {
    let spawned = std::thread::Builder::new().name("sessionmux-job".into()).spawn(move || {
        let r = work();
        post(move || then(r));
    });
    if let Err(e) = spawned {
        log::error(&format!("could not start a worker thread: {e}"));
    }
}

// ------------------------------------------------------------------ IPC plumbing

/// Forwards panel requests to the UI thread's app state.
struct AppIpc;

impl IpcHandler for AppIpc {
    fn handle(&self, req: IpcRequest) {
        post(move || handle_ipc(req));
    }
}

/// Errors whose message is meant for the user as-is.
type UResult<T> = Result<T, String>;

fn human_error(msg: &str) -> String {
    if msg.chars().count() > 300 {
        let mut s: String = msg.chars().take(297).collect();
        s.push_str("...");
        s
    } else {
        msg.to_string()
    }
}

fn fail(msg: &str) -> Value {
    json!({ "ok": false, "error": human_error(msg) })
}

fn api_fail(e: &ApiError) -> Value {
    json!({ "ok": false, "error": e.error, "reason": e.reason })
}

fn ok() -> Value {
    json!({ "ok": true })
}

/// Bumped each time a panel (a fresh page with its own request-id counter) is created.
static PANEL_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const ID_BITS: u32 = 40;
const ID_MASK: u64 = (1 << ID_BITS) - 1;

/// Tag a page request id with the current panel generation, so a reply that a worker delivers
/// after the panel was destroyed and recreated cannot resolve the new page's request `id`.
fn tag_id(id: u64) -> u64 {
    if id == 0 {
        return 0;
    }
    let gen = PANEL_GEN.load(Ordering::Relaxed) & ((1 << (64 - ID_BITS)) - 1);
    (gen << ID_BITS) | (id & ID_MASK)
}

/// Deliver the reply to (tagged) request `id` (0 = notification, no reply).
fn resolve(id: u64, v: Value) {
    if id == 0 {
        return;
    }
    if id != tag_id(id & ID_MASK) {
        log::debug("ipc: dropped a reply meant for a previous panel");
        return;
    }
    with_state(|st| {
        if let Some(p) = st.panel.as_ref() {
            p.resolve(id & ID_MASK, true, &v);
        }
    });
}

fn arg(req: &IpcRequest, i: usize) -> &Value {
    static NULL: Value = Value::Null;
    req.args.get(i).unwrap_or(&NULL)
}

fn handle_ipc(mut req: IpcRequest) {
    req.id = tag_id(req.id);
    let id = req.id;
    match req.name.as_str() {
        "__ready" => {
            with_state(resend_state);
            return;
        }
        "__error" => {
            let msg = arg(&req, 0).as_str().unwrap_or("").to_string();
            with_state(|st| {
                if st.page_errors.len() < 50 {
                    st.page_errors.push(msg);
                }
            });
            return;
        }
        "__blur" => {
            on_blur();
            return;
        }
        _ => {}
    }
    log::debug(&format!("ipc: {} (id {id})", req.name));
    if let Some(v) = dispatch(&req) {
        resolve(id, v);
    }
}

/// Run one `window.sessionmux` method. Some(v) = reply now; None = a worker resolves it later.
fn dispatch(req: &IpcRequest) -> Option<Value> {
    let id = req.id;
    let a0 = arg(req, 0).clone();
    let a1 = arg(req, 1).clone();
    let name = req.name.as_str();
    let r: UResult<Option<Value>> = match name {
        "listAccounts" => {
            return Some(
                with_state(|st| {
                    let list = fresh_accounts(st);
                    st.last_accounts = list.clone();
                    serde_json::to_value(list).unwrap_or(json!([]))
                })
                .unwrap_or(json!([])),
            );
        }
        "openAccount" => open_account(id, &a0, true, OpenFrom::Panel).map(|_| None),
        "openAll" => open_all(id).map(|_| None),
        "closeAccount" => close_account(id, &a0).map(|_| None),
        "addAccount" => state_call(|st| add_account(st, &a0)).map(Some),
        "renameAccount" => state_call(|st| rename_account(st, &a0, &a1)).map(Some),
        "removeAccount" => remove_account(id, &a0, &a1).map(|_| None),
        "reorderAccounts" => state_call(|st| reorder_accounts(st, &a0)).map(Some),
        "listDiscovered" => {
            return Some(
                with_state(|st| {
                    let list = fresh_discovered(st);
                    st.last_discovered = list.clone();
                    serde_json::to_value(list).unwrap_or(json!([]))
                })
                .unwrap_or(json!([])),
            );
        }
        "adoptProfile" => state_call(|st| adopt_profile(st, &a0, &a1)).map(Some),
        "ignoreProfile" => state_call(|st| ignore_profile(st, &a0)).map(Some),
        "listSessions" => list_sessions(id, &a0).map(|_| None),
        "transferSession" => transfer_session(id, &a0).map(|_| None),
        "deleteSession" => delete_session(id, &a0, &a1).map(|_| None),
        "openSessionFolder" => open_session_folder(id, &a0, &a1).map(|_| None),
        "getSettings" => {
            return Some(
                with_state(|st| {
                    st.settings = st.store.settings();
                    panel_settings(st)
                })
                .unwrap_or(json!({})),
            );
        }
        "setSettings" => {
            // setSettings always answers with the current settings, never {ok:false}.
            return Some(
                with_state(|st| match clean_settings_patch(&a0) {
                    Ok(patch) => {
                        update_settings(st, patch);
                        panel_settings(st)
                    }
                    Err(e) => {
                        log::info(&format!("setSettings: {e}"));
                        panel_settings(st)
                    }
                })
                .unwrap_or(json!({})),
            );
        }
        "pinPanel" => {
            pin_panel(a0.as_bool() == Some(true));
            Ok(Some(ok()))
        }
        "togglePanel" => {
            toggle_panel();
            Ok(Some(ok()))
        }
        "hidePanel" => {
            hide_panel(false);
            Ok(Some(ok()))
        }
        "quit" => {
            quit();
            Ok(Some(ok()))
        }
        "openExternal" => {
            open_external(&a0);
            Ok(Some(ok()))
        }
        "openLicense" => {
            open_license();
            Ok(Some(ok()))
        }
        "getAppInfo" => return Some(app_info()),
        "listApprovals" => return with_state(|st| list_approvals(st)).or_else(|| Some(json!([]))),
        "decideApproval" => decide_approval(id, &a0),
        "listWaiting" => return with_state(|st| list_waiting(st)).or_else(|| Some(json!([]))),
        "handOverSession" => hand_over_session(id, &a0),
        "getHooksStatus" => return with_state(|st| hooks_status(st)).or_else(|| Some(json!({}))),
        "previewHooks" => return Some(hooks_preview()),
        "installHooks" => install_hooks(id),
        "removeHooks" => remove_hooks(id),
        "getSignIn" => return Some(with_state(|st| sign_in_payload(st)).unwrap_or_else(|| json!({ "active": false }))),
        "restoreLinkHandler" => restore_link_handler(id),
        "removeChanges" => remove_changes(id),
        "openSystemSettings" => open_system_settings(&a0).map(Some),
        "cancelSignIn" => {
            return Some(
                with_state(|st| {
                    end_sign_in(st, "cancelled");
                    ok()
                })
                .unwrap_or_else(|| fail("Sessionmux is busy, try again.")),
            );
        }
        other => {
            log::warn(&format!("ipc: unknown method {other}"));
            Err("Not allowed.".into())
        }
    };
    match r {
        Ok(v) => v,
        Err(e) => {
            log::info(&format!("{name}: {e}"));
            Some(fail(&e))
        }
    }
}

/// `with_state` for methods that return a user-facing result.
fn state_call(f: impl FnOnce(&mut AppState) -> UResult<Value>) -> UResult<Value> {
    with_state(f).unwrap_or_else(|| Err("Sessionmux is busy, try again.".into()))
}

// ------------------------------------------------------------------ validation helpers

fn id_ok(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn color_ok(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

#[derive(Default, Debug, PartialEq)]
struct AccountPatch {
    name: Option<String>,
    color: Option<String>,
    /// None = unchanged; Some(None) = clear; Some(Some(s)) = set.
    label: Option<Option<String>>,
}

impl AccountPatch {
    fn is_empty(&self) -> bool {
        self.name.is_none() && self.color.is_none() && self.label.is_none()
    }
}

/// Validate an account patch `{name?, color?, label?}` (name required when `require_name`).
fn clean_account_patch(p: &Value, require_name: bool) -> UResult<AccountPatch> {
    let Some(o) = p.as_object() else { return Err("Invalid account details.".into()) };
    let mut out = AccountPatch::default();
    if o.contains_key("name") || require_name {
        let n = o.get("name").and_then(Value::as_str).map(str::trim).unwrap_or("");
        if n.is_empty() {
            return Err("Please enter a name.".into());
        }
        if js_len(n) > 32 {
            return Err("Names can be at most 32 characters.".into());
        }
        out.name = Some(n.to_string());
    }
    if let Some(c) = o.get("color") {
        match c.as_str() {
            Some(c) if color_ok(c) => out.color = Some(c.to_uppercase()),
            _ => return Err("Invalid colour.".into()),
        }
    }
    if let Some(l) = o.get("label") {
        out.label = Some(match l {
            Value::Null => None,
            Value::String(s) if js_len(s) <= 128 => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
            _ => return Err("Label must be at most 128 characters.".into()),
        });
    }
    Ok(out)
}

/// Validate a settings patch; unknown keys are dropped (hotkey is read-only).
fn clean_settings_patch(p: &Value) -> UResult<Value> {
    let Some(o) = p.as_object() else { return Err("Invalid settings.".into()) };
    let mut out = serde_json::Map::new();
    for k in [
        "startWithWindows",
        "shareMcpConfig",
        "showBubble",
        "signInRouting",
        "autoHandover",
        "approveFromPanel",
        "raiseWindowsWithAccessibility",
    ] {
        if let Some(v) = o.get(k) {
            if !v.is_boolean() {
                return Err(format!("Invalid value for {k}."));
            }
            out.insert(k.into(), v.clone());
        }
    }
    let choice = |k: &str, allowed: &[&str], err: &str, out: &mut serde_json::Map<String, Value>| -> UResult<()> {
        if let Some(v) = o.get(k) {
            match v.as_str() {
                Some(s) if allowed.contains(&s) => {
                    out.insert(k.into(), v.clone());
                }
                _ => return Err(err.into()),
            }
        }
        Ok(())
    };
    choice("theme", &["system", "light", "dark"], "Invalid theme.", &mut out)?;
    choice("panelSide", &["auto", "left", "right"], "Invalid panel side.", &mut out)?;
    choice("bubbleMode", &["icon", "tray"], "Invalid floating button mode.", &mut out)?;
    if let Some(bp) = o.get("bubblePosition") {
        if bp.is_null() {
            out.insert("bubblePosition".into(), Value::Null);
        } else {
            let x = bp.get("x").and_then(Value::as_f64).filter(|v| v.is_finite());
            let y = bp.get("y").and_then(Value::as_f64).filter(|v| v.is_finite());
            match (x, y) {
                (Some(x), Some(y)) => {
                    out.insert("bubblePosition".into(), json!({ "x": x.round() as i64, "y": y.round() as i64 }));
                }
                _ => return Err("Invalid bubble position.".into()),
            }
        }
    }
    Ok(Value::Object(out))
}

// ------------------------------------------------------------------ accounts

fn main_data_dir(st: &AppState) -> Option<String> {
    st.located.as_ref().map(|l| l.main_data_dir.clone())
}

fn fresh_accounts(st: &AppState) -> Vec<Account> {
    let mut list = st.store.list_accounts(&st.procs, main_data_dir(st).as_deref());
    for a in &mut list {
        a.limited = st.limited.get(&a.id).cloned();
    }
    list
}

fn main_account_uuid(st: &AppState) -> Option<String> {
    let from = |list: &[Account]| list.iter().find(|a| a.is_main).and_then(|a| a.account_uuid.clone());
    if st.last_accounts.is_empty() {
        from(&fresh_accounts(st))
    } else {
        from(&st.last_accounts)
    }
}

fn fresh_discovered(st: &mut AppState) -> Vec<Discovered> {
    let uuid = main_account_uuid(st);
    st.store.list_discovered(uuid.as_deref())
}

/// The account with id `id` in the current list, or a user-facing error.
fn find_account(st: &AppState, id: &Value) -> UResult<Account> {
    let Some(id) = id.as_str().filter(|s| id_ok(s)) else { return Err("Unknown account.".into()) };
    if let Some(a) = st.last_accounts.iter().find(|a| a.id == id) {
        return Ok(a.clone());
    }
    fresh_accounts(st).into_iter().find(|a| a.id == id).ok_or_else(|| "That account no longer exists.".into())
}

/// The `--user-data-dir` an account's processes run with (None = Main).
fn proc_dir(a: &Account) -> Option<String> {
    if a.is_main || a.data_dir.is_none() {
        None
    } else {
        a.data_dir.clone()
    }
}

fn mcp_candidates(st: &AppState) -> Vec<String> {
    st.located.as_ref().map(|l| l.main_config_candidates.clone()).unwrap_or_default()
}

fn sync_mcp(st: &AppState, id: &str) {
    if is_smoke() {
        return; // smoke never writes into real Claude profile folders
    }
    if let Err(e) = st.store.sync_mcp_config(id, &mcp_candidates(st)) {
        log::warn(&format!("MCP sync for {id} failed: {e}"));
    }
}

fn sync_all_mcp(st: &AppState) {
    for a in fresh_accounts(st).iter().filter(|a| !a.is_main) {
        sync_mcp(st, &a.id);
    }
}

/// What a worker needs to open one account.
struct OpenPlan {
    id: String,
    name: String,
    is_main: bool,
    dir: Option<String>,
    /// Begin a sign-in relay once the window is up.
    route: bool,
}

/// UI-side preparation for opening `acc` (validation; mkdir + MCP sync when it will launch).
fn prepare_open(st: &AppState, acc: &Account) -> UResult<OpenPlan> {
    let dir = proc_dir(acc);
    if !acc.is_main {
        let valid = dir.as_deref().is_some_and(|d| Path::new(d).is_absolute());
        if !valid {
            return Err("This account has no valid data folder.".into());
        }
        if !acc.running && !is_smoke() {
            if let Some(d) = dir.as_deref() {
                if let Err(e) = std::fs::create_dir_all(d) {
                    return Err(format!("Could not create the profile folder: {e}"));
                }
            }
            sync_mcp(st, &acc.id);
        }
    }
    let route = wants_sign_in_route(st, acc, dir.as_deref());
    Ok(OpenPlan { id: acc.id.clone(), name: acc.name.clone(), is_main: acc.is_main, dir, route })
}

/// Worker: focus the running window, or launch the account. `note` receives what happened, for
/// the log.
fn open_worker(
    plan: &OpenPlan,
    located: Option<Located>,
    focus: bool,
    wake: &Option<mpsc::Sender<()>>,
    note: &mut String,
) -> Value {
    let running: Vec<Proc> = claude_app::list_processes()
        .into_iter()
        .filter(|p| claude_app::matches_data_dir(p, plan.dir.as_deref()))
        .collect();
    if !running.is_empty() {
        if !focus {
            *note = format!("already running (pid {})", running[0].pid);
            return ok();
        }
        // On macOS a running instance is never started again, so activation (and the opt-in
        // Accessibility raise) is tried even when none of its windows is on screen.
        let shown = running.iter().find(|p| p.hwnd != 0);
        let target = shown.or_else(|| running.first().filter(|_| cfg!(target_os = "macos")));
        if let Some(w) = target {
            if is_smoke() {
                log::info("smoke: blocked claude_app::focus");
            } else {
                match <Os as Focus>::focus(w.pid) {
                    FocusResult::Focused => {
                        *note = format!("focused pid={} window={}", w.pid, w.hwnd != 0);
                        if plan.route {
                            schedule_sign_in(plan, None);
                        }
                        return ok();
                    }
                    FocusResult::NeedsHandOff => {
                        *note = format!("hand-off pid={} window={}", w.pid, w.hwnd != 0);
                    }
                    // No hand-off can show it either: the page explains what the user can do.
                    FocusResult::Failed(why) => {
                        *note = format!("failed: {} pid={} window={}", why.reason(), w.pid, w.hwnd != 0);
                        let mut v = fail(&format!("Could not bring {}'s Claude window to the front.", plan.name));
                        v["reason"] = json!(why.reason());
                        return v;
                    }
                }
            }
        } else {
            *note = format!("running without a window (pid {})", running[0].pid);
        }
        // Running but hidden in the tray (hwnd 0) or focus refused: launching again hands off
        // to the running instance through Claude's single-instance lock, which shows it.
    }
    if is_smoke() {
        log::info("smoke: blocked claude_app::launch");
        *note = "launch blocked (smoke)".into();
        return fail(&format!("Could not start Claude for {}.", plan.name));
    }
    let Some(loc) = located.or_else(|| claude_app::locate().ok()) else {
        *note = "failed: Claude Desktop not found".into();
        return fail("Claude Desktop was not found on this PC.");
    };
    let r = if plan.is_main {
        <Os as Launcher>::launch_main(&loc).map(|_| ())
    } else {
        match plan.dir.as_deref() {
            Some(d) => {
                let _ = std::fs::create_dir_all(d);
                <Os as Launcher>::launch(&loc, d).map(|_| ())
            }
            None => Err(String::new()),
        }
    };
    if let Err(e) = r {
        log::warn(&format!("open {}: {e}", plan.id));
        *note = "failed: launch error".into();
        let msg = if e.trim().is_empty() { format!("Could not start Claude for {}.", plan.name) } else { e };
        return fail(&msg);
    }
    if focus {
        let dir = plan.dir.clone();
        let id = plan.id.clone();
        let route = plan.route.then(|| plan.id.clone());
        let _ = std::thread::Builder::new().name("sessionmux-focus".into()).spawn(move || {
            if claude_app::wait_and_focus(dir.as_deref(), FOCUS_WAIT_MS).is_none() {
                log::warn(&format!("waitAndFocus {id}: no window within {} s", FOCUS_WAIT_MS / 1000));
            } else if let Some(acc_id) = route {
                std::thread::sleep(SIGNIN_BEGIN_DELAY);
                post(move || begin_sign_in(&acc_id));
            }
        });
    } else if plan.route {
        schedule_sign_in(plan, Some(plan.dir.clone()));
    }
    *note = if note.is_empty() { "launched".into() } else { format!("launched after {note}") };
    if running.is_empty() {
        poll_later(wake.clone(), Duration::from_millis(1500)); // pick up the new process quickly
    }
    ok()
}

fn poll_later(wake: Option<mpsc::Sender<()>>, after: Duration) {
    if let Some(tx) = wake {
        let _ = std::thread::Builder::new().name("sessionmux-poll-later".into()).spawn(move || {
            std::thread::sleep(after);
            let _ = tx.send(());
        });
    }
}

/// Wake the poll thread now (smoke: refresh from the cached process list).
fn request_poll(st: &mut AppState) {
    match st.poll_wake.as_ref() {
        Some(tx) => {
            let _ = tx.send(());
        }
        None => refresh(st),
    }
}

/// Where an open was asked for (named in the log).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenFrom {
    Panel,
    Bubble,
    Tray,
    /// Opened right after the account was added.
    Added,
    /// The target of a session hand-over.
    Handover,
    /// Asked for by the focus check without any input event.
    #[cfg(target_os = "macos")]
    Check,
}

impl OpenFrom {
    fn as_str(self) -> &'static str {
        match self {
            OpenFrom::Panel => "panel",
            OpenFrom::Bubble => "bubble",
            OpenFrom::Tray => "tray",
            OpenFrom::Added => "add",
            OpenFrom::Handover => "handover",
            #[cfg(target_os = "macos")]
            OpenFrom::Check => "check",
        }
    }
}

fn open_account(id: u64, acc_id: &Value, focus: bool, from: OpenFrom) -> UResult<()> {
    let (plan, located, wake, running) = state_call_raw(|st| {
        let acc = find_account(st, acc_id)?;
        st.last_opened = Some(acc.id.clone());
        set_active(st, Some(&acc.id));
        let plan = prepare_open(st, &acc)?;
        let running = st.procs.iter().find(|p| claude_app::matches_data_dir(p, plan.dir.as_deref())).map(|p| p.pid);
        Ok((plan, st.located.clone(), st.poll_wake.clone(), running))
    })?;
    // Runs from the posted handler of the click, menu choice or shortcut (not from within the
    // input event itself): the platform takes what activation it can here, before the worker.
    if focus && !is_smoke() {
        match running {
            Some(pid) => <Os as Focus>::prepare_focus(pid),
            None => log::info(&format!("open {}: no running process known yet, activation not prepared", plan.id)),
        }
    }
    let pin = SURFACE_OPEN_FAILURES && id != 0;
    if pin {
        pin_panel(true);
    }
    spawn_then(
        move || {
            let mut note = String::new();
            let v = open_worker(&plan, located, focus, &wake, &mut note);
            (plan, v, note)
        },
        move |(plan, v, note): (OpenPlan, Value, String)| {
            let failed = v.get("ok") == Some(&Value::Bool(false));
            let line = open_log_line(&plan.id, from, &note);
            if failed {
                log::warn(&format!("{line}; {}", v["error"].as_str().unwrap_or("")));
            } else {
                log::info(&line);
            }
            if failed && SURFACE_OPEN_FAILURES {
                show_open_failure(id, &plan, &v);
            } else if pin {
                pin_panel(false);
            }
            #[cfg(target_os = "macos")]
            let seen = crate::focuscheck::active().then(|| (plan.id.clone(), from.as_str(), note, v.clone()));
            resolve(id, v);
            #[cfg(target_os = "macos")]
            if let Some((acc, surface, note, reply)) = seen {
                let visible = with_state(|st| st.panel.as_ref().is_some_and(|p| p.is_visible())).unwrap_or(false);
                crate::focuscheck::note_open(&acc, surface, &note, reply, visible);
            }
        },
    );
    Ok(())
}

/// The log line that sums up one open: `open <account> via <surface>: <what happened>`.
fn open_log_line(acc: &str, from: OpenFrom, note: &str) -> String {
    format!("open {acc} via {}: {}", from.as_str(), if note.is_empty() { "ok" } else { note })
}

/// Make a failed open visible: the panel comes up (it may have hidden when Claude took the
/// activation) and stays up for the pin time even while Claude is in front, and an open that has
/// no reply to carry the reason is reported as `openFailed`.
fn show_open_failure(id: u64, plan: &OpenPlan, v: &Value) {
    pin_panel(true);
    let visible = with_state(|st| st.panel.as_ref().is_some_and(|p| p.is_visible())).unwrap_or(false);
    if !visible {
        if id == 0 {
            show_tab("accounts");
        } else {
            show_panel();
        }
    }
    // The page that asked may have been destroyed while the open ran (the panel hid during a
    // long Automation prompt); the page shown now cannot take its reply, so it gets the event.
    let stale = id != 0 && id != tag_id(id & ID_MASK);
    if stale {
        with_state(|st| emit(st, "show-tab", &json!("accounts")));
    }
    if id == 0 || stale {
        let payload = open_failed_payload(plan, v);
        with_state(|st| emit(st, "openFailed", &payload));
    }
}

/// The `openFailed` event: which account, and the reason and message a reply would have carried.
fn open_failed_payload(plan: &OpenPlan, v: &Value) -> Value {
    json!({
        "id": plan.id,
        "name": plan.name,
        "reason": v.get("reason").cloned().unwrap_or(Value::Null),
        "error": v.get("error").cloned().unwrap_or(Value::Null),
    })
}

fn state_call_raw<T>(f: impl FnOnce(&mut AppState) -> UResult<T>) -> UResult<T> {
    with_state(f).unwrap_or_else(|| Err("Sessionmux is busy, try again.".into()))
}

fn open_all(id: u64) -> UResult<()> {
    let (plans, located, wake) = state_call_raw(|st| {
        let mut plans = Vec::new();
        for a in fresh_accounts(st).iter().filter(|a| !a.running) {
            match prepare_open(st, a) {
                Ok(p) => {
                    st.last_opened = Some(a.id.clone());
                    plans.push(p);
                }
                Err(e) => log::warn(&format!("openAll: {} failed: {e}", a.id)),
            }
        }
        if let Some(last) = st.last_opened.clone() {
            if plans.iter().any(|p| p.id == last) {
                set_active(st, Some(&last));
            }
        }
        Ok((plans, st.located.clone(), st.poll_wake.clone()))
    })?;
    spawn_then(
        move || {
            let mut failures = Vec::new();
            for p in plans {
                let mut note = String::new();
                let r = open_worker(&p, located.clone(), false, &wake, &mut note);
                if r.get("ok") == Some(&Value::Bool(false)) {
                    log::warn(&format!("openAll: {} failed: {} ({note})", p.id, r["error"]));
                    failures.push((p, r));
                } else {
                    log::info(&format!("openAll: {}: {}", p.id, if note.is_empty() { "ok" } else { &note }));
                }
                std::thread::sleep(OPEN_ALL_STAGGER);
            }
            failures
        },
        move |failures: Vec<(OpenPlan, Value)>| {
            if SURFACE_OPEN_FAILURES {
                // The reply carries no result per account, so each failure is its own event.
                for (p, r) in &failures {
                    show_open_failure(0, p, r);
                }
            }
            resolve(id, ok());
        },
    );
    Ok(())
}

fn close_account(id: u64, acc_id: &Value) -> UResult<()> {
    let (acc, wake) = state_call_raw(|st| Ok((find_account(st, acc_id)?, st.poll_wake.clone())))?;
    spawn_then(
        move || {
            let dir = proc_dir(&acc);
            let pids: Vec<u32> = claude_app::list_processes()
                .into_iter()
                .filter(|p| claude_app::matches_data_dir(p, dir.as_deref()))
                .map(|p| p.pid)
                .collect();
            if pids.is_empty() {
                return json!({ "ok": true, "notRunning": true });
            }
            let r = if is_smoke() {
                log::info("smoke: blocked claude_app::kill");
                Err("smoke".to_string())
            } else {
                <Os as Closer>::close(&pids)
            };
            poll_later(wake, Duration::from_millis(1000));
            match r {
                Ok(()) => ok(),
                Err(e) => {
                    log::warn(&format!("closeAccount {}: {e}", acc.id));
                    fail(&format!("Could not close {}'s Claude window.", acc.name))
                }
            }
        },
        move |v| resolve(id, v),
    );
    Ok(())
}

fn add_account(st: &mut AppState, opts: &Value) -> UResult<Value> {
    let p = clean_account_patch(opts, true)?;
    let name = p.name.clone().unwrap_or_default();
    let label = p.label.clone().flatten();
    let rec = match st.store.add_account(&name, p.color.as_deref(), label.as_deref()) {
        Ok(r) => r,
        Err(e) => return Ok(api_fail(&e)),
    };
    sync_mcp(st, &rec.id);
    refresh(st);
    let account = fresh_accounts(st).into_iter().find(|a| a.id == rec.id);
    // Open it right away (the user signs in there); failures are only logged.
    let new_id = Value::String(rec.id.clone());
    post(move || {
        if let Err(e) = open_account(0, &new_id, true, OpenFrom::Added) {
            log::warn(&format!("open after add failed: {e}"));
        }
    });
    Ok(json!({ "ok": true, "account": account }))
}

fn rename_account(st: &mut AppState, id: &Value, patch: &Value) -> UResult<Value> {
    let acc = find_account(st, id)?;
    let p = clean_account_patch(patch, false)?;
    if p.is_empty() {
        return Ok(ok());
    }
    let label: Option<String> = p.label.map(|l| l.unwrap_or_default());
    let res = st.store.rename_account(&acc.id, p.name.as_deref(), p.color.as_deref(), label.as_deref());
    refresh(st);
    Ok(match res {
        Ok(()) => ok(),
        Err(e) => api_fail(&e),
    })
}

fn remove_account(id: u64, acc_id: &Value, opts: &Value) -> UResult<()> {
    let delete_data = opts.get("deleteData") == Some(&Value::Bool(true));
    let acc = state_call_raw(|st| {
        let acc = find_account(st, acc_id)?;
        if acc.is_main || acc.id == "main" || acc.data_dir.is_none() {
            return Err("The Main account cannot be removed.".into());
        }
        Ok(acc)
    })?;
    let acc_id = acc.id.clone();
    spawn_then(
        move || {
            // Close the account's Claude first; the store refuses to delete a folder in use.
            let dir = proc_dir(&acc);
            let pids: Vec<u32> = claude_app::list_processes()
                .into_iter()
                .filter(|p| claude_app::matches_data_dir(p, dir.as_deref()))
                .map(|p| p.pid)
                .collect();
            if !pids.is_empty() {
                if is_smoke() {
                    log::info("smoke: blocked claude_app::kill");
                } else if let Err(e) = <Os as Closer>::close(&pids) {
                    log::warn(&format!("removeAccount {}: {e}", acc.id));
                }
            }
            match (delete_data, dir.as_deref()) {
                (true, Some(d)) => <Os as Processes>::processes_under(d),
                _ => Vec::new(),
            }
        },
        move |busy| {
            let v = with_state(|st| {
                let res = st.store.remove_account_checked(&acc_id, delete_data, &|_| busy.clone());
                if res.is_ok() {
                    if st.last_opened.as_deref() == Some(acc_id.as_str()) {
                        st.last_opened = None;
                    }
                    if st.last_active.as_deref() == Some(acc_id.as_str()) {
                        st.last_active = None;
                    }
                }
                refresh(st);
                match res {
                    Ok(()) => ok(),
                    Err(e) => api_fail(&e),
                }
            })
            .unwrap_or_else(|| fail("Sessionmux is busy, try again."));
            resolve(id, v);
        },
    );
    Ok(())
}

fn reorder_accounts(st: &mut AppState, ids: &Value) -> UResult<Value> {
    let list = ids.as_array().filter(|a| a.len() <= 500);
    let Some(list) = list else { return Err("Invalid account order.".into()) };
    let mut out = Vec::with_capacity(list.len());
    for v in list {
        match v.as_str() {
            Some(s) if id_ok(s) => out.push(s.to_string()),
            _ => return Err("Invalid account order.".into()),
        }
    }
    st.store.reorder(&out);
    refresh(st);
    Ok(ok())
}

/// The found profile folder `data_dir` (refreshed list), or a user-facing error.
fn find_discovered(st: &mut AppState, data_dir: &Value) -> UResult<Discovered> {
    let d = data_dir.as_str().filter(|s| !s.trim().is_empty() && js_len(s) <= 1024);
    let Some(d) = d else { return Err("Unknown profile folder.".into()) };
    fresh_discovered(st)
        .into_iter()
        .find(|x| paths::same_path(&x.data_dir, d))
        .ok_or_else(|| "That profile folder is no longer available.".into())
}

fn adopt_profile(st: &mut AppState, data_dir: &Value, opts: &Value) -> UResult<Value> {
    let found = find_discovered(st, data_dir)?;
    let p = if opts.is_null() { AccountPatch::default() } else { clean_account_patch(opts, false)? };
    let uuid = main_account_uuid(st);
    let label = p.label.clone().flatten();
    let res = st.store.adopt_profile(
        &found.data_dir,
        p.name.as_deref(),
        p.color.as_deref(),
        label.as_deref(),
        uuid.as_deref(),
    );
    let out = match res {
        Ok(rec) => {
            sync_mcp(st, &rec.id);
            refresh(st);
            let account = fresh_accounts(st).into_iter().find(|a| a.id == rec.id);
            json!({ "ok": true, "account": account })
        }
        Err(e) => {
            refresh(st);
            api_fail(&e)
        }
    };
    Ok(out)
}

fn ignore_profile(st: &mut AppState, data_dir: &Value) -> UResult<Value> {
    let found = find_discovered(st, data_dir)?;
    st.store.ignore_profile(&found.data_dir);
    refresh(st);
    Ok(ok())
}

// ------------------------------------------------------------------ sign-in routing

/// Opening `acc` (whose processes use `dir`) should route sign-in links to it: an extra account
/// that is not signed in, routing on, not smoke.
fn wants_sign_in_route(st: &AppState, acc: &Account, dir: Option<&str>) -> bool {
    if is_smoke() || !st.settings.sign_in_routing || acc.is_main || acc.signed_in {
        return false;
    }
    match dir {
        Some(d) if !d.trim().is_empty() => !main_data_dir(st).is_some_and(|m| paths::same_path(&m, d)),
        _ => false,
    }
}

/// Worker side: post `begin_sign_in` `SIGNIN_BEGIN_DELAY` after the account's window is up.
/// `wait_for`: Some(dir) = first wait (without focusing) for a window of that profile;
/// None = the window is already up.
fn schedule_sign_in(plan: &OpenPlan, wait_for: Option<Option<String>>) {
    let acc_id = plan.id.clone();
    let _ = std::thread::Builder::new().name("sessionmux-signin".into()).spawn(move || {
        if let Some(dir) = wait_for {
            let deadline = Instant::now() + Duration::from_millis(FOCUS_WAIT_MS);
            loop {
                if claude_app::list_processes()
                    .iter()
                    .any(|p| claude_app::matches_data_dir(p, dir.as_deref()) && p.hwnd != 0)
                {
                    break;
                }
                if Instant::now() >= deadline {
                    log::warn(&format!("sign-in routing {acc_id}: no window within {} s", FOCUS_WAIT_MS / 1000));
                    return;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
        std::thread::sleep(SIGNIN_BEGIN_DELAY);
        post(move || begin_sign_in(&acc_id));
    });
}

/// `oauth:tokenCache` in the profile's `config.json` (the accounts list's `signedIn` rule).
fn profile_signed_in(data_dir: &str) -> bool {
    let cfg: Option<Value> = ujson::read(&Path::new(data_dir).join("config.json"));
    match cfg.as_ref().and_then(|c| c.get("oauth:tokenCache")) {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// `getSignIn` shape: `{active, accountId, accountName, since, handlerPending, handlerDeclined}` or
/// `{active:false}`, both with `restoreDeclined` (the user kept Sessionmux as the handler after a
/// sign-in; "Give sign-in links back to Claude" undoes it) and `needsConsent` (each handler change
/// asks the user, so routing is opt-in). While active, `forwardFailed` (the last link went to Main
/// instead) and `automationDenied` (because the system did not allow sending it) appear when true.
fn sign_in_payload(st: &AppState) -> Value {
    let state = st.relay.state();
    let mut v = match state.as_ref().filter(|s| s.active) {
        Some(s) => {
            let mut v = json!({
                "active": true,
                "accountId": s.account_id,
                "accountName": s.account_name,
                "since": s.since,
                "handlerPending": s.handler_pending,
                "handlerDeclined": s.handler_declined,
            });
            if let Some(o) = v.as_object_mut() {
                if st.relay_forward_failed {
                    o.insert("forwardFailed".into(), json!(true));
                }
                if st.relay_automation_denied {
                    o.insert("automationDenied".into(), json!(true));
                }
            }
            v
        }
        None => json!({ "active": false }),
    };
    if let Some(o) = v.as_object_mut() {
        o.insert("restoreDeclined".into(), json!(state.is_some_and(|s| !s.active && s.restore_declined)));
        o.insert("needsConsent".into(), json!(<Os as UrlHandler>::changes_need_consent()));
    }
    v
}

/// Emit `signin-changed`; `ended` (signedIn | timeout | closed | cancelled | disabled | quit) and
/// the account name are added when a relay just ended, so the page can confirm it with a toast.
fn push_sign_in(st: &AppState, ended: Option<(&str, String)>) {
    let mut v = sign_in_payload(st);
    if let (Some((reason, name)), Some(o)) = (ended, v.as_object_mut()) {
        o.insert("ended".into(), json!(reason));
        o.insert("accountName".into(), json!(name));
    }
    emit(st, "signin-changed", &v);
}

/// UI thread: start routing sign-in links to `acc_id` (replacing another waiting account).
fn begin_sign_in(acc_id: &str) {
    with_state(|st| {
        if is_smoke() || !st.settings.sign_in_routing {
            return;
        }
        let Some(acc) = fresh_accounts(st).into_iter().find(|a| a.id == acc_id) else { return };
        let dir = proc_dir(&acc);
        if !wants_sign_in_route(st, &acc, dir.as_deref()) {
            return;
        }
        let Some(dir) = dir else { return };
        if profile_signed_in(&dir) {
            return;
        }
        let (id, name, running) = (acc.id.clone(), acc.name.clone(), acc.running);
        let work = move |rl: &Relay| {
            if let Some(cur) = rl.state().filter(|s| s.active && s.account_id != id) {
                log::info(&format!("relay: {name} replaces {} as the account signing in", cur.account_name));
                if let Err(e) = rl.end() {
                    log::warn(&format!("relay end (replace): {e}"));
                }
            }
            rl.begin(&id, &name, &dir).map_err(|e| format!("relay begin {id}: {e}"))
        };
        relay_job(st, work, move |st, r| match r {
            Ok(_) => {
                st.relay_seen_running = running;
                st.relay_started = Some(Instant::now());
                st.relay_last_error = None;
                st.relay_forward_failed = false;
                st.relay_automation_denied = false;
                if !st.settings.sign_in_routing {
                    // Turned off while the handler was being taken.
                    end_sign_in(st, "disabled");
                    return;
                }
                set_timer(TimerId::SignIn, relay::POLL_MS as u32);
                push_sign_in(st, None);
            }
            Err(e) => log::warn(&e),
        });
    });
}

/// End the active relay (the relay worker restores the handler and deletes relay.json) and tell
/// the page why once it is done.
fn end_sign_in(st: &mut AppState, reason: &str) {
    kill_timer(TimerId::SignIn);
    st.relay_seen_running = false;
    st.relay_started = None;
    st.relay_last_error = None;
    st.relay_forward_failed = false;
    st.relay_automation_denied = false;
    if st.relay.state().is_none() {
        push_sign_in(st, None);
        return;
    }
    let reason = reason.to_string();
    let work = |rl: &Relay| {
        let s = rl.state();
        let r = s.as_ref().map(|_| rl.end());
        (s, r)
    };
    relay_job(st, work, move |st, (s, r)| {
        let Some(s) = s else {
            push_sign_in(st, None);
            return;
        };
        if let Some(Err(e)) = r {
            log::warn(&format!("relay end ({reason}): {e}"));
        }
        watch_restore(st);
        log::info(&format!("relay: sign-in for {} ended ({reason})", s.account_name));
        push_sign_in(st, s.active.then_some((reason.as_str(), s.account_name)));
    });
}

/// Quitting ends the sign-in routing without waiting for the user. Where quitting gives the
/// handler back, the relay worker does it now (waited for, after any change already queued);
/// elsewhere, and wherever each change asks the user, the restore is left to the next start and
/// the handler is not touched.
fn end_relay_at_exit(st: &mut AppState, why: &str) {
    if st.relay.state().is_none() {
        return;
    }
    let r = if Relay::<Os>::exit_defers_restore() {
        st.relay.defer_restore()
    } else {
        relay_job_wait(st, |rl| rl.end(), EXIT_RESTORE_WAIT)
            .unwrap_or_else(|| Err("the relay worker did not finish in time".into()))
    };
    if let Err(e) = r {
        log::warn(&format!("relay end ({why}): {e}"));
    }
}

/// `quit`: end the sign-in routing as [`end_relay_at_exit`] does and tell the page.
fn end_sign_in_at_quit(st: &mut AppState) {
    kill_timer(TimerId::SignIn);
    st.relay_seen_running = false;
    st.relay_started = None;
    let Some(s) = st.relay.state() else { return };
    end_relay_at_exit(st, "quit");
    if s.active {
        log::info(&format!("relay: sign-in for {} ended (quit)", s.account_name));
    }
    push_sign_in(st, s.active.then_some(("quit", s.account_name)));
}

/// Keep the sign-in timer running while giving the links back still waits for the user's answer.
fn watch_restore(st: &AppState) {
    if st.relay.state().is_some_and(|s| !s.active && s.restore_pending) {
        set_timer(TimerId::SignIn, relay::POLL_MS as u32);
    }
}

/// `TimerId::SignIn`: keep the handler ours; end on sign-in, exit or timeout. After the relay, a
/// restore that waits for the user's answer is checked until it settles. While a relay job is
/// still queued or running (a handler change may wait for the user), the tick does nothing.
fn sign_in_tick(st: &mut AppState) {
    if st.relay_busy > 0 {
        return;
    }
    let state = st.relay.state();
    if state.as_ref().is_some_and(|s| !s.active && s.restore_pending) {
        relay_job(
            st,
            |rl| rl.restore_settled(),
            |st, settled| {
                if settled {
                    kill_timer(TimerId::SignIn);
                    push_sign_in(st, None);
                }
            },
        );
        return;
    }
    let Some(rs) = state.filter(|s| s.active) else {
        end_sign_in(st, "gone");
        return;
    };
    if profile_signed_in(&rs.data_dir) {
        end_sign_in(st, "signedIn");
        refresh(st); // signedIn flips in the accounts list and the bubble
        return;
    }
    let running = st.procs.iter().any(|p| claude_app::matches_data_dir(p, Some(&rs.data_dir)));
    if running {
        st.relay_seen_running = true;
    } else if st.relay_seen_running || st.relay_started.is_none_or(|t| t.elapsed() >= SIGNIN_NEVER_RUNNING) {
        end_sign_in(st, "closed");
        return;
    }
    if st.relay.expired(relay::now_secs()) {
        end_sign_in(st, "timeout");
        return;
    }
    let before = (rs.handler_pending, rs.handler_declined);
    relay_job(
        st,
        |rl| rl.ensure(),
        move |st, r| {
            match r {
                Ok(_) => st.relay_last_error = None,
                Err(e) => {
                    if st.relay_last_error.as_deref() != Some(e.as_str()) {
                        log::warn(&format!("relay ensure: {e}"));
                    }
                    st.relay_last_error = Some(e);
                }
            }
            // The answer about the handler arrived (or Claude took it back): the notice changes.
            let now = st.relay.state().filter(|s| s.active).map(|s| (s.handler_pending, s.handler_declined));
            if now.is_some_and(|n| n != before) {
                push_sign_in(st, None);
            }
        },
    );
}

/// Startup: resume a relay whose account still runs unsigned-in, else end it (which also heals
/// a handler left pointing at us without a relay.json).
fn init_sign_in(st: &mut AppState) {
    let state = st.relay.state();
    let resume = state.as_ref().filter(|s| s.active).is_some_and(|s| {
        let acc = st.last_accounts.iter().find(|a| a.id == s.account_id);
        st.settings.sign_in_routing && acc.is_some_and(|a| a.running && !a.signed_in) && !profile_signed_in(&s.data_dir)
    });
    if resume {
        st.relay_seen_running = true;
        st.relay_started = Some(Instant::now());
        set_timer(TimerId::SignIn, relay::POLL_MS as u32);
        log::info("relay: resumed the sign-in routing of the previous run");
    } else {
        relay_job(
            st,
            |rl| rl.end(),
            |st, r| {
                if let Err(e) = r {
                    log::warn(&format!("relay cleanup: {e}"));
                }
                watch_restore(st);
            },
        );
    }
}

/// Relay worker: give `claude://` back to Claude because the user asked, also after an earlier
/// refusal. The outcome: "restored", "pending" (the system still waits for the user's answer;
/// checked on the sign-in timer) or "declined".
fn give_links_back(rl: &Relay) -> Result<&'static str, String> {
    rl.restore_now()?;
    Ok(match rl.state() {
        Some(s) if s.restore_pending => "pending",
        Some(s) if s.restore_declined => "declined",
        _ => "restored",
    })
}

/// `restoreLinkHandler` -> `{ ok, linkHandler }`, resolved once the relay worker is done. Not
/// while an account is signing in.
fn restore_link_handler(id: u64) -> UResult<Option<Value>> {
    state_call_raw(|st| {
        if st.relay.state().is_some_and(|s| s.active) {
            return Err("An account is signing in. Cancel its sign-in first.".into());
        }
        relay_job(st, give_links_back, move |st, r| {
            watch_restore(st);
            push_sign_in(st, None);
            let v = match r {
                Ok(link) => json!({ "ok": true, "linkHandler": link }),
                Err(e) => {
                    log::warn(&format!("relay restore: {e}"));
                    fail("Sign-in links could not be given back to Claude.")
                }
            };
            post(move || resolve(id, v));
        });
        Ok(None)
    })
}

/// How the last link for the account signing in was delivered: None = to that account,
/// Some(denied) = to Main instead (`denied`: the system did not allow sending it). The sign-in
/// notice tells the user what to do about it.
fn note_forward(st: &mut AppState, issue: Option<bool>) {
    if !st.relay.state().is_some_and(|s| s.active) {
        return;
    }
    let flags = match issue {
        None => (false, false),
        Some(denied) => (true, denied),
    };
    if (st.relay_forward_failed, st.relay_automation_denied) == flags {
        return;
    }
    (st.relay_forward_failed, st.relay_automation_denied) = flags;
    push_sign_in(st, None);
}

/// A `claude://` link the system handed to Sessionmux: forwarded on a worker to the account
/// signing in, else (no relay, or forwarding there failed) to Main. A link that arrives while no
/// relay is active also gives the handler back to Claude if it still points at Sessionmux.
fn on_open_url(raw: &str) {
    let url = match relay::validate_url(raw) {
        Ok(u) => u,
        Err(e) => {
            log::warn(&format!("relay: refused link {} ({e})", relay::redact(raw.trim())));
            return;
        }
    };
    let shown = relay::redact(&url);
    if is_smoke() {
        log::info(&format!("smoke: blocked forwarding {shown}"));
        return;
    }
    let job = with_state(|st| {
        let state = st.relay.state();
        let (target, _) = relay::relay_decision(state.as_ref(), relay::now_secs(), true);
        if state.as_ref().is_none_or(|s| !s.active && !s.restore_pending) {
            relay_job(
                st,
                |rl| rl.end(),
                |st, r| {
                    if let Err(e) = r {
                        log::warn(&format!("relay: giving the links back failed ({e})"));
                    }
                    watch_restore(st);
                },
            );
        }
        if st.located.is_none() {
            st.located = claude_app::locate().ok();
        }
        let pid_of = |dir: Option<&str>| st.procs.iter().find(|p| claude_app::matches_data_dir(p, dir)).map(|p| p.pid);
        let name = state.map(|s| s.account_name).unwrap_or_default();
        let target = target.map(|d| (pid_of(Some(&d)), d, name));
        (st.located.clone(), target, pid_of(None))
    });
    let Some((located, target, main_pid)) = job else { return };
    let Some(loc) = located else {
        log::error(&format!("relay: cannot forward {shown}: Claude not found"));
        return;
    };
    let spawned = std::thread::Builder::new().name("sessionmux-link".into()).spawn(move || {
        let mut issue = None;
        if let Some((pid, dir, name)) = &target {
            match relay::forward_to(&loc, Some(dir), *pid, &url) {
                Ok(()) => {
                    log::info(&format!("relay: forwarded {shown} to {name}"));
                    post(|| {
                        with_state(|st| note_forward(st, None));
                    });
                    return;
                }
                Err(e) => {
                    log::error(&format!("relay: forwarding {shown} to {name} failed ({e}); using default"));
                    issue = Some(e == ForwardError::NotPermitted);
                }
            }
        }
        match relay::forward_to(&loc, None, main_pid, &url) {
            Ok(()) => log::info(&format!("relay: forwarded {shown} to default")),
            Err(e) => log::error(&format!("relay: forwarding {shown} to default failed ({e})")),
        }
        if let Some(denied) = issue {
            post(move || {
                with_state(|st| note_forward(st, Some(denied)));
            });
        }
    });
    if let Err(e) = spawned {
        log::error(&format!("could not start a worker thread: {e}"));
    }
}

// ------------------------------------------------------------------ sessions

/// The data dir and Claude account uuid of `acc`, as the session code needs them.
fn session_account(st: &mut AppState, acc: &Account) -> UResult<SessionAccount> {
    let dir = if acc.is_main {
        if st.located.is_none() {
            st.located = claude_app::locate().ok();
        }
        main_data_dir(st)
    } else {
        acc.data_dir.clone()
    };
    let Some(dir) = dir.filter(|d| !d.is_empty()) else {
        return Err(format!("Could not find the data folder for {}.", acc.name));
    };
    let config: Option<Value> = ujson::read(&Path::new(&dir).join("config.json"));
    let from_config = config
        .as_ref()
        .and_then(|c| c.get("lastKnownAccountUuid"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Ok(SessionAccount {
        id: acc.id.clone(),
        name: acc.name.clone(),
        data_dir: dir,
        is_main: acc.is_main,
        account_uuid: acc.account_uuid.clone().or(from_config),
        running: acc.running,
    })
}

fn check_session_id(v: &Value) -> UResult<String> {
    match v.as_str() {
        Some(s) if valid_session_id(s) => Ok(s.to_string()),
        _ => Err("Unknown session.".into()),
    }
}

fn sessions_clone(st: &AppState) -> Sessions {
    Sessions { home: st.sessions.home.clone() }
}

fn list_sessions(id: u64, acc_id: &Value) -> UResult<()> {
    let (sa, sessions) = state_call_raw(|st| {
        let acc = find_account(st, acc_id)?;
        Ok((session_account(st, &acc)?, sessions_clone(st)))
    })?;
    spawn_then(move || serde_json::to_value(sessions.list(&sa)).unwrap_or(json!([])), move |v| resolve(id, v));
    Ok(())
}

fn reason_text(reason: Option<&str>, to_name: &str) -> String {
    match reason {
        Some("running") => "This session is running. Stop it in Claude first.".into(),
        Some("target-never-opened-code") => {
            format!("{to_name} has never opened the Code tab. Open it there once, then try again.")
        }
        _ => "The session could not be transferred.".into(),
    }
}

fn transfer_session(id: u64, opts: &Value) -> UResult<()> {
    if !opts.is_object() {
        return Err("Invalid transfer request.".into());
    }
    let mode = match opts.get("mode").and_then(Value::as_str) {
        Some(m @ ("copy" | "move")) => m.to_string(),
        _ => return Err("Invalid transfer mode.".into()),
    };
    let session_id = check_session_id(opts.get("sessionId").unwrap_or(&Value::Null))?;
    let from_id = opts.get("fromId").cloned().unwrap_or(Value::Null);
    let to_id = opts.get("toId").cloned().unwrap_or(Value::Null);
    if from_id == to_id {
        return Err("Pick a different account.".into());
    }
    let (from_s, to_s, to_dir, to_name, sessions) = state_call_raw(|st| {
        let from = find_account(st, &from_id)?;
        let to = find_account(st, &to_id)?;
        let fs = session_account(st, &from)?;
        let ts = session_account(st, &to)?;
        Ok((fs, ts, proc_dir(&to), to.name.clone(), sessions_clone(st)))
    })?;
    spawn_then(
        move || {
            let mut to_s = to_s;
            // The cached account list can be a poll behind; ask for the target's live state.
            to_s.running =
                claude_app::list_processes().iter().any(|p| claude_app::matches_data_dir(p, to_dir.as_deref()));
            sessions.transfer(&from_s, &to_s, &session_id, &mode)
        },
        move |r| {
            with_state(request_poll);
            let v = if r.ok {
                serde_json::to_value(&r).unwrap_or_else(|_| ok())
            } else {
                let error = r
                    .error
                    .clone()
                    .filter(|e| !e.is_empty())
                    .unwrap_or_else(|| reason_text(r.reason.as_deref(), &to_name));
                json!({ "ok": false, "reason": r.reason, "error": error })
            };
            resolve(id, v);
        },
    );
    Ok(())
}

fn delete_session(id: u64, acc_id: &Value, session_id: &Value) -> UResult<()> {
    let sid = check_session_id(session_id)?;
    let (sa, sessions) = state_call_raw(|st| {
        let acc = find_account(st, acc_id)?;
        Ok((session_account(st, &acc)?, sessions_clone(st)))
    })?;
    spawn_then(
        move || match sessions.delete(&sa, &sid) {
            Ok(()) => ok(),
            Err(e) => api_fail(&e),
        },
        move |v| {
            with_state(request_poll);
            resolve(id, v);
        },
    );
    Ok(())
}

fn open_session_folder(id: u64, acc_id: &Value, session_id: &Value) -> UResult<()> {
    let sid = check_session_id(session_id)?;
    let (sa, sessions) = state_call_raw(|st| {
        let acc = find_account(st, acc_id)?;
        Ok((session_account(st, &acc)?, sessions_clone(st)))
    })?;
    spawn_then(
        move || {
            let list = sessions.list(&sa);
            let Some(s) = list.iter().find(|s| s.session_id == sid).filter(|s| !s.cwd.is_empty()) else {
                return fail("Session folder not found.");
            };
            if !Path::new(&s.cwd).exists() {
                return fail("That folder no longer exists.");
            }
            if is_smoke() {
                return ok();
            }
            logged(<Os as Shell>::reveal(Path::new(&s.cwd)));
            ok()
        },
        move |v| resolve(id, v),
    );
    Ok(())
}

/// Log a failed shell open; the user simply sees nothing open.
fn logged(r: Result<(), String>) {
    if let Err(e) = r {
        log::warn(&e);
    }
}

fn open_external(url: &Value) {
    let Some(u) = url.as_str() else { return };
    let lower = u.to_ascii_lowercase();
    if js_len(u) > 2048 || !(lower.starts_with("https:") || lower.starts_with("http:")) {
        return;
    }
    if is_smoke() {
        log::info("smoke: blocked openExternal");
        return;
    }
    logged(<Os as Shell>::open_url(u));
}

// ------------------------------------------------------------------ about

/// `getAppInfo` shape: product identity, links and the license name for the About card, the
/// system's name, and `translocated` (this copy runs from a place it cannot stay in).
fn app_info() -> Value {
    json!({
        "productName": brand::NAME,
        "version": brand::VERSION,
        "buildDate": brand::build_date(),
        "tagline": brand::TAGLINE,
        "homeUrl": brand::URL_HOME,
        "supportUrl": brand::URL_SUPPORT,
        "releasesUrl": brand::URL_RELEASES,
        "sponsorUrl": brand::URL_SPONSOR,
        "licenseName": brand::LICENSE_NAME,
        "disclaimer": brand::DISCLAIMER,
        "platform": platform::OS_NAME,
        "translocated": <Os as Misc>::translocated(),
    })
}

/// Open `LICENSE.txt` next to the exe, else the license file in the repository.
fn open_license() {
    if is_smoke() {
        log::info("smoke: blocked openLicense");
        return;
    }
    let local =
        std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("LICENSE.txt"))).filter(|p| p.is_file());
    match local {
        Some(p) => logged(<Os as Shell>::open_url(&p.to_string_lossy())),
        None => logged(<Os as Shell>::open_url(brand::URL_LICENSE)),
    }
}

/// Files Sessionmux does not use; removed at start if present.
const STALE_FILES: [&str; 1] = ["license.json"];

/// Delete the stale files if present. Logs only what it removed or could not remove; never fails.
fn remove_stale_state(user_data: &Path) {
    for name in STALE_FILES {
        let p = user_data.join(name);
        if p.is_file() {
            match std::fs::remove_file(&p) {
                Ok(()) => log::info(&format!("cleanup: removed {}", p.display())),
                Err(e) => log::warn(&format!("cleanup: could not remove {}: {e}", p.display())),
            }
        }
    }
}

// ------------------------------------------------------------------ settings

fn panel_settings(st: &AppState) -> Value {
    let mut v = serde_json::to_value(&st.settings).unwrap_or_else(|_| json!({}));
    if let Some(o) = v.as_object_mut() {
        // The store leaves these out while off; the page always gets both.
        o.insert("autoHandover".into(), json!(st.settings.auto_handover));
        o.insert("approveFromPanel".into(), json!(st.settings.approve_from_panel));
        o.insert("appVersion".into(), json!(env!("CARGO_PKG_VERSION")));
        // What the system says about login start, the window-raising permission and routing.
        o.insert("autostartStatus".into(), json!(<Os as Autostart>::status().as_str()));
        if let Some(trusted) = <Os as Focus>::accessibility_trusted(false) {
            o.insert("accessibilityTrusted".into(), json!(trusted));
        }
        if let Some(why) = <Os as UrlHandler>::unavailable() {
            o.insert("signInRoutingUnavailable".into(), json!(why));
        }
        o.insert(
            "paths".into(),
            json!({
                "store": st.store.store_path().to_string_lossy(),
                "log": paths::log_file().to_string_lossy(),
                "transcripts": paths::home().join(".claude").join("projects").to_string_lossy(),
                "mainDataDir": main_data_dir(st),
            }),
        );
    }
    v
}

fn push_settings_if_changed(st: &mut AppState) {
    let v = panel_settings(st);
    let s = v.to_string();
    if s == st.last_settings_json {
        return;
    }
    st.last_settings_json = s;
    emit(st, "settings", &v);
}

fn update_settings(st: &mut AppState, patch: Value) -> Settings {
    let prev = st.settings.clone();
    let move_bubble = patch.get("bubblePosition").is_some();
    let next = match st.store.set_settings(patch) {
        Ok(s) => s,
        Err(e) => {
            log::warn(&format!("setSettings: {}", e.error));
            st.store.settings()
        }
    };
    st.settings = next.clone();
    apply_settings(st, &prev, &next, move_bubble);
    push_settings_if_changed(st);
    next
}

fn apply_login_item(enabled: bool) {
    if is_smoke() {
        return;
    }
    if let Err(e) = AccountsStore::start_with_windows(enabled) {
        log::warn(&format!("start with Windows: {e}"));
    }
}

/// Apply the side effects of a settings change. The bubble only moves when the patch set bubblePosition.
fn apply_settings(st: &mut AppState, prev: &Settings, next: &Settings, move_bubble: bool) {
    // Theme: panel.js applies data-theme from the `settings` event (pushed by the caller).
    if prev.start_with_windows != next.start_with_windows {
        apply_login_item(next.start_with_windows);
    }
    if prev.show_bubble != next.show_bubble {
        set_bubble_visible(st, next.show_bubble);
    }
    if !prev.share_mcp_config && next.share_mcp_config {
        sync_all_mcp(st);
    }
    if prev.sign_in_routing && !next.sign_in_routing {
        end_sign_in(st, "disabled");
    }
    if !prev.raise_windows_with_accessibility && next.raise_windows_with_accessibility && !is_smoke() {
        // Turned on: the system may show its grant prompt now rather than at the first switch.
        let _ = <Os as Focus>::accessibility_trusted(true);
    }
    if !prev.sign_in_routing && next.sign_in_routing && !is_smoke() {
        // Turned on while an extra account already waits to sign in (the routing-off card's
        // "Turn on"): route that account now instead of only on its next open. The most recently
        // opened one wins when several qualify. Posted: begin_sign_in takes the state itself.
        let waiting: Vec<Account> =
            fresh_accounts(st).into_iter().filter(|a| !a.is_main && a.running && !a.signed_in).collect();
        let pick = waiting
            .iter()
            .find(|a| st.last_opened.as_deref() == Some(a.id.as_str()))
            .or_else(|| waiting.first())
            .map(|a| a.id.clone());
        if let Some(id) = pick {
            post(move || begin_sign_in(&id));
        }
    }
    if prev.approve_from_panel && !next.approve_from_panel {
        // Every helper still waiting falls back to the prompt in Claude.
        st.approvals.clear();
        st.waiting.clear();
    }
    if move_bubble && prev.bubble_position != next.bubble_position {
        if let (Some(b), Some(p)) = (st.bubble.as_ref(), next.bubble_position.as_ref()) {
            let r = b.rect();
            if r.x != p.x || r.y != p.y {
                b.set_position(p.x, p.y);
            }
        }
    }
    sync_bubble(st);
    update_tray(st);
}

fn persist_bubble_position(st: &mut AppState, x: i32, y: i32) {
    st.settings.bubble_position = Some(Point { x, y });
    st.last_settings_json = panel_settings(st).to_string();
    if let Err(e) = st.store.set_settings(json!({ "bubblePosition": { "x": x, "y": y } })) {
        log::warn(&format!("persist bubble failed: {}", e.error));
    }
}

// ------------------------------------------------------------------ poll & active account

/// Recompute accounts / discovered / active from the cached process list and push diffs.
fn refresh(st: &mut AppState) {
    let accounts = fresh_accounts(st);
    let json = serde_json::to_string(&accounts).unwrap_or_default();
    if json != st.last_accounts_json {
        st.last_accounts_json = json;
        st.last_accounts = accounts;
        let v = serde_json::to_value(&st.last_accounts).unwrap_or(json!([]));
        emit(st, "accounts-changed", &v);
        update_tray(st);
        sync_bubble(st);
    }
    let discovered = fresh_discovered(st);
    let djson = serde_json::to_string(&discovered).unwrap_or_default();
    if st.last_discovered_json.as_deref() != Some(djson.as_str()) {
        st.last_discovered_json = Some(djson);
        st.last_discovered = discovered;
        let v = serde_json::to_value(&st.last_discovered).unwrap_or(json!([]));
        emit(st, "discovered-changed", &v);
    }
    // Active account: the foreground Claude window's account, else the last opened one.
    let fg = st.fg_pid.and_then(|pid| st.last_accounts.iter().find(|a| a.pid == Some(pid)).map(|a| a.id.clone()));
    if let Some(id) = fg.clone() {
        st.last_opened = Some(id);
    }
    let target = fg.or_else(|| st.last_opened.clone());
    set_active(st, target.as_deref());
    push_settings_if_changed(st);
}

fn on_poll(procs: Vec<Proc>, fg: Option<u32>, located: Option<Located>) {
    with_state(|st| {
        st.procs = procs;
        st.fg_pid = fg;
        if located.is_some() && located != st.located {
            st.located = located;
        }
        expire_session_state(st, relay::now_secs());
        refresh(st);
        log_procs_if_changed(st);
        start_index(st);
    });
}

/// Log the Claude processes and the account each one was matched to, when that changes.
fn log_procs_if_changed(st: &mut AppState) {
    let line = procs_summary(&st.procs, &st.last_accounts);
    if line != st.last_procs_log {
        log::info(&format!("claude processes: {line}"));
        st.last_procs_log = line;
    }
}

/// One entry per process: `pid=P acc=<id|none> dir=<data folder|default> window=<bool>`.
fn procs_summary(procs: &[Proc], accounts: &[Account]) -> String {
    if procs.is_empty() {
        return "none".into();
    }
    procs
        .iter()
        .map(|p| {
            let acc = accounts.iter().find(|a| a.pid == Some(p.pid)).map(|a| a.id.as_str()).unwrap_or("none");
            let dir = p.data_dir.as_deref().unwrap_or("default");
            format!("pid={} acc={acc} dir={dir} window={}", p.pid, p.hwnd != 0)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn start_poll_thread() -> Option<mpsc::Sender<()>> {
    let (tx, rx) = mpsc::channel::<()>();
    let spawned = std::thread::Builder::new().name("sessionmux-poll".into()).spawn(move || {
        let mut watch = gesture::DebugWatch::default();
        let mut next_debug_check = Instant::now() + DEBUGGER_FIRST;
        'poll: loop {
            if QUITTING.load(Ordering::Relaxed) {
                break;
            }
            let procs = claude_app::list_processes();
            let fg = <Os as Windows>::foreground_pid();
            let loc = claude_app::locate().ok();
            post(move || on_poll(procs, fg, loc));
            let next_poll = Instant::now() + Duration::from_millis(POLL_MS);
            // Wait for the next poll (or a wake-up), running the debugger check on its own clock.
            loop {
                let now = Instant::now();
                if now >= next_debug_check {
                    next_debug_check = now + DEBUGGER_EVERY;
                    if watch.update(<Os as Debugger>::present()) {
                        post(|| tamper(Reason::Debugger));
                    }
                }
                if now >= next_poll {
                    break;
                }
                match rx.recv_timeout(next_poll.min(next_debug_check).saturating_duration_since(now)) {
                    Ok(()) => {
                        while rx.try_recv().is_ok() {}
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break 'poll,
                }
            }
        }
    });
    match spawned {
        Ok(_) => Some(tx),
        Err(e) => {
            log::error(&format!("poll thread: {e}"));
            None
        }
    }
}

/// A tamper trigger: gated by [`gesture::armed`], rate-limited, logged, and the cosmetic gesture
/// beside the bubble. Never blocks.
fn tamper(reason: Reason) {
    with_state(|st| {
        let exiting = QUITTING.load(Ordering::Relaxed);
        if !gesture::armed(reason, st.started.elapsed(), is_smoke(), exiting) {
            return;
        }
        let limit = if reason == Reason::Debugger { &mut st.debugger_limit } else { &mut st.tamper_limit };
        if !limit.allow(Instant::now()) {
            return;
        }
        log::info(&format!("tamper: {}", reason.as_str()));
        let anchor = st.bubble.as_ref().filter(|b| st.settings.show_bubble && b.is_visible()).map(|b| b.rect());
        <Os as GestureHost>::play(anchor);
    });
}

/// Set the active account: falls back to the current active, Main, then the first account.
fn set_active(st: &mut AppState, id: Option<&str>) {
    let list = &st.last_accounts;
    let acc = id
        .and_then(|id| list.iter().find(|a| a.id == id))
        .or_else(|| st.last_active.as_deref().and_then(|cur| list.iter().find(|a| a.id == cur)))
        .or_else(|| list.iter().find(|a| a.is_main))
        .or_else(|| list.first())
        .cloned();
    let Some(acc) = acc else { return };
    st.last_active = Some(acc.id.clone());
    let payload = json!({
        "id": acc.id,
        "name": acc.name,
        "color": acc.color,
        "initials": if acc.initials.is_empty() { crate::core::accounts::initials(&acc.name) } else { acc.initials.clone() },
        "running": acc.running,
    });
    sync_bubble(st);
    let s = payload.to_string();
    if s == st.last_active_json {
        return;
    }
    st.last_active_json = s;
    emit(st, "active-account", &payload);
}

// ------------------------------------------------------------------ bubble

fn bubble_mode_str(st: &AppState) -> &'static str {
    if st.settings.bubble_mode == "tray" {
        "tray"
    } else {
        "icon"
    }
}

fn parse_color(c: &str) -> u32 {
    c.strip_prefix('#').filter(|h| h.len() == 6).and_then(|h| u32::from_str_radix(h, 16).ok()).unwrap_or(0x8A7CFF)
}

fn initials_of(a: &Account) -> String {
    if a.initials.is_empty() {
        crate::core::accounts::initials(&a.name)
    } else {
        a.initials.clone()
    }
}

fn bubble_state(st: &AppState) -> BubbleState {
    let to_b = |a: &Account| BubbleAccount {
        id: a.id.clone(),
        name: a.name.clone(),
        color: parse_color(&a.color),
        initials: initials_of(a),
        running: a.running,
        active: Some(&a.id) == st.last_active.as_ref(),
        needs_sign_in: a.running && !a.signed_in,
        limited: a.limited.is_some(),
    };
    BubbleState {
        mode: if bubble_mode_str(st) == "tray" { BubbleMode::Tray } else { BubbleMode::Icon },
        active: st.last_accounts.iter().find(|a| Some(&a.id) == st.last_active.as_ref()).map(to_b),
        accounts: st.last_accounts.iter().take(TRAY_MAX_ACCOUNTS).map(to_b).collect(),
        waiting: st.approvals.len(),
    }
}

/// Re-render / resize the native bubble when anything changed.
fn sync_bubble(st: &mut AppState) {
    let s = bubble_state(st);
    if st.last_bubble_state.as_ref() != Some(&s) {
        st.last_bubble_state = Some(s.clone());
        let mut moved = None;
        if let Some(b) = st.bubble.as_mut() {
            let before = b.rect();
            b.set_state(s);
            let after = b.rect();
            if (before.w, before.h) != (after.w, after.h) {
                moved = Some((after.x, after.y));
            }
        }
        if let Some((x, y)) = moved {
            // Only re-persist a position the user already has (a never-dragged bubble keeps the default).
            if let Some(saved) = st.settings.bubble_position.as_ref() {
                if saved.x != x || saved.y != y {
                    persist_bubble_position(st, x, y);
                }
            }
            reposition_panel(st);
        }
    }
}

fn bubble_event_sink() -> Box<dyn Fn(BubbleEvent)> {
    Box::new(move |ev| post(move || on_event(PlatformEvent::Bubble(ev))))
}

fn create_bubble(st: &mut AppState) {
    let pos = st.settings.bubble_position.as_ref().map(|p| (p.x, p.y));
    match Bubble::create(pos, bubble_event_sink()) {
        Ok(b) => {
            st.bubble = Some(b);
            st.last_bubble_state = None;
            sync_bubble(st);
        }
        Err(e) => log::warn(&format!("bubble: {e}")),
    }
}

fn set_bubble_visible(st: &mut AppState, visible: bool) {
    if visible {
        match st.bubble.as_ref() {
            None => create_bubble(st),
            Some(b) => b.show(true),
        }
    } else if let Some(b) = st.bubble.as_ref() {
        b.show(false);
    }
}

fn on_bubble_event(ev: BubbleEvent) {
    log::debug(&format!("bubble event: {ev:?}"));
    match ev {
        BubbleEvent::Click | BubbleEvent::RightClick | BubbleEvent::MoreClick => toggle_panel(),
        BubbleEvent::AvatarClick(id) => {
            if let Err(e) = open_account(0, &Value::String(id.clone()), true, OpenFrom::Bubble) {
                log::warn(&format!("bubble open {id}: {e}"));
            }
        }
        BubbleEvent::DragEnd(x, y) => {
            with_state(|st| {
                persist_bubble_position(st, x, y);
                reposition_panel(st);
            });
        }
    }
}

// ------------------------------------------------------------------ tray

fn update_tray(st: &mut AppState) {
    let Some(t) = st.tray.clone() else { return };
    let items: Vec<(String, String, bool)> =
        st.last_accounts.iter().map(|a| (a.id.clone(), a.name.clone(), a.running)).collect();
    let shown = st.settings.show_bubble;
    let mode = bubble_mode_str(st);
    let applied = match t.try_borrow_mut() {
        Ok(mut t) => {
            t.set_menu(&TrayMenu::new(&items, shown, mode));
            t.set_waiting(st.approvals.len());
            true
        }
        Err(_) => false,
    };
    if !applied {
        st.tray_dirty = true; // menu open (modal loop); applied when it closes
    }
}

/// A tray icon notification: the tray reads it (a right click runs the menu here), then the
/// menu updates that arrived meanwhile are applied.
#[cfg(windows)]
fn on_tray_notify(n: platform::event_loop::TrayNotify) {
    let Some(tray) = with_state(|st| st.tray.clone()).flatten() else { return };
    let ev = match tray.try_borrow() {
        Ok(t) => platform::event_loop::tray_event(&t, &n),
        Err(_) => None,
    };
    with_state(|st| {
        if st.tray_dirty {
            st.tray_dirty = false;
            update_tray(st);
        }
    });
    if let Some(ev) = ev {
        on_tray_event(ev);
    }
}

fn on_tray_event(ev: TrayEvent) {
    log::debug(&format!("tray event: {ev:?}"));
    match ev {
        TrayEvent::LeftClick => toggle_panel(),
        TrayEvent::OpenAccount(id) => {
            if let Err(e) = open_account(0, &Value::String(id.clone()), true, OpenFrom::Tray) {
                log::warn(&format!("tray open {id}: {e}"));
            }
        }
        TrayEvent::OpenAll => {
            if let Err(e) = open_all(0) {
                log::warn(&format!("tray openAll: {e}"));
            }
        }
        TrayEvent::ToggleBubble => {
            with_state(|st| {
                let v = !st.settings.show_bubble;
                update_settings(st, json!({ "showBubble": v }));
            });
        }
        TrayEvent::BubbleMode(m) => {
            with_state(|st| {
                if (m == "icon" || m == "tray") && m != st.settings.bubble_mode {
                    update_settings(st, json!({ "bubbleMode": m }));
                }
            });
        }
        TrayEvent::OpenSettings => show_tab("settings"),
        TrayEvent::Quit => quit(),
    }
}

// ------------------------------------------------------------------ panel lifecycle

fn emit(st: &AppState, event: &str, payload: &Value) {
    #[cfg(target_os = "macos")]
    crate::focuscheck::note_event(event, payload);
    if let Some(p) = st.panel.as_ref() {
        p.emit(event, payload);
    }
}

/// After the page (re)loaded, give it the current state.
fn resend_state(st: &mut AppState) {
    let s = panel_settings(st);
    st.last_settings_json = s.to_string();
    emit(st, "settings", &s);
    if !st.last_accounts.is_empty() {
        emit(st, "accounts-changed", &serde_json::to_value(&st.last_accounts).unwrap_or(json!([])));
    }
    if st.last_discovered_json.is_some() {
        emit(st, "discovered-changed", &serde_json::to_value(&st.last_discovered).unwrap_or(json!([])));
    }
    if let Ok(v) = serde_json::from_str::<Value>(&st.last_active_json) {
        emit(st, "active-account", &v);
    }
    push_sign_in(st, None);
}

/// Make sure the panel exists, creating it without any AppState borrow held (create pumps).
/// `show_early`: show the bare native window at its target place while WebView2 starts.
fn ensure_panel(show_early: bool) -> bool {
    let need = with_state(|st| {
        if st.panel.is_some() {
            Some(false)
        } else if st.panel_creating {
            None
        } else {
            st.panel_creating = true;
            Some(true)
        }
    })
    .flatten();
    let Some(need) = need else { return false };
    if !need {
        return true;
    }
    if with_state(|_| ()).is_none() {
        return false;
    }
    let ipc: Arc<dyn IpcHandler> = Arc::new(AppIpc);
    let t0 = Instant::now();
    let near = if show_early { with_state(|st| (panel_anchor(st), st.settings.panel_side.clone())) } else { None };
    // New page, new id counter: replies for the old one must not reach it (see tag_id).
    PANEL_GEN.fetch_add(1, Ordering::Relaxed);
    let res = match near.as_ref() {
        Some((r, side)) => Panel::create_near(ipc, Some((*r, side.as_str()))),
        None => Panel::create(ipc),
    };
    with_state(|st| {
        st.panel_creating = false;
        match res {
            Ok(p) => {
                log::debug(&format!("panel created in {} ms", t0.elapsed().as_millis()));
                st.panel = Some(p);
                true
            }
            Err(e) => {
                log::warn(&format!("panel: {e}"));
                false
            }
        }
    })
    .unwrap_or(false)
}

/// The visible bubble's rect; an empty rect places the panel at the cursor (tray icon / hotkey).
fn panel_anchor(st: &AppState) -> Rect {
    match st.bubble.as_ref() {
        Some(b) if st.settings.show_bubble && b.is_visible() => b.rect(),
        _ => Rect::default(),
    }
}

fn reposition_panel(st: &AppState) {
    if let Some(p) = st.panel.as_ref().filter(|p| p.is_visible()) {
        p.show_near(panel_anchor(st), &st.settings.panel_side);
    }
}

/// Show the panel (creating it on demand) next to the bubble.
pub fn show_panel() {
    if !ensure_panel(true) {
        return;
    }
    with_state(|st| {
        kill_timer(TimerId::DestroyPanel);
        let anchor = panel_anchor(st);
        if let Some(p) = st.panel.as_ref() {
            p.show_near(anchor, &st.settings.panel_side);
        }
        request_poll(st);
    });
}

/// Hide the panel; it is destroyed 30 s later (WebView2 processes go away).
fn hide_panel(from_blur: bool) {
    with_state(|st| hide_panel_in(st, from_blur));
}

fn hide_panel_in(st: &mut AppState, from_blur: bool) {
    let Some(p) = st.panel.as_ref() else { return };
    if !p.is_visible() {
        return;
    }
    p.hide();
    if from_blur {
        st.last_blur_hide = Some(Instant::now());
    }
    set_timer(TimerId::DestroyPanel, PANEL_DESTROY_MS);
}

fn pinned(st: &AppState) -> bool {
    st.pinned_until.is_some_and(|t| Instant::now() < t)
}

fn on_blur() {
    with_state(|st| {
        if is_smoke() || pinned(st) {
            return;
        }
        hide_panel_in(st, true);
    });
}

/// Hide the panel if it lost focus while pinned (blur events were ignored then).
fn hide_if_unfocused(st: &mut AppState) {
    if is_smoke() || pinned(st) {
        return;
    }
    let Some(p) = st.panel.as_ref().filter(|p| p.is_visible()) else { return };
    if <Os as Desktop>::is_foreground(p.window()) {
        return;
    }
    hide_panel_in(st, true);
}

fn pin_panel(pin: bool) {
    with_state(|st| {
        kill_timer(TimerId::Pin);
        if pin {
            st.pinned_until = Some(Instant::now() + Duration::from_millis(PIN_MS as u64));
            set_timer(TimerId::Pin, PIN_MS + 50);
        } else {
            st.pinned_until = None;
            // A blur that arrived while pinned was ignored; honour it now.
            set_timer(TimerId::Pin, 50);
        }
    });
}

/// Hotkey / bubble click: show or hide the panel.
pub fn toggle_panel() {
    let (visible, recent_blur) = with_state(|st| {
        (
            st.panel.as_ref().is_some_and(|p| p.is_visible()),
            st.last_blur_hide.is_some_and(|t| t.elapsed() < BLUR_TOGGLE_GRACE),
        )
    })
    .unwrap_or((false, false));
    if visible {
        hide_panel(false);
    } else if !recent_blur {
        // The click on the bubble/tray that toggles us also blurred (and hid) the panel a
        // moment ago; treat that as "close", not "reopen".
        show_panel();
    }
}

fn show_tab(tab: &str) {
    show_panel();
    with_state(|st| emit(st, "show-tab", &json!(tab)));
}

fn on_timer(id: TimerId) {
    match id {
        TimerId::DestroyPanel => {
            let panel = with_state(|st| {
                kill_timer(TimerId::DestroyPanel);
                if st.panel.as_ref().is_some_and(|p| !p.is_visible()) {
                    st.panel.take()
                } else {
                    None
                }
            })
            .flatten();
            if let Some(p) = panel {
                p.destroy(); // outside the borrow: destroying the WebView may dispatch messages
                trim_working_set();
                log::debug("panel destroyed after 30 s hidden");
            }
        }
        TimerId::StartupTrim => {
            let idle = with_state(|st| {
                kill_timer(TimerId::StartupTrim);
                st.panel.is_none() && !st.panel_creating
            })
            .unwrap_or(false);
            if idle {
                trim_working_set();
                log::debug("startup working-set trim");
            }
        }
        TimerId::Pin => {
            with_state(|st| {
                kill_timer(TimerId::Pin);
                hide_if_unfocused(st);
            });
        }
        TimerId::SignIn => {
            with_state(sign_in_tick);
        }
    }
}

/// Everything the system reports, on the UI thread from the top-level loop.
fn on_event(ev: PlatformEvent) {
    match ev {
        PlatformEvent::ShowPanel => show_panel(),
        PlatformEvent::Gesture(reason) => tamper(reason),
        // The installer and uninstaller ask for this too: quit cleanly (ends a sign-in relay,
        // keeps the Claude Desktop windows running).
        PlatformEvent::QuitRequested => quit(),
        PlatformEvent::Hotkey(id) if id == HOTKEY_ID => toggle_panel(),
        PlatformEvent::Hotkey(_) => {}
        PlatformEvent::Timer(id) => on_timer(id),
        PlatformEvent::Tray(ev) => on_tray_event(ev),
        #[cfg(windows)]
        PlatformEvent::TrayNotify(n) => on_tray_notify(n),
        PlatformEvent::Bubble(ev) => on_bubble_event(ev),
        PlatformEvent::OpenUrl(url) => on_open_url(&url),
        PlatformEvent::PanelBlur => on_blur(),
        PlatformEvent::DisplayChanged => {
            with_state(|st| reposition_panel(st));
        }
    }
}

/// After the panel's web view is gone, hand the pages it touched (loader, heaps) back to the
/// OS so the idle footprint returns to "bubble only". Pages are faulted back in on demand.
fn trim_working_set() {
    <Os as Misc>::trim_memory();
}

/// True when running from an installed location rather than a cargo target dir.
fn is_packaged() -> bool {
    std::env::current_exe().map(|p| !is_cargo_build(&p)).unwrap_or(false)
}

/// A cargo build lives in `<target dir>\{debug,release}\` next to cargo's `deps` and
/// `.fingerprint` folders, whatever the target dir is called.
fn is_cargo_build(exe: &Path) -> bool {
    let Some(profile_dir) = exe.parent() else { return false };
    let profile = profile_dir.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    (profile == "release" || profile == "debug")
        && (profile_dir.join("deps").is_dir() || profile_dir.join(".fingerprint").is_dir())
}

// ------------------------------------------------------------------ approvals, waiting sessions, limits

/// Start the hook helpers' pipe; every request is handled on the UI thread. A show request (a
/// second launch where the system has no window message for it) opens the panel.
fn start_pipe() -> Option<Server> {
    let on_request = Box::new(move |req: Request, resp: Responder| {
        post(move || on_pipe_request(req, resp));
    });
    let on_show = Box::new(|| post(|| on_event(PlatformEvent::ShowPanel)));
    match Server::start_with_show(on_request, on_show) {
        Ok(s) => Some(s),
        Err(e) => {
            log::warn(&format!("pipe: not started: {e}"));
            None
        }
    }
}

/// A tool name as it may appear in the log: letters, digits, `_`, `-` and `.`, at most 64.
fn log_tool(name: &str) -> String {
    let s: String =
        name.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')).take(64).collect();
    if s.is_empty() {
        "tool".into()
    } else {
        s
    }
}

/// A session id the helpers sent, usable as a key: 1-128 visible ASCII characters.
fn helper_session_ok(s: &str) -> bool {
    (1..=128).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_graphic())
}

/// The `waiting` key of the session Claude Code calls `cli`.
fn session_key(st: &AppState, cli: &str) -> (String, String) {
    match st.index.get(cli) {
        Some(s) => (s.account_id.clone(), s.session_id.clone()),
        None => (String::new(), cli.to_string()),
    }
}

fn account_name(st: &AppState, id: &str) -> String {
    st.last_accounts.iter().find(|a| a.id == id).map(|a| a.name.clone()).unwrap_or_else(|| "Claude".into())
}

/// The badge count and tooltip follow the pending approvals.
fn approvals_changed(st: &mut AppState) {
    sync_bubble(st);
    update_tray(st);
}

/// UI thread: one request from a hook helper.
fn on_pipe_request(req: Request, responder: Responder) {
    with_state(move |st| match req {
        Request::Approve { request_id, session_id, cwd, tool_name, mcp_server, summary, .. } => {
            // Dropping the responder leaves the helper silent, so the prompt in Claude decides.
            if !st.settings.approve_from_panel || !helper_session_ok(&session_id) {
                return;
            }
            if st.approvals.len() >= MAX_APPROVALS || st.approvals.iter().any(|a| a.id == request_id) {
                return;
            }
            let mapped = st.index.get(&session_id).cloned();
            let (account_id, account_name) = match &mapped {
                Some(m) => (m.account_id.clone(), account_name(st, &m.account_id)),
                None => (String::new(), "Claude".to_string()),
            };
            log::info(&format!("approval: {} waits in session {session_id}", log_tool(&tool_name)));
            let now = relay::now_secs();
            st.waiting.insert(session_key(st, &session_id), Waiting { kind: WaitingKind::Permission, since: now });
            st.approvals.push(PendingApproval {
                id: request_id,
                account_id,
                account_name,
                session_id: mapped.as_ref().map(|m| m.session_id.clone()),
                session_title: mapped.as_ref().map(|m| m.title.clone()).filter(|t| !t.is_empty()),
                cli_session_id: session_id,
                cwd,
                tool_name,
                mcp_server,
                summary,
                received_at: now,
                responder: Some(responder),
            });
            if mapped.is_none() {
                request_poll(st);
            }
            approvals_changed(st);
        }
        Request::Notify { event, notification_type, session_id, message, .. } => {
            drop(responder);
            if helper_session_ok(&session_id) {
                on_notify(st, &session_id, notify_effect(&event, notification_type.as_deref(), &message));
            }
        }
    });
}

/// What one `--notify-hook` event means.
#[derive(Clone, Debug, PartialEq)]
enum NotifyEffect {
    /// The session waits for the user.
    Wait(WaitingKind),
    /// The turn finished: the session no longer waits, and the account is past its limit.
    Completed,
    /// The account hit its plan limit.
    Limit(LimitHit),
    /// Claude Code resumed after the limit reset.
    LimitCleared,
    Nothing,
}

fn notify_effect(event: &str, kind: Option<&str>, message: &str) -> NotifyEffect {
    match (event, kind) {
        ("Notification", Some("permission_prompt")) => NotifyEffect::Wait(WaitingKind::Permission),
        ("Notification", Some("idle_prompt")) => NotifyEffect::Wait(WaitingKind::Idle),
        ("Notification", Some("agent_needs_input")) => NotifyEffect::Wait(WaitingKind::NeedsInput),
        ("Notification", Some("agent_completed")) => NotifyEffect::Completed,
        ("Notification", Some("quota_auto_resume_fired")) => NotifyEffect::LimitCleared,
        ("Notification", Some("quota_auto_resume_stale" | "quota_auto_resume_disabled")) => {
            // Still limited; the message carries no reset time in the versions seen so far.
            let hit = limits::classify(message).unwrap_or(LimitHit {
                kind: LimitKind::Session,
                reset_text: None,
                until: None,
                source: LimitSource::Notification,
            });
            NotifyEffect::Limit(LimitHit { source: LimitSource::Notification, ..hit })
        }
        // A throttle or any other API error is not a limit hit (classify says None).
        ("StopFailure", Some("rate_limit")) => match limits::classify(message) {
            Some(hit) => NotifyEffect::Limit(LimitHit { source: LimitSource::HookStopFailure, ..hit }),
            None => NotifyEffect::Nothing,
        },
        _ => NotifyEffect::Nothing,
    }
}

fn on_notify(st: &mut AppState, cli: &str, effect: NotifyEffect) {
    let now = relay::now_secs();
    let key = session_key(st, cli);
    let account = st.index.get(cli).map(|s| s.account_id.clone());
    match effect {
        NotifyEffect::Nothing => return,
        NotifyEffect::Wait(kind) => {
            let w = st.waiting.entry(key).or_insert(Waiting { kind, since: now });
            if w.kind != kind {
                *w = Waiting { kind, since: now };
            }
        }
        NotifyEffect::Completed => {
            st.waiting.remove(&key);
            // The turn is over, so any approve request of it was answered in Claude.
            let before = st.approvals.len();
            st.approvals.retain(|a| a.cli_session_id != cli);
            if st.approvals.len() != before {
                approvals_changed(st);
            }
            if let Some(acc) = account.as_deref() {
                if st.limited.get(acc).is_some_and(|l| now >= l.since + COMPLETED_GRACE_SECS) {
                    clear_limit(st, acc, "a turn completed");
                }
            }
        }
        NotifyEffect::LimitCleared => match account.as_deref() {
            Some(acc) => clear_limit(st, acc, "Claude Code resumed"),
            None => st.unmapped_limits.retain(|(c, _, _)| c != cli),
        },
        NotifyEffect::Limit(hit) => match account.as_deref() {
            Some(acc) => note_limit(st, acc, hit, Some(key.1)),
            None => {
                st.unmapped_limits.retain(|(c, _, _)| c != cli);
                st.unmapped_limits.push((cli.to_string(), hit, now));
            }
        },
    }
    if account.is_none() {
        request_poll(st);
    }
}

fn limit_words(l: &Limited) -> (&'static str, &'static str) {
    let kind = match l.kind {
        LimitKind::Session => "session",
        LimitKind::Weekly => "weekly",
        LimitKind::Model => "model",
    };
    let source = match l.source {
        LimitSource::HookStopFailure => "turn ended on it",
        LimitSource::Transcript => "transcript",
        LimitSource::Notification => "quota notification",
    };
    (kind, source)
}

/// Account `account_id` hit its plan limit (any new signal re-arms the state), then maybe hand
/// `session` (or its most recent running session) over.
fn note_limit(st: &mut AppState, account_id: &str, hit: LimitHit, session: Option<String>) {
    let now = relay::now_secs();
    let next = Limited::from_hit(&hit, now);
    if !st.limited.contains_key(account_id) {
        let (kind, source) = limit_words(&next);
        log::info(&format!("limits: {account_id} hit its {kind} limit ({source})"));
    }
    st.limited.insert(account_id.to_string(), next);
    refresh(st);
    maybe_auto_handover(st, account_id, session);
}

fn clear_limit(st: &mut AppState, account_id: &str, why: &str) {
    if st.limited.remove(account_id).is_some() {
        log::info(&format!("limits: {account_id} is no longer limited ({why})"));
        refresh(st);
    }
}

/// The automatic hand-over of an account limited until `until` is due: once per limit window
/// (keyed on `until`), or once per [`HANDOVER_QUIET_SECS`] while the reset time is unknown.
fn handover_due(mark: Option<&HandoverMark>, until: Option<u64>, now: u64) -> bool {
    match mark {
        None => true,
        Some(m) => match (m.until, until) {
            (Some(before), Some(next)) => next != before && now >= before,
            _ => now >= m.at + HANDOVER_QUIET_SECS,
        },
    }
}

/// With automatic hand-over on, hand one running session of the limited account over: the one
/// the signal named, else its most recently active one. Never while that session has an approve
/// request waiting (tried again once it is answered).
fn maybe_auto_handover(st: &mut AppState, account_id: &str, session: Option<String>) {
    if !st.settings.auto_handover {
        return;
    }
    let Some(until) = st.limited.get(account_id).map(|l| l.until) else { return };
    let now = relay::now_secs();
    if !handover_due(st.handover_marks.get(account_id), until, now) {
        return;
    }
    let running = |sid: &str| st.index.values().any(|s| s.account_id == account_id && s.session_id == sid && s.running);
    let named = session.filter(|sid| running(sid));
    let pick = named.or_else(|| {
        st.index
            .values()
            .filter(|s| s.account_id == account_id && s.running)
            .max_by_key(|s| s.last_activity_at)
            .map(|s| s.session_id.clone())
    });
    let Some(sid) = pick else { return };
    if st.handovers.contains(&sid) || st.approvals.iter().any(|a| a.session_id.as_deref() == Some(sid.as_str())) {
        return;
    }
    st.handover_marks.insert(account_id.to_string(), HandoverMark { until, at: now });
    log::info(&format!("handover: {account_id} is limited; handing {sid} over"));
    match start_handover(st, account_id, &sid, None, 0, false) {
        Ok(Some(v)) => {
            log::info(&format!("handover: {sid} stays ({})", v["error"].as_str().unwrap_or("not handed over")))
        }
        Ok(None) => {}
        Err(e) => log::warn(&format!("handover: {sid}: {e}")),
    }
}

/// Try the automatic hand-over again for every limited account (after an approval left).
fn retry_auto_handover(st: &mut AppState) {
    if !st.settings.auto_handover {
        return;
    }
    let ids: Vec<String> = st.limited.keys().cloned().collect();
    for id in ids {
        maybe_auto_handover(st, &id, None);
    }
}

/// Drop approve requests older than the helper waits, stale waiting entries, limits that have
/// reset and limit signals that never found their session.
fn expire_session_state(st: &mut AppState, now: u64) {
    let before = st.approvals.len();
    // A helper that disconnected was answered in Claude (or gave up): its card goes, and so does
    // the session's permission wait.
    let left: Vec<(String, (String, String))> = st
        .approvals
        .iter()
        .filter(|a| a.responder.as_ref().is_some_and(Responder::is_gone))
        .map(|a| {
            let key = match &a.session_id {
                Some(s) => (a.account_id.clone(), s.clone()),
                None => session_key(st, &a.cli_session_id),
            };
            (a.id.clone(), key)
        })
        .collect();
    for (id, key) in left {
        st.approvals.retain(|a| a.id != id);
        if st.waiting.get(&key).is_some_and(|w| w.kind == WaitingKind::Permission) {
            st.waiting.remove(&key);
        }
    }
    let expiry = pipe::APPROVE_EXPIRY.as_secs();
    st.approvals.retain(|a| now < a.received_at + expiry);
    let approvals_left = st.approvals.len() != before;
    st.waiting.retain(|_, w| now < w.since + WAITING_EXPIRY_SECS);
    st.unmapped_limits.retain(|(_, _, at)| now < at + UNMAPPED_LIMIT_SECS);
    let reset: Vec<String> = st.limited.iter().filter(|(_, l)| l.expired(now)).map(|(k, _)| k.clone()).collect();
    for id in reset {
        clear_limit(st, &id, "the limit reset");
    }
    if approvals_left {
        approvals_changed(st);
        retry_auto_handover(st);
    }
}

/// What the index worker gets: the running accounts and the transcripts seen so far.
struct IndexJob {
    accounts: Vec<SessionAccount>,
    sessions: Sessions,
    seen: HashMap<PathBuf, TranscriptSeen>,
}

#[derive(Default)]
struct IndexResult {
    index: HashMap<String, IndexedSession>,
    seen: HashMap<PathBuf, TranscriptSeen>,
    /// (CLI session id, modification time in unix seconds) of transcripts written since the last scan.
    progressed: Vec<(String, u64)>,
    /// (account id, CLI session id, hit): a new limit hit, or `None` when a transcript whose
    /// newest reply was one has moved on.
    hits: Vec<(String, String, Option<LimitHit>)>,
}

fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A limit hit read from a transcript written at `mtime` counts while it can still be in force.
fn fresh_transcript_hit(hit: &LimitHit, mtime: u64, now: u64) -> bool {
    now.saturating_sub(mtime) <= limits::FALLBACK_SESSION_SECS && hit.until.is_none_or(|u| u > now)
}

/// Worker: list the sessions of the running accounts and scan the tail of every running
/// session's transcript whose modification time changed since the last scan.
fn build_index(job: IndexJob, now: u64) -> IndexResult {
    let mut out = IndexResult::default();
    let projects = job.sessions.home.join(".claude").join("projects");
    for a in &job.accounts {
        for s in job.sessions.list(a) {
            if s.cli_session_id.is_empty() {
                continue;
            }
            if s.running && s.has_transcript && !s.cwd.is_empty() {
                let path = projects.join(paths::slug(&s.cwd)).join(format!("{}.jsonl", s.cli_session_id));
                if let Ok(mtime) = std::fs::metadata(&path).and_then(|m| m.modified()) {
                    let prev = job.seen.get(&path).copied();
                    let mut hit = prev.is_some_and(|p| p.hit);
                    if prev.is_none_or(|p| p.mtime != mtime) {
                        let secs = unix_secs(mtime);
                        if prev.is_some() {
                            out.progressed.push((s.cli_session_id.clone(), secs));
                        }
                        let found = limits::scan_transcript_tail(&path).filter(|h| fresh_transcript_hit(h, secs, now));
                        match found {
                            Some(h) if !hit => {
                                hit = true;
                                out.hits.push((a.id.clone(), s.cli_session_id.clone(), Some(h)));
                            }
                            Some(_) => {}
                            None if hit => {
                                hit = false;
                                out.hits.push((a.id.clone(), s.cli_session_id.clone(), None));
                            }
                            None => {}
                        }
                    }
                    out.seen.insert(path, TranscriptSeen { mtime, hit });
                }
            }
            out.index.insert(
                s.cli_session_id.clone(),
                IndexedSession {
                    account_id: a.id.clone(),
                    session_id: s.session_id.clone(),
                    title: s.title.clone(),
                    running: s.running,
                    last_activity_at: s.last_activity_at,
                },
            );
        }
    }
    out
}

/// Rebuild the session index (and scan transcripts) on a worker, at most one at a time.
fn start_index(st: &mut AppState) {
    if st.index_busy || is_smoke() {
        return;
    }
    let running: Vec<Account> = st.last_accounts.iter().filter(|a| a.running).cloned().collect();
    let accounts: Vec<SessionAccount> = running.iter().filter_map(|a| session_account(st, a).ok()).collect();
    if accounts.is_empty() {
        st.index.clear();
        st.transcripts.clear();
        return;
    }
    st.index_busy = true;
    let job = IndexJob { accounts, sessions: sessions_clone(st), seen: st.transcripts.clone() };
    spawn_then(
        move || build_index(job, relay::now_secs()),
        |res| {
            with_state(|st| apply_index(st, res));
        },
    );
}

/// Fill in the account and session of approvals, waiting entries and limit signals that came
/// in before the index knew their session. True when an approval changed.
fn map_unmapped(st: &mut AppState) -> bool {
    let mut changed = false;
    for a in st.approvals.iter_mut().filter(|a| a.account_id.is_empty()) {
        if let Some(s) = st.index.get(&a.cli_session_id) {
            a.account_id = s.account_id.clone();
            a.account_name = st
                .last_accounts
                .iter()
                .find(|x| x.id == s.account_id)
                .map(|x| x.name.clone())
                .unwrap_or_else(|| a.account_name.clone());
            a.session_id = Some(s.session_id.clone());
            a.session_title = Some(s.title.clone()).filter(|t| !t.is_empty());
            changed = true;
        }
    }
    let unmapped: Vec<(String, String)> =
        st.waiting.keys().filter(|(acc, cli)| acc.is_empty() && st.index.contains_key(cli)).cloned().collect();
    for old in unmapped {
        if let Some(w) = st.waiting.remove(&old) {
            let key = session_key(st, &old.1);
            st.waiting.insert(key, w);
        }
    }
    let pending = std::mem::take(&mut st.unmapped_limits);
    for (cli, hit, at) in pending {
        match st.index.get(&cli).map(|s| (s.account_id.clone(), s.session_id.clone())) {
            Some((acc, sid)) => note_limit(st, &acc, hit, Some(sid)),
            None => st.unmapped_limits.push((cli, hit, at)),
        }
    }
    changed
}

fn apply_index(st: &mut AppState, res: IndexResult) {
    st.index_busy = false;
    st.index = res.index;
    st.transcripts = res.seen;
    if map_unmapped(st) {
        approvals_changed(st);
    }
    // A transcript written after the session started waiting means the user answered in Claude.
    for (cli, secs) in &res.progressed {
        let key = session_key(st, cli);
        if st.waiting.get(&key).is_some_and(|w| *secs > w.since + PROGRESS_GRACE_SECS) {
            st.waiting.remove(&key);
        }
    }
    for (acc, cli, hit) in res.hits {
        match hit {
            Some(h) => {
                let sid = st.index.get(&cli).map(|s| s.session_id.clone());
                note_limit(st, &acc, h, sid);
            }
            None => {
                if st.limited.get(&acc).is_some_and(|l| l.source == LimitSource::Transcript) {
                    clear_limit(st, &acc, "the session replied again");
                }
            }
        }
    }
}

/// `listApprovals` -> `[{ id, accountId, accountName, sessionId?, sessionTitle?, cwd, toolName,
/// mcpServer?, summary, receivedAt }]`, oldest first.
fn list_approvals(st: &AppState) -> Value {
    let list: Vec<Value> = st
        .approvals
        .iter()
        .map(|a| {
            let mut v = json!({
                "id": a.id,
                "accountId": a.account_id,
                "accountName": a.account_name,
                "cwd": a.cwd,
                "toolName": a.tool_name,
                "summary": a.summary,
                "receivedAt": a.received_at,
            });
            if let Some(o) = v.as_object_mut() {
                if let Some(s) = &a.session_id {
                    o.insert("sessionId".into(), json!(s));
                }
                if let Some(t) = &a.session_title {
                    o.insert("sessionTitle".into(), json!(t));
                }
                if let Some(m) = &a.mcp_server {
                    o.insert("mcpServer".into(), json!(m));
                }
            }
            v
        })
        .collect();
    Value::Array(list)
}

fn gone() -> Value {
    json!({ "ok": false, "error": "gone", "reason": "gone" })
}

/// `decideApproval { id, decision: "allow" | "deny", note? }` -> `{ok:true}`, or `{ok:false,
/// error:"gone"}` when the request was answered in Claude or expired. Resolved by a worker.
fn decide_approval(id: u64, opts: &Value) -> UResult<Option<Value>> {
    let req_id = opts
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("Unknown request.")?
        .to_string();
    let note = match opts.get("note") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if js_len(s) <= 2000 => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        _ => return Err("Notes can be at most 2000 characters.".into()),
    };
    let decision = match opts.get("decision").and_then(Value::as_str) {
        Some("allow") => Decision::Allow,
        Some("deny") => Decision::Deny { message: note },
        _ => return Err("Invalid decision.".into()),
    };
    let taken = state_call_raw(|st| {
        let Some(pos) = st.approvals.iter().position(|a| a.id == req_id) else { return Ok(None) };
        let a = st.approvals.remove(pos);
        let key = match &a.session_id {
            Some(s) => (a.account_id.clone(), s.clone()),
            None => session_key(st, &a.cli_session_id),
        };
        st.waiting.remove(&key);
        approvals_changed(st);
        Ok(Some(a))
    })?;
    let Some(mut a) = taken else { return Ok(Some(gone())) };
    let Some(responder) = a.responder.take() else { return Ok(Some(gone())) };
    let word = if decision == Decision::Allow { "allow" } else { "deny" };
    let tool = log_tool(&a.tool_name);
    let session = a.session_id.clone().unwrap_or_else(|| a.cli_session_id.clone());
    spawn_then(
        move || responder.decide(decision),
        move |r| {
            let v = match r {
                Ok(()) => {
                    log::info(&format!("approval: {word} {tool} in session {session}"));
                    ok()
                }
                Err(e) => {
                    log::info(&format!("approval: {word} {tool} in session {session} not delivered ({e})"));
                    gone()
                }
            };
            with_state(retry_auto_handover);
            resolve(id, v);
        },
    );
    Ok(None)
}

/// `listWaiting` -> `[{ accountId, sessionId, kind: "permission" | "idle" | "needs_input", since }]`.
fn list_waiting(st: &AppState) -> Value {
    let mut list: Vec<(&(String, String), &Waiting)> = st.waiting.iter().collect();
    list.sort_by(|a, b| a.1.since.cmp(&b.1.since).then_with(|| a.0.cmp(b.0)));
    Value::Array(
        list.into_iter()
            .map(|((acc, sid), w)| json!({ "accountId": acc, "sessionId": sid, "kind": w.kind.as_str(), "since": w.since }))
            .collect(),
    )
}

/// `handOverSession { sessionId, fromId?, to?, restart? }`: copy a session to another account
/// that is signed in and not limited, then show it there. `restart` lets the target's open
/// Claude window be restarted when no session is running there. Resolved by a worker.
fn hand_over_session(id: u64, opts: &Value) -> UResult<Option<Value>> {
    if !opts.is_object() {
        return Err("Invalid hand-over request.".into());
    }
    let sid = check_session_id(opts.get("sessionId").unwrap_or(&Value::Null))?;
    let to = match opts.get("to") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if id_ok(s) => Some(s.clone()),
        _ => return Err("Unknown account.".into()),
    };
    let restart = match opts.get("restart") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        _ => return Err("Invalid hand-over request.".into()),
    };
    state_call_raw(|st| {
        let from = match opts.get("fromId").and_then(Value::as_str) {
            Some(f) if id_ok(f) => f.to_string(),
            Some(_) => return Err("Unknown account.".into()),
            None => st
                .index
                .values()
                .find(|s| s.session_id == sid)
                .map(|s| s.account_id.clone())
                .ok_or_else(|| "Unknown session.".to_string())?,
        };
        start_handover(st, &from, &sid, to, id, restart)
    })
}

/// The reset time of the limited account (other than `except`) that resets first.
fn earliest_reset(st: &AppState, except: &str) -> Option<u64> {
    st.limited
        .iter()
        .filter(|(id, _)| id.as_str() != except)
        .map(|(_, l)| {
            l.until.unwrap_or_else(|| {
                l.since
                    + match l.kind {
                        LimitKind::Weekly => limits::FALLBACK_WEEKLY_SECS,
                        LimitKind::Session | LimitKind::Model => limits::FALLBACK_SESSION_SECS,
                    }
            })
        })
        .min()
}

/// Copy session `sid` of account `from_id` to `to` (or the first account in panel order that is
/// signed in, has used the Code tab and is not limited), then show it there: the target's Claude
/// is opened when no window of it runs. `reply_id` 0 is the automatic hand-over: it skips
/// accounts with a request waiting, never closes or restarts a Claude window (the result asks
/// for a restart instead) and tells the panel with `handover-done`. From the panel, `restart`
/// allows the target's open window to be restarted, which happens only when no session is
/// running there at that moment. `Ok(Some(v))` = answered now (no target); `Ok(None)` =
/// `reply_id` is resolved by the worker (0 = no reply).
fn start_handover(
    st: &mut AppState,
    from_id: &str,
    sid: &str,
    to: Option<String>,
    reply_id: u64,
    restart: bool,
) -> UResult<Option<Value>> {
    let from = find_account(st, &json!(from_id))?;
    let target = match to {
        Some(t) => t,
        None => {
            let views: Vec<AccountView> = fresh_accounts(st)
                .iter()
                .map(|a| AccountView { id: a.id.clone(), signed_in: a.signed_in, code_ready: a.code_ready })
                .collect();
            let mut skip: HashSet<String> = st.limited.keys().cloned().collect();
            if reply_id == 0 {
                // Nobody confirms an automatic hand-over: leave accounts with a request waiting alone.
                skip.extend(st.approvals.iter().map(|a| a.account_id.clone()).filter(|id| !id.is_empty()));
            }
            match pick_handover_target(&views, &from.id, &skip) {
                Some(t) => t,
                None => {
                    return Ok(Some(json!({
                        "ok": false,
                        "reason": "no-target",
                        "error": "Every other account is limited",
                        "until": earliest_reset(st, &from.id),
                    })))
                }
            }
        }
    };
    if target == from.id {
        return Err("Pick a different account.".into());
    }
    if st.handovers.contains(sid) {
        return Err("This session is already being handed over.".into());
    }
    let to_acc = find_account(st, &json!(target))?;
    let from_s = session_account(st, &from)?;
    let to_s = session_account(st, &to_acc)?;
    let plan = prepare_open(st, &to_acc)?;
    let window = DesktopWindow { plan, located: st.located.clone(), wake: st.poll_wake.clone() };
    let sessions = sessions_clone(st);
    let sid = sid.to_string();
    let mode = if reply_id == 0 { HandoverMode::Auto } else { HandoverMode::Manual { restart } };
    let auto = mode == HandoverMode::Auto;
    st.handovers.insert(sid.clone());
    let (to_name, to_id, from_name, from_id) =
        (to_acc.name.clone(), to_acc.id.clone(), from.name.clone(), from.id.clone());
    let job_sid = sid.clone();
    spawn_then(
        move || run_handover(&sessions, &from_s, to_s, &job_sid, mode, &window),
        move |out| {
            with_state(|st| {
                st.handovers.remove(&sid);
                request_poll(st);
            });
            let how = if auto { "automatically" } else { "from the panel" };
            let r = &out.result;
            let v = if r.ok {
                log::info(&format!("handover: {sid} handed over to {to_id} {how}"));
                if out.window_left_open {
                    log::info(&format!("handover: {to_id}'s Claude window left open (a session is running there)"));
                }
                let mut v = serde_json::to_value(r).unwrap_or_else(|_| ok());
                if let Some(o) = v.as_object_mut() {
                    o.insert("toName".into(), json!(to_name));
                    o.insert("fromName".into(), json!(from_name));
                    o.insert("opened".into(), json!(out.shown.as_ref().is_some_and(|s| s["ok"] != json!(false))));
                    o.insert("restarted".into(), json!(out.restarted));
                    o.insert("restartNeeded".into(), json!(out.restart_needed));
                    o.insert("windowLeftOpen".into(), json!(out.window_left_open));
                    o.insert("auto".into(), json!(auto));
                    o.insert("toId".into(), json!(to_id));
                    o.insert("fromId".into(), json!(from_id));
                }
                if auto {
                    with_state(|st| emit(st, "handover-done", &v));
                }
                v
            } else {
                log::info(&format!(
                    "handover: {sid} not handed over to {to_id} ({})",
                    r.reason.as_deref().unwrap_or("error")
                ));
                let error = r
                    .error
                    .clone()
                    .filter(|e| !e.is_empty())
                    .unwrap_or_else(|| reason_text(r.reason.as_deref(), &to_name));
                json!({ "ok": false, "reason": r.reason, "error": error })
            };
            resolve(reply_id, v);
        },
    );
    Ok(None)
}

/// Who asked for a hand-over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandoverMode {
    /// The app, for a limited account: never closes a Claude window.
    Auto,
    /// The panel's Hand over dialog; `restart` = the user allowed restarting the target's window.
    Manual { restart: bool },
}

/// The target account's Claude window as a hand-over sees it.
trait TargetWindow {
    /// Pids of the target's running Claude (empty = no window open).
    fn pids(&self) -> Vec<u32>;
    /// Open the target's Claude.
    fn open(&self, focus: bool) -> Value;
    /// Close the target's Claude (`pids`) and open it again focused.
    fn restart(&self, pids: Vec<u32>) -> Value;
}

/// The real Claude Desktop window of a hand-over target.
struct DesktopWindow {
    plan: OpenPlan,
    located: Option<Located>,
    wake: Option<mpsc::Sender<()>>,
}

impl TargetWindow for DesktopWindow {
    fn pids(&self) -> Vec<u32> {
        claude_app::list_processes()
            .into_iter()
            .filter(|p| claude_app::matches_data_dir(p, self.plan.dir.as_deref()))
            .map(|p| p.pid)
            .collect()
    }

    fn open(&self, focus: bool) -> Value {
        open_logged(&self.plan, self.located.clone(), focus, &self.wake)
    }

    fn restart(&self, pids: Vec<u32>) -> Value {
        restart_and_open(&self.plan, pids, self.located.clone(), &self.wake)
    }
}

/// What a hand-over did after the copy.
struct HandoverOutcome {
    result: TransferResult,
    /// Result of opening or restarting the target's window, when that was done.
    shown: Option<Value>,
    restarted: bool,
    /// The copy shows up only after the target's open window restarts.
    restart_needed: bool,
    /// A restart was allowed but a session is running in the target's window, so it stays open.
    window_left_open: bool,
}

/// Worker: copy the session, then make it visible in the target's Claude. A window that is not
/// open is opened (focused only from the panel). An open window is restarted only from the
/// panel with `restart` allowed and no live session of the target at that moment; otherwise it
/// is left alone and the outcome asks for a restart.
fn run_handover(
    sessions: &Sessions,
    from_s: &SessionAccount,
    mut to_s: SessionAccount,
    sid: &str,
    mode: HandoverMode,
    window: &dyn TargetWindow,
) -> HandoverOutcome {
    let pids = window.pids();
    to_s.running = !pids.is_empty();
    let result = sessions.transfer(from_s, &to_s, sid, "copy");
    let mut out =
        HandoverOutcome { result, shown: None, restarted: false, restart_needed: false, window_left_open: false };
    if !out.result.ok {
        return out;
    }
    // A window that failed to open or restart still has to be (re)started by hand.
    let worked = |shown: &Value| shown["ok"] != json!(false);
    if pids.is_empty() {
        let shown = window.open(mode != HandoverMode::Auto);
        out.restart_needed = !worked(&shown);
        out.shown = Some(shown);
        return out;
    }
    // Claude lists sessions only when it starts.
    match mode {
        HandoverMode::Manual { restart: true } if sessions.live_sessions_in(&to_s) == 0 => {
            let shown = window.restart(pids);
            out.restarted = worked(&shown);
            out.restart_needed = !out.restarted;
            out.shown = Some(shown);
        }
        HandoverMode::Manual { restart: true } => {
            out.restart_needed = true;
            out.window_left_open = true;
        }
        HandoverMode::Manual { restart: false } | HandoverMode::Auto => out.restart_needed = true,
    }
    out
}

/// Worker: close the target's Claude (when `pids` run) and open it again focused. Only a
/// hand-over from the panel with a restart allowed and no live session gets here with pids.
fn restart_and_open(
    plan: &OpenPlan,
    pids: Vec<u32>,
    located: Option<Located>,
    wake: &Option<mpsc::Sender<()>>,
) -> Value {
    if !pids.is_empty() {
        if is_smoke() {
            log::info("smoke: blocked claude_app::kill");
            return ok();
        }
        if let Err(e) = <Os as Closer>::close(&pids) {
            log::warn(&format!("handover: closing {}: {e}", plan.id));
            return fail(&format!("Could not restart {}'s Claude window.", plan.name));
        }
        let deadline = Instant::now() + RESTART_WAIT;
        while Instant::now() < deadline
            && claude_app::list_processes().iter().any(|p| claude_app::matches_data_dir(p, plan.dir.as_deref()))
        {
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    open_logged(plan, located, true, wake)
}

/// A hand-over's open of its target, logged like an open from the panel.
fn open_logged(plan: &OpenPlan, located: Option<Located>, focus: bool, wake: &Option<mpsc::Sender<()>>) -> Value {
    let mut note = String::new();
    let v = open_worker(plan, located, focus, wake, &mut note);
    log::info(&open_log_line(&plan.id, OpenFrom::Handover, &note));
    v
}

/// Hook changes are allowed outside smoke mode, and in smoke mode only on the demo fixture
/// (whose home folder is a throw-away copy).
fn hooks_allowed() -> bool {
    !is_smoke() || SMOKE_FIXTURE.load(Ordering::Relaxed)
}

/// `getHooksStatus` -> `{ installed, disabledAll, managedOnly, backupExists }` for the user's
/// Claude Code settings file.
fn hooks_status(_st: &AppState) -> Value {
    let s = match std::env::current_exe() {
        Ok(exe) if hooks_allowed() => hooks::status(&hooks::user_settings_path(), &exe),
        _ => hooks::HookStatus::default(),
    };
    json!({
        "installed": s.installed,
        "disabledAll": s.disabled_all,
        "managedOnly": s.managed_only,
        "backupExists": s.backup_exists,
    })
}

/// `previewHooks` -> `{ path, json }`: the file and the exact entries `installHooks` adds.
fn hooks_preview() -> Value {
    match std::env::current_exe() {
        Ok(exe) => json!({
            "path": hooks::user_settings_path().to_string_lossy(),
            "json": hooks::preview_json(&hooks::plan(&exe)),
        }),
        Err(e) => fail(&format!("The location of {} is unknown: {e}", brand::NAME)),
    }
}

/// Run a hook change on a worker and resolve `id` with its result.
fn hooks_job(id: u64, work: impl FnOnce(&Path, &Path) -> Value + Send + 'static) -> UResult<Option<Value>> {
    if !hooks_allowed() {
        return Err("Claude Code's settings are not changed during the smoke check.".into());
    }
    let exe = std::env::current_exe().map_err(|e| format!("The location of {} is unknown: {e}", brand::NAME))?;
    state_call_raw(|_| Ok(()))?;
    spawn_then(move || work(&hooks::user_settings_path(), &exe), move |v| resolve(id, v));
    Ok(None)
}

/// `installHooks`: back up the user's Claude Code settings and add our entries. Resolved by a
/// worker.
fn install_hooks(id: u64) -> UResult<Option<Value>> {
    hooks_job(id, |path, exe| match hooks::install(path, &hooks::plan(exe)) {
        Ok(r) => {
            log::info(&format!("hooks: added {} entries (backup written: {})", r.added, r.backup_written));
            json!({ "ok": true, "added": r.added, "backupWritten": r.backup_written })
        }
        Err(e) => {
            log::warn(&format!("hooks: install: {e}"));
            fail(&e.to_string())
        }
    })
}

/// `removeHooks`: take exactly our entries out of the user's Claude Code settings. Resolved by a
/// worker.
fn remove_hooks(id: u64) -> UResult<Option<Value>> {
    hooks_job(id, |path, exe| match hooks::remove(path, exe) {
        Ok(r) => {
            log::info(&format!("hooks: removed {} entries", r.removed));
            json!({ "ok": true, "removed": r.removed, "written": r.written })
        }
        Err(e) => {
            log::warn(&format!("hooks: remove: {e}"));
            fail(&e.to_string())
        }
    })
}

/// `removeChanges`: take out everything Sessionmux changed outside its own folder, so it can be
/// removed without an uninstaller: a sign-in in progress ends, the `claude://` handler goes back
/// to Claude, the login item is turned off and our Claude Code hooks are removed. Resolved by a
/// worker with `{ ok, removed, written, linkHandler }` (`linkHandler` as for
/// `restoreLinkHandler`, or "failed").
fn remove_changes(id: u64) -> UResult<Option<Value>> {
    if !hooks_allowed() {
        return Err("Claude Code's settings are not changed during the smoke check.".into());
    }
    state_call_raw(|st| {
        if st.relay.state().is_some_and(|s| s.active) {
            end_sign_in(st, "cancelled");
        }
        if st.settings.start_with_windows {
            update_settings(st, json!({ "startWithWindows": false }));
        } else {
            apply_login_item(false);
        }
        // Queued after the cancel above, so the relay has ended when the links are given back.
        let work = |rl: &Relay| {
            give_links_back(rl).unwrap_or_else(|e| {
                log::warn(&format!("relay restore: {e}"));
                "failed"
            })
        };
        relay_job(st, work, move |st, link| {
            watch_restore(st);
            push_sign_in(st, None);
            log::info(&format!("remove changes: sign-in links {link}, login item off"));
            post(move || {
                if let Err(e) = remove_hooks_after(id, link) {
                    resolve(id, json!({ "ok": false, "error": e, "linkHandler": link }));
                }
            });
        });
        Ok(())
    })?;
    Ok(None)
}

/// The hooks part of `removeChanges`, once the link handler is settled (`link`).
fn remove_hooks_after(id: u64, link: &'static str) -> UResult<Option<Value>> {
    hooks_job(id, move |path, exe| match hooks::remove(path, exe) {
        Ok(r) => {
            log::info(&format!("hooks: removed {} entries", r.removed));
            json!({ "ok": true, "removed": r.removed, "written": r.written, "linkHandler": link })
        }
        Err(e) => {
            log::warn(&format!("hooks: remove: {e}"));
            json!({ "ok": false, "error": human_error(&e.to_string()), "linkHandler": link })
        }
    })
}

/// `openSystemSettings(page)`: the system settings page where the user grants what `page`
/// ("loginItems", "automation" or "accessibility") needs.
fn open_system_settings(page: &Value) -> UResult<Value> {
    let pane = match page.as_str() {
        Some("loginItems") => None,
        Some("automation") => Some(PrivacyPane::Automation),
        Some("accessibility") => Some(PrivacyPane::Accessibility),
        _ => return Err("Unknown settings page.".into()),
    };
    if is_smoke() {
        log::info("smoke: blocked openSystemSettings");
        return Ok(ok());
    }
    match pane {
        None => <Os as Autostart>::open_settings(),
        Some(p) => <Os as Shell>::open_privacy_settings(p)?,
    }
    Ok(ok())
}

// ------------------------------------------------------------------ lifecycle

/// Set up the event loop, open the store and build the UI. Call once on the UI thread.
pub fn init(hidden: bool) -> Result<(), String> {
    let smoke = is_smoke();
    <Os as EventLoop>::init(on_event)?;
    remove_stale_state(&paths::user_data());
    let store = AccountsStore::open(&paths::user_data(), &paths::app_data());
    // In smoke mode or with a user-data override, the relay uses relay.json only and never touches
    // the claude:// handler.
    let relay = if smoke || paths::user_data_overridden() {
        Relay::open_with(&paths::user_data(), None)
    } else {
        Relay::open(&paths::user_data())
    };
    let located = match claude_app::locate() {
        Ok(l) => Some(l),
        Err(e) => {
            log::warn(&format!("Claude Desktop not located at startup: {e}"));
            None
        }
    };
    let settings = store.settings();
    let procs = claude_app::list_processes();
    let fg_pid = <Os as Windows>::foreground_pid();

    let tray = if smoke {
        None
    } else {
        match <Tray as TrayHost>::create() {
            Ok(t) => Some(Rc::new(RefCell::new(t))),
            Err(e) => {
                log::warn(&format!("tray: {e}"));
                None
            }
        }
    };
    if !smoke {
        if let Err(e) = <Os as HotkeyHost>::register(HOTKEY_ID, &settings.hotkey) {
            log::warn(&format!("hotkey {}: {e}", settings.hotkey));
        }
        if is_packaged() {
            apply_login_item(settings.start_with_windows); // keep the exe path current
        }
    }

    let state = AppState {
        store,
        relay,
        located,
        sessions: Sessions::new(),
        bubble: None,
        panel: None,
        tray,
        last_accounts: Vec::new(),
        last_active: None,
        pinned_until: None,
        settings: settings.clone(),
        procs,
        fg_pid,
        last_opened: None,
        last_accounts_json: String::new(),
        last_discovered: Vec::new(),
        last_discovered_json: None,
        last_settings_json: String::new(),
        last_procs_log: String::new(),
        last_active_json: String::new(),
        last_bubble_state: None,
        last_blur_hide: None,
        panel_creating: false,
        tray_dirty: false,
        poll_wake: None,
        page_errors: Vec::new(),
        relay_seen_running: false,
        relay_started: None,
        relay_last_error: None,
        relay_jobs: None,
        relay_busy: 0,
        relay_forward_failed: false,
        relay_automation_denied: false,
        started: Instant::now(),
        tamper_limit: gesture::Limiter::default(),
        debugger_limit: gesture::Limiter::default(),
        approvals: Vec::new(),
        waiting: HashMap::new(),
        limited: HashMap::new(),
        pipe: None,
        index: HashMap::new(),
        index_busy: false,
        transcripts: HashMap::new(),
        unmapped_limits: Vec::new(),
        handover_marks: HashMap::new(),
        handovers: HashSet::new(),
    };
    APP.with(|a| *a.borrow_mut() = Some(Rc::new(RefCell::new(state))));

    let no_surface = with_state(|st| {
        st.last_settings_json = panel_settings(st).to_string();
        refresh(st);
        if st.settings.show_bubble || smoke {
            create_bubble(st);
        }
        update_tray(st);
        if !smoke {
            init_sign_in(st);
            st.poll_wake = start_poll_thread();
            st.pipe = start_pipe();
        }
        set_timer(TimerId::StartupTrim, STARTUP_TRIM_MS);
        st.tray.is_none() && st.bubble.is_none()
    })
    .unwrap_or(true);

    if no_surface && !smoke {
        // Still reachable through the hotkey and a second launch (single-instance hand-off),
        // but say so: without bubble and tray the app is easy to lose.
        log::warn("no bubble and no tray icon; reachable through the hotkey only");
    }
    log::info(&format!("{} {} started{}", brand::NAME, brand::VERSION, if hidden { " (hidden)" } else { "" }));
    if !hidden {
        show_panel();
    }
    Ok(())
}

/// Request exit: the event loop ends and main.rs calls `shutdown`.
pub fn quit() {
    QUITTING.store(true, Ordering::Relaxed);
    with_state(end_sign_in_at_quit);
    <Os as EventLoop>::quit();
}

/// Release UI resources (idempotent): poll thread, timers, hotkey, tray, panel, bubble, event
/// loop.
pub fn shutdown() {
    QUITTING.store(true, Ordering::Relaxed);
    let Some(app) = APP.with(|a| a.borrow_mut().take()) else { return };
    let Ok(cell) = Rc::try_unwrap(app) else {
        log::warn("shutdown: app state still referenced");
        return;
    };
    let mut st = cell.into_inner();
    <Os as GestureHost>::close();
    st.poll_wake.take(); // disconnects the poll thread's channel
    for id in [TimerId::DestroyPanel, TimerId::Pin, TimerId::StartupTrim, TimerId::SignIn] {
        kill_timer(id);
    }
    // Pending approvals are told nothing: each helper exits silently and the prompt in Claude decides.
    st.approvals.clear();
    if let Some(p) = st.pipe.take() {
        p.stop();
    }
    SMOKE_FIXTURE.store(false, Ordering::Relaxed);
    // Never leave Sessionmux registered as the claude:// handler after it exits: given back now
    // where that asks nobody, else at the next start.
    end_relay_at_exit(&mut st, "shutdown");
    st.relay_jobs.take(); // ends the relay worker once its queue is empty
    if !is_smoke() {
        <Os as HotkeyHost>::unregister(HOTKEY_ID);
    }
    if let Some(p) = st.panel.take() {
        p.destroy();
    }
    drop(st.tray.take());
    drop(st.bubble.take());
    <Os as EventLoop>::shutdown();
    log::info("shutdown complete");
}

// ------------------------------------------------------------------ smoke helpers

/// (accounts, discovered, locate (kind, exe), procs) — fresh, read-only.
pub type SmokeCounts = (usize, usize, Option<(String, String)>, usize);

/// Reads [`SmokeCounts`] from the live state.
pub fn smoke_counts() -> Option<SmokeCounts> {
    with_state(|st| {
        st.procs = claude_app::list_processes();
        if st.located.is_none() {
            st.located = claude_app::locate().ok();
        }
        let accounts = fresh_accounts(st).len();
        let discovered = fresh_discovered(st).len();
        let loc = st.located.as_ref().map(|l| (l.kind.to_string(), l.exe.clone()));
        refresh(st);
        (accounts, discovered, loc, st.procs.len())
    })
}

/// Apply a settings patch exactly as the panel would (validated).
pub fn smoke_set_settings(patch: Value) -> Result<(), String> {
    let patch = clean_settings_patch(&patch)?;
    with_state(|st| {
        update_settings(st, patch);
    })
    .ok_or_else(|| "app state busy".to_string())
}

pub fn smoke_bubble_mode() -> String {
    with_state(|st| bubble_mode_str(st).to_string()).unwrap_or_default()
}

/// Bubble window rect and its display's scale.
pub fn smoke_bubble_rect() -> Option<(Rect, f32)> {
    let r: Rect = with_state(|st| st.bubble.as_ref().map(|b| b.rect())).flatten()?;
    Some((r, <Os as Desktop>::units_per_dip(&r)))
}

/// Create the menu bar icon with the current menu and register the shortcut, then remove both
/// again (the self-check keeps neither). Main thread only. What was checked, or why it failed.
#[cfg(target_os = "macos")]
pub fn smoke_menu_bar_and_shortcut() -> Result<Value, String> {
    let (items, mode, hotkey) = with_state(|st| {
        let items: Vec<(String, String, bool)> =
            st.last_accounts.iter().map(|a| (a.id.clone(), a.name.clone(), a.running)).collect();
        (items, bubble_mode_str(st).to_string(), st.settings.hotkey.clone())
    })
    .ok_or("app state busy")?;
    let mut tray = <Tray as TrayHost>::create().map_err(|e| format!("tray: {e}"))?;
    tray.set_menu(&TrayMenu::new(&items, true, &mode));
    tray.set_waiting(1);
    tray.set_waiting(0);
    let anchored = tray.anchor().is_some();
    drop(tray);
    log::info(&format!("smoke: tray shown ({} accounts, anchored {anchored})", items.len()));
    <Os as HotkeyHost>::register(HOTKEY_ID, &hotkey).map_err(|e| format!("hotkey {hotkey}: {e}"))?;
    <Os as HotkeyHost>::unregister(HOTKEY_ID);
    log::info(&format!("smoke: hotkey {hotkey} registered"));
    Ok(json!({ "tray": true, "trayAnchored": anchored, "hotkey": hotkey }))
}

/// Number of accounts the mini tray shows.
pub fn smoke_tray_accounts() -> usize {
    with_state(|st| st.last_accounts.len().clamp(1, TRAY_MAX_ACCOUNTS)).unwrap_or(1)
}

pub fn smoke_bubble_png(path: &Path) -> Result<(), String> {
    with_state(|st| match st.bubble.as_ref() {
        Some(b) => b.render_to_png(path),
        None => Err("no bubble".into()),
    })
    .unwrap_or_else(|| Err("app state busy".into()))
}

/// Create the panel and show it without activation off-screen.
pub fn smoke_show_panel_offscreen() -> Result<(), String> {
    if !ensure_panel(false) {
        return Err("panel could not be created".into());
    }
    with_state(|st| {
        kill_timer(TimerId::DestroyPanel);
        if let Some(p) = st.panel.as_ref() {
            p.show_inactive_at(-10_000, 100);
        }
    });
    Ok(())
}

pub fn smoke_panel_ready() -> bool {
    with_state(|st| st.panel.as_ref().is_some_and(|p| p.is_ready())).unwrap_or(false)
}

pub fn smoke_panel_window() -> Option<WindowId> {
    with_state(|st| st.panel.as_ref().map(|p| p.window())).flatten()
}

pub fn smoke_show_tab(tab: &str) {
    with_state(|st| emit(st, "show-tab", &json!(tab)));
}

/// Hide and destroy the panel now (what the 30 s timer does).
pub fn smoke_destroy_panel() {
    let p = with_state(|st| {
        hide_panel_in(st, false);
        kill_timer(TimerId::DestroyPanel);
        st.panel.take()
    })
    .flatten();
    if let Some(p) = p {
        p.destroy();
        trim_working_set();
    }
}

pub fn smoke_page_errors() -> Vec<String> {
    with_state(|st| st.page_errors.clone()).unwrap_or_default()
}

pub fn smoke_tabs() -> [&'static str; 3] {
    TABS
}

/// `getSignIn` of the running app (smoke: always `{active:false}`, a relay never begins).
pub fn smoke_sign_in() -> Value {
    with_state(|st| sign_in_payload(st)).unwrap_or(Value::Null)
}

/// Push a synthetic `signin-changed` payload to the page (page check only; no relay, no
/// handler change). `None` re-sends the real state.
pub fn smoke_emit_sign_in(fake: Option<Value>) {
    with_state(|st| match fake {
        None => push_sign_in(st, None),
        Some(v) => emit(st, "signin-changed", &v),
    });
}

/// One Code session of the demo fixture: (account id, record id, CLI session id, title).
pub type SmokeSession<'a> = (&'a str, &'a str, &'a str, &'a str);

/// Demo fixture (`SESSIONMUX_SMOKE_FIXTURE`): index the demo sessions, mark the first one as
/// waiting for a permission and `limited_account` as limited. No pipe, no hooks, no settings
/// file is written.
pub fn smoke_seed_feature_fixture(sessions: &[SmokeSession], limited_account: &str) {
    SMOKE_FIXTURE.store(true, Ordering::Relaxed);
    with_state(|st| {
        let now = relay::now_secs();
        for (i, (acc, sid, cli, title)) in sessions.iter().enumerate() {
            st.index.insert(
                cli.to_string(),
                IndexedSession {
                    account_id: acc.to_string(),
                    session_id: sid.to_string(),
                    title: title.to_string(),
                    running: false,
                    last_activity_at: now.saturating_sub(i as u64 * 60),
                },
            );
        }
        if let Some((acc, sid, _, _)) = sessions.first() {
            st.waiting
                .insert((acc.to_string(), sid.to_string()), Waiting { kind: WaitingKind::Permission, since: now - 40 });
        }
        if let Some(hit) = limits::classify("You've hit your session limit · resets 3:45pm") {
            st.limited.insert(
                limited_account.to_string(),
                Limited::from_hit(&LimitHit { source: LimitSource::Transcript, ..hit }, now),
            );
        }
        refresh(st);
        approvals_changed(st);
    });
}

/// Demo fixture: two approve requests (with no helper behind them) on the first two sessions.
pub fn smoke_seed_approvals(sessions: &[SmokeSession]) {
    with_state(|st| {
        let now = relay::now_secs();
        let demo = [
            ("Bash", None, r"C:\Projects\landing-page", "npm run build && npm test -- --watch=false"),
            (
                "Edit",
                None,
                r"C:\Projects\quarterly-report",
                "C:\\Projects\\quarterly-report\\report.md\nold: 212 characters, new: 348 characters",
            ),
        ];
        for (i, ((acc, sid, cli, title), (tool, mcp, cwd, summary))) in sessions.iter().zip(demo).enumerate() {
            st.approvals.push(PendingApproval {
                id: format!("{:032x}", 0xa11ce_u128 + i as u128),
                account_id: acc.to_string(),
                account_name: account_name(st, acc),
                session_id: Some(sid.to_string()),
                cli_session_id: cli.to_string(),
                session_title: Some(title.to_string()),
                cwd: cwd.to_string(),
                tool_name: tool.to_string(),
                mcp_server: mcp.map(str::to_string),
                summary: summary.to_string(),
                received_at: now.saturating_sub(20 - i as u64 * 10),
                responder: None,
            });
        }
        approvals_changed(st);
    });
}

/// Number of approve requests the app holds (smoke).
pub fn smoke_approvals() -> usize {
    with_state(|st| st.approvals.len()).unwrap_or(0)
}

/// (id, name) of the first extra account, for the synthetic sign-in notice.
pub fn smoke_extra_account() -> Option<(String, String)> {
    with_state(|st| st.last_accounts.iter().find(|a| !a.is_main).map(|a| (a.id.clone(), a.name.clone()))).flatten()
}

// ------------------------------------------------------------------ focus check helpers

/// The focus check's view of account `id`: its name, the data folder its Claude runs with and
/// its slot in the floating button's mini tray (with the number of slots shown).
#[cfg(target_os = "macos")]
pub fn check_account(id: &str) -> Option<(String, Option<String>, Option<usize>, usize)> {
    with_state(|st| {
        let acc = st.last_accounts.iter().find(|a| a.id == id)?;
        let shown = st.last_accounts.len().clamp(1, TRAY_MAX_ACCOUNTS);
        let slot = st.last_accounts.iter().take(TRAY_MAX_ACCOUNTS).position(|a| a.id == id);
        Some((acc.name.clone(), proc_dir(acc), slot, shown))
    })
    .flatten()
}

/// Whether account `id` is listed as running.
#[cfg(target_os = "macos")]
pub fn check_running(id: &str) -> bool {
    with_state(|st| st.last_accounts.iter().any(|a| a.id == id && a.running)).unwrap_or(false)
}

/// Show the panel on its Accounts tab, as the floating button does.
#[cfg(target_os = "macos")]
pub fn check_show_accounts() {
    show_tab("accounts");
}

/// Hide the panel, if shown.
#[cfg(target_os = "macos")]
pub fn check_hide_panel() {
    hide_panel(false);
}

/// The panel's window, whether it is visible and whether its page is ready.
#[cfg(target_os = "macos")]
pub fn check_panel() -> Option<(WindowId, bool, bool)> {
    with_state(|st| st.panel.as_ref().map(|p| (p.window(), p.is_visible(), p.is_ready()))).flatten()
}

/// Run `js` in the panel's page; `done` receives the result as JSON text. False without a panel.
#[cfg(target_os = "macos")]
pub fn check_panel_eval(js: &str, done: impl Fn(String) + Send + 'static) -> bool {
    with_state(|st| st.panel.as_ref().map(|p| p.eval_with_result(js, done)).unwrap_or(false)).unwrap_or(false)
}

/// Open account `id` from posted work, with no input event behind it.
#[cfg(target_os = "macos")]
pub fn check_open(id: &str) -> Result<(), String> {
    open_account(0, &Value::String(id.to_string()), true, OpenFrom::Check)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_patch_rules() {
        assert_eq!(clean_account_patch(&json!([]), false), Err("Invalid account details.".into()));
        assert_eq!(clean_account_patch(&json!({}), true), Err("Please enter a name.".into()));
        assert_eq!(clean_account_patch(&json!({ "name": null }), false), Err("Please enter a name.".into()));
        assert_eq!(clean_account_patch(&json!({ "name": "   " }), true), Err("Please enter a name.".into()));
        assert_eq!(
            clean_account_patch(&json!({ "name": "x".repeat(33) }), true),
            Err("Names can be at most 32 characters.".into())
        );
        assert_eq!(clean_account_patch(&json!({ "color": "red" }), false), Err("Invalid colour.".into()));
        assert_eq!(
            clean_account_patch(&json!({ "label": 5 }), false),
            Err("Label must be at most 128 characters.".into())
        );
        let p = clean_account_patch(&json!({ "name": " Work ", "color": "#abcdef", "label": "  " }), true).unwrap();
        assert_eq!(p, AccountPatch { name: Some("Work".into()), color: Some("#ABCDEF".into()), label: Some(None) });
        let p = clean_account_patch(&json!({ "label": null }), false).unwrap();
        assert_eq!(p.label, Some(None));
        assert!(clean_account_patch(&json!({}), false).unwrap().is_empty());
    }

    #[test]
    fn settings_patch_rules() {
        assert!(clean_settings_patch(&json!(null)).is_err());
        assert_eq!(clean_settings_patch(&json!({ "showBubble": "yes" })), Err("Invalid value for showBubble.".into()));
        assert_eq!(clean_settings_patch(&json!({ "theme": "neon" })), Err("Invalid theme.".into()));
        assert_eq!(clean_settings_patch(&json!({ "panelSide": "top" })), Err("Invalid panel side.".into()));
        assert_eq!(clean_settings_patch(&json!({ "bubbleMode": "x" })), Err("Invalid floating button mode.".into()));
        assert_eq!(
            clean_settings_patch(&json!({ "bubblePosition": { "x": 1 } })),
            Err("Invalid bubble position.".into())
        );
        let p = clean_settings_patch(
            &json!({ "hotkey": "Alt+X", "theme": "light", "bubblePosition": { "x": 1.6, "y": -2.2 } }),
        )
        .unwrap();
        assert_eq!(p, json!({ "theme": "light", "bubblePosition": { "x": 2, "y": -2 } }));
        assert_eq!(
            clean_settings_patch(&json!({ "bubblePosition": null })).unwrap(),
            json!({ "bubblePosition": null })
        );
        assert_eq!(
            clean_settings_patch(&json!({ "signInRouting": 1 })),
            Err("Invalid value for signInRouting.".into())
        );
        assert_eq!(
            clean_settings_patch(&json!({ "signInRouting": false })).unwrap(),
            json!({ "signInRouting": false })
        );
        assert_eq!(
            clean_settings_patch(&json!({ "autoHandover": "on" })),
            Err("Invalid value for autoHandover.".into())
        );
        assert_eq!(
            clean_settings_patch(&json!({ "autoHandover": true, "approveFromPanel": false })).unwrap(),
            json!({ "autoHandover": true, "approveFromPanel": false })
        );
        assert_eq!(
            clean_settings_patch(&json!({ "raiseWindowsWithAccessibility": true })).unwrap(),
            json!({ "raiseWindowsWithAccessibility": true })
        );
        assert!(clean_settings_patch(&json!({ "raiseWindowsWithAccessibility": "on" })).is_err());
        // A key older builds accepted is dropped like any unknown key, whatever its value.
        let retired = concat!("tamper", "Gesture");
        for v in [json!(false), json!("off")] {
            let mut patch = json!({ "showBubble": true });
            patch[retired] = v;
            assert_eq!(clean_settings_patch(&patch).unwrap(), json!({ "showBubble": true }));
        }
    }

    #[test]
    fn profile_signed_in_rule() {
        let d = std::env::temp_dir().join(format!("sessionmux-signin-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let dir = d.to_string_lossy().into_owned();
        assert!(!profile_signed_in(&dir)); // no config.json
        for (cfg, want) in [
            (json!({}), false),
            (json!({ "oauth:tokenCache": null }), false),
            (json!({ "oauth:tokenCache": "" }), false),
            (json!({ "oauth:tokenCache": "enc:abc" }), true),
            (json!({ "oauth:tokenCache": { "a": 1 } }), true),
        ] {
            std::fs::write(d.join("config.json"), cfg.to_string()).unwrap();
            assert_eq!(profile_signed_in(&dir), want, "{cfg}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ids_colors_errors() {
        assert!(id_ok("main") && id_ok("3f2a-b_c") && !id_ok("") && !id_ok("a b") && !id_ok(&"a".repeat(65)));
        assert!(color_ok("#A1b2C3") && !color_ok("#A1B2C") && !color_ok("A1B2C3D"));
        assert_eq!(parse_color("#8A7CFF"), 0x8A7CFF);
        assert_eq!(parse_color("bogus"), 0x8A7CFF);
        assert_eq!(human_error(&"x".repeat(400)).len(), 300);
        assert_eq!(
            reason_text(Some("target-never-opened-code"), "Work"),
            "Work has never opened the Code tab. Open it there once, then try again."
        );
        assert_eq!(reason_text(None, "W"), "The session could not be transferred.");
    }

    #[test]
    fn system_settings_pages() {
        for page in [json!("nope"), json!(""), json!(null), json!(1), json!("LoginItems")] {
            assert!(open_system_settings(&page).is_err(), "{page}");
        }
    }

    #[test]
    fn app_info_shape() {
        let a = app_info();
        for k in [
            "productName",
            "version",
            "buildDate",
            "tagline",
            "homeUrl",
            "supportUrl",
            "releasesUrl",
            "sponsorUrl",
            "licenseName",
            "disclaimer",
            "platform",
            "translocated",
        ] {
            assert!(a.get(k).is_some(), "getAppInfo lacks {k}");
        }
        assert_eq!(a.as_object().map(|o| o.len()), Some(12));
        assert_eq!(a["platform"], platform::OS_NAME);
        assert_eq!(a["productName"], "Sessionmux");
        assert_eq!(a["licenseName"], "CPAL-1.0");
        assert_eq!(a["homeUrl"], brand::REPO_URL);
        assert_eq!(a["supportUrl"], format!("{}/issues", brand::REPO_URL));
        assert_eq!(a["releasesUrl"], format!("{}/releases", brand::REPO_URL));
        assert_eq!(a["buildDate"].as_str().map(str::len), Some(10));
    }

    #[test]
    fn cargo_builds_are_not_packaged() {
        let t = tempfile::tempdir().unwrap();
        for target in ["target", "sessionmux-target"] {
            let rel = t.path().join(target).join("release");
            std::fs::create_dir_all(rel.join("deps")).unwrap();
            assert!(is_cargo_build(&rel.join("Sessionmux.exe")), "{target}");
        }
        let installed = t.path().join("Programs").join("Sessionmux");
        std::fs::create_dir_all(&installed).unwrap();
        assert!(!is_cargo_build(&installed.join("Sessionmux.exe")));
        let release_no_cargo = t.path().join("Apps").join("release");
        std::fs::create_dir_all(&release_no_cargo).unwrap();
        assert!(!is_cargo_build(&release_no_cargo.join("Sessionmux.exe")));
    }

    #[test]
    fn notify_events_map_to_effects() {
        let n = |kind: &str, msg: &str| notify_effect("Notification", Some(kind), msg);
        assert_eq!(n("permission_prompt", "Claude needs your permission"), NotifyEffect::Wait(WaitingKind::Permission));
        assert_eq!(n("idle_prompt", ""), NotifyEffect::Wait(WaitingKind::Idle));
        assert_eq!(n("agent_needs_input", ""), NotifyEffect::Wait(WaitingKind::NeedsInput));
        assert_eq!(n("agent_completed", ""), NotifyEffect::Completed);
        assert_eq!(n("quota_auto_resume_fired", ""), NotifyEffect::LimitCleared);
        for kind in ["quota_auto_resume_stale", "quota_auto_resume_disabled"] {
            let NotifyEffect::Limit(hit) = n(kind, "Usage limit reached") else { panic!("{kind}") };
            assert_eq!((hit.kind, hit.until, hit.source), (LimitKind::Session, None, LimitSource::Notification));
            let NotifyEffect::Limit(hit) = n(kind, "") else { panic!("{kind} without text") };
            assert_eq!(hit.source, LimitSource::Notification);
        }
        assert_eq!(n("auth_success", ""), NotifyEffect::Nothing);
        assert_eq!(notify_effect("Notification", None, ""), NotifyEffect::Nothing);
        let stop = |msg: &str| notify_effect("StopFailure", Some("rate_limit"), msg);
        let NotifyEffect::Limit(hit) = stop("You've hit your weekly limit · resets Mon 12:00am") else { panic!() };
        assert_eq!((hit.kind, hit.source), (LimitKind::Weekly, LimitSource::HookStopFailure));
        assert_eq!(hit.reset_text.as_deref(), Some("Mon 12:00am"));
        assert_eq!(stop("API Error: Rate limit reached"), NotifyEffect::Nothing);
        assert_eq!(
            stop("API Error: Server is temporarily limiting requests (not your usage limit) · Rate limited"),
            NotifyEffect::Nothing
        );
        assert_eq!(
            notify_effect("StopFailure", Some("server_error"), "You've hit your session limit"),
            NotifyEffect::Nothing
        );
        assert_eq!(notify_effect("Stop", Some("rate_limit"), "You've hit your session limit"), NotifyEffect::Nothing);
    }

    #[test]
    fn auto_handover_once_per_limit_window() {
        let now = 1_000_000;
        assert!(handover_due(None, Some(now + 600), now));
        assert!(handover_due(None, None, now));
        // Known reset time: once per window, again only for a later window after this one ended.
        let mark = HandoverMark { until: Some(now + 600), at: now };
        assert!(!handover_due(Some(&mark), Some(now + 600), now + 10));
        assert!(!handover_due(Some(&mark), Some(now + 600), now + 2 * HANDOVER_QUIET_SECS));
        assert!(!handover_due(Some(&mark), Some(now + 900), now + 300));
        assert!(handover_due(Some(&mark), Some(now + 20_000), now + 601));
        // Unknown reset time (on either side): at most once per 30 min.
        let mark = HandoverMark { until: None, at: now };
        assert!(!handover_due(Some(&mark), None, now + HANDOVER_QUIET_SECS - 1));
        assert!(!handover_due(Some(&mark), Some(now + 600), now + 60));
        assert!(handover_due(Some(&mark), None, now + HANDOVER_QUIET_SECS));
        let mark = HandoverMark { until: Some(now + 600), at: now };
        assert!(!handover_due(Some(&mark), None, now + 60));
        assert!(handover_due(Some(&mark), None, now + HANDOVER_QUIET_SECS));
    }

    #[test]
    fn transcript_hits_count_while_in_force() {
        let now = 2_000_000;
        let hit =
            |until| LimitHit { kind: LimitKind::Session, reset_text: None, until, source: LimitSource::Transcript };
        assert!(fresh_transcript_hit(&hit(None), now - 60, now));
        assert!(fresh_transcript_hit(&hit(Some(now + 1)), now - 60, now));
        assert!(!fresh_transcript_hit(&hit(Some(now)), now - 60, now));
        assert!(!fresh_transcript_hit(&hit(None), now - limits::FALLBACK_SESSION_SECS - 1, now));
    }

    #[test]
    fn log_tool_names_are_plain() {
        assert_eq!(log_tool("Bash"), "Bash");
        assert_eq!(log_tool("mcp__srv__do-it.v2"), "mcp__srv__do-it.v2");
        assert_eq!(log_tool("rm -rf /\n"), "rm-rf");
        assert_eq!(log_tool(""), "tool");
        assert_eq!(log_tool(&"x".repeat(100)).len(), 64);
        assert!(helper_session_ok("2c8d3e5f-6071-4b82-9ca3-b4c5d6e7f809"));
        assert!(!helper_session_ok("") && !helper_session_ok("a b") && !helper_session_ok(&"a".repeat(129)));
    }

    #[test]
    fn index_maps_sessions_and_scans_running_transcripts() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let data = t.path().join("data");
        let cwd = t.path().join("project");
        let (uuid, org, cli, sid) =
            ("u-1", "o-1", "11111111-2222-4333-8444-555555555555", "local_aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee");
        let records = data.join("claude-code-sessions").join(uuid).join(org);
        std::fs::create_dir_all(&records).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let cwd_s = cwd.to_string_lossy().into_owned();
        std::fs::write(
            records.join(format!("{sid}.json")),
            json!({ "sessionId": sid, "cliSessionId": cli, "cwd": cwd_s, "title": "Demo", "lastActivityAt": 5 })
                .to_string(),
        )
        .unwrap();
        let registry = home.join(".claude").join("sessions");
        std::fs::create_dir_all(&registry).unwrap();
        let pid = std::process::id();
        std::fs::write(registry.join(format!("{pid}.json")), json!({ "pid": pid, "hostSessionId": sid }).to_string())
            .unwrap();
        let dir = home.join(".claude").join("projects").join(paths::slug(&cwd_s));
        std::fs::create_dir_all(&dir).unwrap();
        let transcript = dir.join(format!("{cli}.jsonl"));
        let limit = json!({ "type": "assistant", "isApiErrorMessage": true, "apiError": "usage_limit_reached",
            "error": "rate_limit", "message": { "role": "assistant", "content": [
                { "type": "text", "text": "You've hit your session limit · resets 3:45pm" }] } });
        std::fs::write(&transcript, format!("{limit}\n")).unwrap();

        let account = SessionAccount {
            id: "acc".into(),
            name: "Work".into(),
            data_dir: data.to_string_lossy().into_owned(),
            is_main: false,
            account_uuid: Some(uuid.into()),
            running: true,
        };
        let sessions = Sessions { home: home.clone() };
        let job = |seen| IndexJob { accounts: vec![account.clone()], sessions: sessions.clone(), seen };
        let now = relay::now_secs();

        let r = build_index(job(HashMap::new()), now);
        let s = r.index.get(cli).expect("indexed by the CLI session id");
        assert_eq!(
            (s.account_id.as_str(), s.session_id.as_str(), s.title.as_str(), s.running),
            ("acc", sid, "Demo", true)
        );
        assert!(r.progressed.is_empty(), "the first look is not progress");
        assert_eq!(r.hits.len(), 1);
        let (acc, hit_cli, hit) = &r.hits[0];
        assert_eq!((acc.as_str(), hit_cli.as_str()), ("acc", cli));
        let hit = hit.as_ref().expect("a limit hit");
        assert_eq!((hit.kind, hit.source), (LimitKind::Session, LimitSource::Transcript));

        // Nothing written since: no scan, no news.
        let r = build_index(job(r.seen), now);
        assert!(r.progressed.is_empty() && r.hits.is_empty());
        assert!(r.seen.values().all(|s| s.hit));

        // The session replied again: progress, and the limit is over.
        std::thread::sleep(Duration::from_millis(30));
        let reply = json!({ "type": "assistant", "message": { "role": "assistant", "content": [
            { "type": "text", "text": "Done." }] } });
        std::fs::write(&transcript, format!("{limit}\n{reply}\n")).unwrap();
        let r = build_index(job(r.seen), now);
        assert_eq!(r.progressed.len(), 1);
        assert_eq!(r.progressed[0].0, cli);
        assert_eq!(r.hits.len(), 1);
        assert!(r.hits[0].2.is_none());
        assert!(r.seen.values().all(|s| !s.hit));
        assert!(t.path().starts_with(std::env::temp_dir()));
    }

    #[test]
    fn stale_state_cleanup_removes_files() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("license.json"), "{}").unwrap();
        std::fs::write(d.path().join("relay.json"), "{}").unwrap();
        remove_stale_state(d.path());
        assert!(!d.path().join("license.json").exists());
        assert!(d.path().join("relay.json").exists());
        remove_stale_state(d.path()); // nothing left: still fine
    }

    /// Counts what a hand-over asks of the target's Claude window.
    struct FakeWindow {
        pids: Vec<u32>,
        opened: std::cell::Cell<u32>,
        focused: std::cell::Cell<bool>,
        restarted: std::cell::Cell<u32>,
        /// Opening and restarting fail.
        broken: bool,
    }

    impl FakeWindow {
        fn new(pids: Vec<u32>) -> FakeWindow {
            FakeWindow {
                pids,
                opened: std::cell::Cell::new(0),
                focused: std::cell::Cell::new(false),
                restarted: std::cell::Cell::new(0),
                broken: false,
            }
        }

        fn broken(pids: Vec<u32>) -> FakeWindow {
            FakeWindow { broken: true, ..FakeWindow::new(pids) }
        }

        fn result(&self) -> Value {
            if self.broken {
                json!({ "ok": false, "error": "Could not restart B's Claude window." })
            } else {
                ok()
            }
        }
    }

    impl TargetWindow for FakeWindow {
        fn pids(&self) -> Vec<u32> {
            self.pids.clone()
        }
        fn open(&self, focus: bool) -> Value {
            self.opened.set(self.opened.get() + 1);
            self.focused.set(focus);
            self.result()
        }
        fn restart(&self, _pids: Vec<u32>) -> Value {
            self.restarted.set(self.restarted.get() + 1);
            self.result()
        }
    }

    const FROM_UUID: &str = "aaaaaaaa-0000-0000-0000-00000000000a";
    const TO_UUID: &str = "bbbbbbbb-0000-0000-0000-00000000000b";

    struct HandoverFixture {
        _tmp: tempfile::TempDir,
        sessions: Sessions,
        from: SessionAccount,
        to: SessionAccount,
        to_records: PathBuf,
        registry: PathBuf,
    }

    fn write_fixture_json(f: &Path, v: &Value) {
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, serde_json::to_string(v).unwrap()).unwrap();
    }

    /// Two accounts under the temp folder; the source has session `local_src`.
    fn handover_fixture() -> HandoverFixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let home = root.join("home");
        let registry = home.join(".claude").join("sessions");
        std::fs::create_dir_all(&registry).unwrap();
        let (dir_a, dir_b) = (root.join("Claude-A"), root.join("Claude-B"));
        write_fixture_json(&dir_a.join("config.json"), &json!({ "lastKnownAccountUuid": FROM_UUID }));
        write_fixture_json(&dir_b.join("config.json"), &json!({ "lastKnownAccountUuid": TO_UUID }));
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        write_fixture_json(
            &dir_a.join("claude-code-sessions").join(FROM_UUID).join("org-a").join("local_src.json"),
            &json!({ "sessionId": "local_src", "cliSessionId": "11111111-1111-1111-1111-111111111111",
                     "cwd": project.to_string_lossy(), "title": "Source" }),
        );
        let to_records = dir_b.join("claude-code-sessions").join(TO_UUID).join("org-b");
        std::fs::create_dir_all(&to_records).unwrap();
        let account = |id: &str, dir: &Path, uuid: &str| SessionAccount {
            id: id.into(),
            name: id.to_uppercase(),
            data_dir: dir.to_string_lossy().into_owned(),
            is_main: false,
            account_uuid: Some(uuid.into()),
            running: false,
        };
        HandoverFixture {
            sessions: Sessions { home },
            from: account("a", &dir_a, FROM_UUID),
            to: account("b", &dir_b, TO_UUID),
            to_records,
            registry,
            _tmp: tmp,
        }
    }

    /// A session of the target that the live registry names (our own pid is alive).
    fn add_live_target_session(f: &HandoverFixture) {
        write_fixture_json(
            &f.to_records.join("local_busy.json"),
            &json!({ "sessionId": "local_busy", "cliSessionId": "22222222-2222-2222-2222-222222222222", "title": "Busy" }),
        );
        let me = std::process::id();
        write_fixture_json(
            &f.registry.join(format!("{me}.json")),
            &json!({ "pid": me, "sessionId": "22222222-2222-2222-2222-222222222222", "hostSessionId": "local_busy" }),
        );
    }

    fn copied(f: &HandoverFixture, out: &HandoverOutcome) -> bool {
        let id = out.result.new_session_id.clone().unwrap_or_default();
        out.result.ok && f.to_records.join(format!("{id}.json")).is_file()
    }

    #[test]
    fn automatic_handover_leaves_an_open_window_with_a_live_session() {
        let f = handover_fixture();
        add_live_target_session(&f);
        let w = FakeWindow::new(vec![4242]);
        let out = run_handover(&f.sessions, &f.from, f.to.clone(), "local_src", HandoverMode::Auto, &w);
        assert!(copied(&f, &out), "{:?}", out.result);
        assert_eq!(w.restarted.get(), 0, "never closed");
        assert_eq!(w.opened.get(), 0);
        assert!(out.restart_needed);
        assert!(!out.window_left_open);
        assert!(out.shown.is_none());
    }

    #[test]
    fn automatic_handover_leaves_an_open_window_without_a_live_session() {
        let f = handover_fixture();
        let w = FakeWindow::new(vec![4242]);
        let out = run_handover(&f.sessions, &f.from, f.to.clone(), "local_src", HandoverMode::Auto, &w);
        assert!(copied(&f, &out), "{:?}", out.result);
        assert_eq!(w.restarted.get(), 0, "never closed");
        assert!(out.restart_needed);
    }

    #[test]
    fn automatic_handover_opens_a_closed_window_unfocused() {
        let f = handover_fixture();
        let w = FakeWindow::new(Vec::new());
        let out = run_handover(&f.sessions, &f.from, f.to.clone(), "local_src", HandoverMode::Auto, &w);
        assert!(copied(&f, &out), "{:?}", out.result);
        assert_eq!(w.opened.get(), 1);
        assert!(!w.focused.get());
        assert_eq!(w.restarted.get(), 0);
        assert!(!out.restart_needed);
    }

    #[test]
    fn manual_restart_keeps_a_window_with_a_live_session_open() {
        let f = handover_fixture();
        add_live_target_session(&f);
        let w = FakeWindow::new(vec![4242]);
        let mode = HandoverMode::Manual { restart: true };
        let out = run_handover(&f.sessions, &f.from, f.to.clone(), "local_src", mode, &w);
        assert!(copied(&f, &out), "{:?}", out.result);
        assert_eq!(w.restarted.get(), 0, "never closed");
        assert_eq!(w.opened.get(), 0);
        assert!(out.window_left_open);
        assert!(out.restart_needed);
        assert!(!out.restarted);
    }

    #[test]
    fn manual_restart_restarts_a_window_without_a_live_session() {
        let f = handover_fixture();
        // A dead registry entry for a target session does not count.
        write_fixture_json(&f.to_records.join("local_old.json"), &json!({ "sessionId": "local_old" }));
        write_fixture_json(
            &f.registry.join("2147483000.json"),
            &json!({ "pid": 2147483000u32, "hostSessionId": "local_old" }),
        );
        let w = FakeWindow::new(vec![4242]);
        let mode = HandoverMode::Manual { restart: true };
        let out = run_handover(&f.sessions, &f.from, f.to.clone(), "local_src", mode, &w);
        assert!(copied(&f, &out), "{:?}", out.result);
        assert_eq!(w.restarted.get(), 1, "closed and opened again");
        assert!(out.restarted);
        assert!(!out.restart_needed);
        assert!(!out.window_left_open);
    }

    #[test]
    fn manual_without_restart_never_closes() {
        let f = handover_fixture();
        let w = FakeWindow::new(vec![4242]);
        let mode = HandoverMode::Manual { restart: false };
        let out = run_handover(&f.sessions, &f.from, f.to.clone(), "local_src", mode, &w);
        assert!(copied(&f, &out), "{:?}", out.result);
        assert_eq!(w.restarted.get(), 0);
        assert_eq!(w.opened.get(), 0);
        assert!(out.restart_needed);
        assert!(!out.window_left_open);
        // No window open: it is opened (focused), still never closed.
        let w = FakeWindow::new(Vec::new());
        let out = run_handover(&f.sessions, &f.from, f.to.clone(), "local_src", mode, &w);
        assert!(copied(&f, &out), "{:?}", out.result);
        assert_eq!((w.opened.get(), w.restarted.get()), (1, 0));
        assert!(w.focused.get());
    }

    #[test]
    fn manual_restart_keeps_a_window_open_when_a_live_registry_entry_is_unreadable() {
        let f = handover_fixture();
        add_live_target_session(&f);
        // Our own (live) pid's registry file, caught part-written.
        std::fs::write(f.registry.join(format!("{}.json", std::process::id())), "{\"pid\": ").unwrap();
        let w = FakeWindow::new(vec![4242]);
        let mode = HandoverMode::Manual { restart: true };
        let out = run_handover(&f.sessions, &f.from, f.to.clone(), "local_src", mode, &w);
        assert!(copied(&f, &out), "{:?}", out.result);
        assert_eq!(w.restarted.get(), 0, "never closed");
        assert!(out.window_left_open);
        assert!(out.restart_needed);
    }

    #[test]
    fn a_failed_restart_asks_for_a_restart() {
        let f = handover_fixture();
        let w = FakeWindow::broken(vec![4242]);
        let mode = HandoverMode::Manual { restart: true };
        let out = run_handover(&f.sessions, &f.from, f.to.clone(), "local_src", mode, &w);
        assert!(copied(&f, &out), "{:?}", out.result);
        assert_eq!(w.restarted.get(), 1);
        assert!(!out.restarted);
        assert!(out.restart_needed);
        assert!(!out.window_left_open);
    }

    #[test]
    fn a_failed_open_asks_for_a_restart() {
        for mode in [HandoverMode::Auto, HandoverMode::Manual { restart: true }] {
            let f = handover_fixture();
            let w = FakeWindow::broken(Vec::new());
            let out = run_handover(&f.sessions, &f.from, f.to.clone(), "local_src", mode, &w);
            assert!(copied(&f, &out), "{:?}", out.result);
            assert_eq!((w.opened.get(), w.restarted.get()), (1, 0));
            assert!(out.restart_needed, "{mode:?}");
            assert!(!out.restarted);
        }
    }

    #[test]
    fn failed_copy_leaves_the_window_alone() {
        let f = handover_fixture();
        let w = FakeWindow::new(Vec::new());
        let out = run_handover(&f.sessions, &f.from, f.to.clone(), "local_missing", HandoverMode::Auto, &w);
        assert!(!out.result.ok);
        assert_eq!((w.opened.get(), w.restarted.get()), (0, 0));
    }

    #[test]
    fn focus_failure_reasons() {
        use crate::platform::FocusFailure as F;
        let all = [
            (F::Refused, "focus-refused"),
            (F::NoWindow, "focus-no-window"),
            (F::AutomationDenied, "focus-automation-denied"),
            (F::AccessibilityOff, "focus-accessibility-off"),
            (F::AccessibilityUntrusted, "focus-accessibility-untrusted"),
            (F::NotResponding, "focus-not-responding"),
            (F::Gone, "focus-gone"),
            (F::Other, "focus-failed"),
        ];
        for (f, r) in all {
            assert_eq!(f.reason(), r);
        }
    }

    fn plan(id: &str, name: &str) -> OpenPlan {
        OpenPlan { id: id.into(), name: name.into(), is_main: false, dir: None, route: false }
    }

    #[test]
    fn open_log_lines() {
        assert_eq!(open_log_line("a1", OpenFrom::Panel, ""), "open a1 via panel: ok");
        assert_eq!(
            open_log_line("a1", OpenFrom::Bubble, "failed: focus-no-window pid=7 window=false"),
            "open a1 via bubble: failed: focus-no-window pid=7 window=false"
        );
        assert_eq!(open_log_line("a1", OpenFrom::Tray, "launched"), "open a1 via tray: launched");
        assert_eq!(OpenFrom::Added.as_str(), "add");
        assert_eq!(OpenFrom::Handover.as_str(), "handover");
    }

    #[test]
    fn open_failed_event_payload() {
        let v = json!({ "ok": false, "error": "Could not bring Work's Claude window to the front.", "reason": "focus-refused" });
        assert_eq!(
            open_failed_payload(&plan("a1", "Work"), &v),
            json!({ "id": "a1", "name": "Work", "reason": "focus-refused", "error": "Could not bring Work's Claude window to the front." })
        );
        let v = fail("Claude Desktop was not found on this PC.");
        assert_eq!(open_failed_payload(&plan("a2", "Home"), &v)["reason"], Value::Null);
    }

    #[test]
    fn process_summary() {
        assert_eq!(procs_summary(&[], &[]), "none");
        let p = |pid: u32, dir: Option<&str>, hwnd: isize| Proc {
            pid,
            exe: String::new(),
            cmdline: String::new(),
            data_dir: dir.map(String::from),
            hwnd,
        };
        let procs = [p(10, None, 5), p(11, Some("/d/acc2"), 0)];
        let acc = |id: &str, pid: Option<u32>| Account {
            id: id.into(),
            name: id.into(),
            data_dir: None,
            color: String::new(),
            initials: String::new(),
            label: None,
            is_main: false,
            signed_in: false,
            account_uuid: None,
            running: pid.is_some(),
            pid,
            session_count: 0,
            code_ready: false,
            same_sign_in_as: None,
            limited: None,
        };
        assert_eq!(
            procs_summary(&procs, &[acc("main", Some(10)), acc("other", None)]),
            "pid=10 acc=main dir=default window=true; pid=11 acc=none dir=/d/acc2 window=false"
        );
    }
}
