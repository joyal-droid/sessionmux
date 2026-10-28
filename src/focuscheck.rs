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

//! `Sessionmux --focus-check <plan.json>`, only with `SESSIONMUX_FOCUS_CHECK=1` (macOS): runs the
//! app as a normal start does (its own data folder, event loop, floating button, panel and menu
//! bar icon) and checks, one step at a time, what a click on an account really does.
//!
//! The plan (JSON) names the result file (`out`), a folder for the hand-shake with the script
//! that drives the check (`sync`), optionally a data folder to use instead of the usual one
//! (`userData`), the accounts that must be running before the first step (`waitForRunning`) and
//! the steps. Each step has a `name` (letters, digits, `-` and `_`), the `account` to open, the
//! surface it is opened from (`via`: `panel` — a click on its row in the panel; `bubble` — a
//! click on its avatar in the floating button's mini tray; `posted` — opened from posted work
//! with no input event at all), what must happen (`expect`: `front` — its Claude window ends up
//! in front; `minimised` — its Claude window was in front and the click put it away: minimised
//! or the app hidden, no window of it on screen any more; `visible-failure` — the open fails and
//! the panel shows why; `record` — nothing is judged), optionally a settings patch applied first
//! (`settings`) and whether a window of the account must be on screen before the click
//! (`windowBefore`).
//!
//! For every step:
//! 1. the script arranges the windows and writes `<sync>/<name>.go`, optionally with a JSON
//!    note about what it did;
//! 2. Sessionmux records what is on screen and gets the click target ready: for `panel` it shows
//!    the panel on the Accounts tab, for `bubble` it hides the panel. It writes
//!    `<sync>/<name>.target.json`: `{"kind":"panel"|"bubble","x":..,"y":..}`, in points from the
//!    top left of the primary display, or `{"kind":"none"}` for `posted`;
//! 3. the script posts a real mouse click at that point and writes `<sync>/<name>.clicked`:
//!    `{"posted":true|false,"detail":".."}` (a `posted` step opens the account itself instead);
//! 4. Sessionmux waits for the open to finish, lets the screen settle, records what is on screen
//!    again, reads the panel's toasts and writes `<sync>/<name>.done`.
//!
//! The result file is rewritten after every step. Per step it holds the reply of the open, the
//! app in front, the owner of the frontmost normal window, whether the account's Claude has a
//! window on screen, whether the panel is visible, the left clicks Sessionmux received, the
//! events sent to the panel, the toasts on the page, the new lines of the log and a verdict:
//! `pass`, `fail`, `unproven` (the click could not be posted, never reached Sessionmux, or the
//! windows were not arranged as the step needs) or `recorded`. When everything ran, the app
//! quits.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSEvent, NSEventMask};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::app;
use crate::brand;
use crate::core::claude_app;
use crate::platform::{self, Os, Pid, SingleInstance, Windows};
use crate::ui::bubble::model::avatar_centre;
use crate::util::{log, paths};

/// How long the script may take to arrange the windows for a step.
const GO_WAIT: Duration = Duration::from_secs(240);
/// How long the script may take to post a click.
const CLICK_WAIT: Duration = Duration::from_secs(30);
/// How long an open may take (activation, reopen request, Accessibility, waiting for a window).
const OPEN_WAIT: Duration = Duration::from_secs(45);
/// How long to wait for a posted click to reach Sessionmux.
const NO_CLICK_WAIT: Duration = Duration::from_secs(5);
/// Time for the screen to settle after an open finished.
const SETTLE: Duration = Duration::from_millis(1500);
/// How long the accounts in `waitForRunning` may take to be listed as running.
const RUNNING_WAIT: Duration = Duration::from_secs(90);
/// How long the panel may take to be shown with its page ready.
const PANEL_WAIT: Duration = Duration::from_secs(10);
/// How long one request to the main thread may take.
const MAIN_WAIT: Duration = Duration::from_secs(5);
/// Events kept per step.
const MAX_EVENTS: usize = 200;
/// Log lines kept per step.
const MAX_LOG_LINES: usize = 300;
/// Events whose payload is kept (the others are kept by name).
const KEEP_PAYLOAD: [&str; 2] = ["openFailed", "show-tab"];

static ACTIVE: AtomicBool = AtomicBool::new(false);
static RECORD: Mutex<Record> = Mutex::new(Record::new());

/// What the app reported during the current step.
struct Record {
    events: Vec<Value>,
    opens: Vec<Value>,
    /// Window numbers of the left clicks Sessionmux received.
    clicks: Vec<i64>,
}

impl Record {
    const fn new() -> Record {
        Record { events: Vec::new(), opens: Vec::new(), clicks: Vec::new() }
    }
}

fn with_record<R>(f: impl FnOnce(&mut Record) -> R) -> Option<R> {
    RECORD.lock().ok().map(|mut r| f(&mut r))
}

/// True while a focus check runs.
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// An event sent to the panel's page.
pub fn note_event(event: &str, payload: &Value) {
    if !active() {
        return;
    }
    with_record(|r| {
        if r.events.len() < MAX_EVENTS {
            let payload = if KEEP_PAYLOAD.contains(&event) { payload.clone() } else { Value::Null };
            r.events.push(json!({ "event": event, "payload": payload }));
        }
    });
}

/// An open finished: its account, surface, what happened, its reply and whether the panel was
/// visible once the reply was delivered.
pub fn note_open(account: &str, via: &str, note: &str, reply: Value, panel_visible: bool) {
    if !active() {
        return;
    }
    with_record(|r| {
        r.opens.push(json!({
            "account": account, "via": via, "note": note, "reply": reply, "panelVisibleAtReply": panel_visible,
        }))
    });
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Plan {
    out: PathBuf,
    sync: PathBuf,
    #[serde(default)]
    user_data: Option<PathBuf>,
    #[serde(default)]
    wait_for_running: Vec<String>,
    steps: Vec<Step>,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Step {
    name: String,
    account: String,
    via: Via,
    expect: Expect,
    #[serde(default)]
    settings: Option<Value>,
    #[serde(default)]
    window_before: Option<bool>,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Via {
    Panel,
    Bubble,
    Posted,
}

impl Via {
    /// The surface an open of this step is reported from.
    fn surface(self) -> &'static str {
        match self {
            Via::Panel => "panel",
            Via::Bubble => "bubble",
            Via::Posted => "check",
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Expect {
    Front,
    Minimised,
    VisibleFailure,
    Record,
}

/// What the script reports about the click it posted.
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq)]
struct Clicked {
    #[serde(default)]
    posted: bool,
    #[serde(default)]
    detail: String,
}

/// What is on screen, as seen from Sessionmux.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Seen {
    /// The app in front (AppKit).
    front_app_pid: Option<Pid>,
    /// The owner of the frontmost normal window (window server).
    top_window_pid: Option<Pid>,
    /// The account's Claude processes.
    target_pids: Vec<Pid>,
    /// Whether one of them has a normal window on screen.
    target_on_screen: bool,
    /// Whether one of them has a normal window off screen (minimised, closed to the Dock, on
    /// another Space).
    target_off_screen: bool,
    /// Whether one of them is hidden.
    target_hidden: bool,
    panel_visible: bool,
}

impl Seen {
    fn target_in_front(&self) -> bool {
        let is_target = |p: Option<Pid>| p.is_some_and(|p| self.target_pids.contains(&p));
        is_target(self.front_app_pid) && is_target(self.top_window_pid) && self.target_on_screen
    }

    /// The account's Claude shows no window: none on screen, none in front. Its app may stay the
    /// active one only with its window minimised to the Dock (as after Minimise in its menu); a
    /// hidden app never is.
    fn target_put_away(&self) -> bool {
        let is_target = |p: Option<Pid>| p.is_some_and(|p| self.target_pids.contains(&p));
        let minimised = self.target_off_screen && !self.target_hidden;
        !self.target_on_screen && !is_target(self.top_window_pid) && (!is_target(self.front_app_pid) || minimised)
    }
}

/// Everything one step saw, for its verdict.
struct Outcome<'a> {
    step: &'a Step,
    /// Why the windows could not be arranged for the step.
    setup: Option<&'a str>,
    /// Why Sessionmux could not get the click target ready.
    target: Option<&'a str>,
    before: &'a Seen,
    clicked: Option<&'a Clicked>,
    clicks: usize,
    open: Option<&'a Value>,
    after: &'a Seen,
    toasts: &'a [String],
}

/// The verdict of a step and why.
fn judge(o: &Outcome) -> (&'static str, String) {
    if let Some(e) = o.setup {
        return ("unproven", format!("the windows could not be arranged: {e}"));
    }
    if let Some(want) = o.step.window_before {
        if o.before.target_on_screen != want {
            let what = if want { "a window" } else { "no window" };
            return ("unproven", format!("the account's Claude should have had {what} on screen before the click"));
        }
    }
    if o.step.expect == Expect::Front && o.before.target_in_front() {
        return ("unproven", "the account's Claude window was already in front before the click".into());
    }
    if o.step.expect == Expect::Minimised && !o.before.target_in_front() {
        return ("unproven", format!("the account's Claude window was not in front before the click: {:?}", o.before));
    }
    if let Some(e) = o.target {
        return ("fail", format!("the click target could not be found: {e}"));
    }
    if o.step.via != Via::Posted {
        match o.clicked {
            None => return ("unproven", "no click was posted".into()),
            Some(c) if !c.posted => return ("unproven", format!("the click could not be posted: {}", c.detail)),
            _ => {}
        }
        if o.open.is_none() && o.clicks == 0 {
            return ("unproven", "the posted click never reached Sessionmux".into());
        }
    }
    let Some(open) = o.open else {
        return ("fail", format!("Sessionmux received {} click(s) but did not open the account", o.clicks));
    };
    if open["via"] != o.step.via.surface() {
        return ("fail", format!("the account was opened via {}, not via {}", open["via"], o.step.via.surface()));
    }
    let reply = &open["reply"];
    let ok = reply["ok"].as_bool().unwrap_or(false);
    let reason = reply["reason"].as_str().unwrap_or("");
    match o.step.expect {
        Expect::Record => ("recorded", format!("ok={ok} reason={reason:?} note={}", open["note"])),
        Expect::Front => {
            if !ok {
                ("fail", format!("the open failed: {} ({reason})", reply["error"].as_str().unwrap_or("")))
            } else if !o.after.target_in_front() {
                ("fail", format!("the open replied ok but the account's window is not in front: {:?}", o.after))
            } else {
                ("pass", "the account's Claude window is in front".into())
            }
        }
        Expect::Minimised => {
            if !ok {
                ("fail", format!("the open failed: {} ({reason})", reply["error"].as_str().unwrap_or("")))
            } else if reply["minimised"] != true {
                ("fail", format!("the open did not minimise the window: {}", open["note"]))
            } else if !o.after.target_put_away() {
                ("fail", format!("the open replied minimised but the account's window still shows: {:?}", o.after))
            } else if !o.toasts.is_empty() {
                ("fail", format!("the panel shows a message: {:?}", o.toasts))
            } else {
                let how = if o.after.target_hidden { "the app is hidden" } else { "the window is minimised" };
                ("pass", format!("the account's Claude window was put away ({how})"))
            }
        }
        Expect::VisibleFailure => {
            let at_reply = open["panelVisibleAtReply"].as_bool().unwrap_or(false);
            if ok {
                ("fail", "the open did not fail".into())
            } else if reason.is_empty() {
                ("fail", "the failure carries no reason".into())
            } else if !at_reply || !o.after.panel_visible {
                (
                    "fail",
                    format!("the panel was not visible (at the reply: {at_reply}, after: {})", o.after.panel_visible),
                )
            } else if o.toasts.is_empty() {
                ("fail", "the panel shows no message".into())
            } else {
                ("pass", format!("the panel shows the failure ({reason})"))
            }
        }
    }
}

fn valid_name(s: &str) -> bool {
    (1..=40).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn read_plan(path: &Path) -> Result<Plan, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let plan: Plan = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some(s) = plan.steps.iter().find(|s| !valid_name(&s.name)) {
        return Err(format!("step name {:?} is not allowed", s.name));
    }
    Ok(plan)
}

/// Run the focus check; returns the process exit code.
pub fn run(plan_path: &Path) -> i32 {
    let plan = match read_plan(plan_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("focus check: {e}");
            return 2;
        }
    };
    if let Some(dir) = plan.user_data.clone() {
        paths::set_user_data(Some(dir));
    }
    log::init(&paths::log_file());
    let Some(guard) = platform::instance::Os::acquire() else {
        log::warn("focus check: another instance is running");
        eprintln!("focus check: another instance of Sessionmux is running");
        return 3;
    };
    log::info(&format!("{} {} starting (focus check, {} steps)", brand::NAME, brand::VERSION, plan.steps.len()));
    ACTIVE.store(true, Ordering::Relaxed);
    let launch = move |_login_item: bool| {
        app::init(true).inspect_err(|e| log::error(&format!("startup failed: {e}")))?;
        if let Some(mtm) = MainThreadMarker::new() {
            watch_clicks(mtm);
        }
        std::thread::Builder::new()
            .name("sessionmux-focus-check".into())
            .spawn(move || drive(plan))
            .map_err(|e| format!("focus check: {e}"))?;
        Ok(())
    };
    fn finish() {
        app::shutdown();
        log::info("exit");
    }
    let code = platform::event_loop::run_app(launch, finish);
    drop(guard);
    code
}

/// Count the left clicks that reach Sessionmux's windows, whichever window they hit.
fn watch_clicks(_mtm: MainThreadMarker) {
    let handler = RcBlock::new(|ev: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit hands the monitor a live event for the duration of the call.
        let number = unsafe { ev.as_ref() }.windowNumber() as i64;
        with_record(|r| r.clicks.push(number));
        ev.as_ptr()
    });
    // SAFETY: the handler returns the event it was given, which is a valid pointer.
    let monitor =
        unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::LeftMouseDown, &handler) };
    if monitor.is_none() {
        log::warn("focus check: clicks cannot be watched");
    }
    // Kept for the life of the process.
    std::mem::forget(monitor);
}

/// Run `f` on the main thread and wait for its result.
fn on_main<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> Option<R> {
    platform::window::on_main_wait(MAIN_WAIT, move |_| f())
}

fn wait_file(path: &Path, within: Duration) -> Option<String> {
    let end = Instant::now() + within;
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            return Some(text);
        }
        if Instant::now() >= end {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn write_file(path: &Path, text: &str) {
    // Written next to the target and renamed, so the script never reads half a file.
    let tmp = path.with_extension("part");
    if std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, path)).is_err() {
        log::warn(&format!("focus check: cannot write {}", path.display()));
    }
}

fn log_len() -> u64 {
    std::fs::metadata(paths::log_file()).map(|m| m.len()).unwrap_or(0)
}

/// The log lines written since `from` (all of them when the log was rotated meanwhile).
fn log_since(from: u64) -> Vec<String> {
    let Ok(mut f) = std::fs::File::open(paths::log_file()) else { return Vec::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = if len < from { 0 } else { from };
    let mut text = String::new();
    if f.seek(SeekFrom::Start(start)).is_err() || f.read_to_string(&mut text).is_err() {
        return Vec::new();
    }
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let skip = lines.len().saturating_sub(MAX_LOG_LINES);
    lines.into_iter().skip(skip).collect()
}

/// The pids of the Claude processes running with data folder `dir` (None: the default one).
fn target_pids(dir: Option<&str>) -> Vec<Pid> {
    claude_app::list_processes().into_iter().filter(|p| claude_app::matches_data_dir(p, dir)).map(|p| p.pid).collect()
}

fn observe(dir: Option<&str>) -> Seen {
    let target_pids = target_pids(dir);
    let target_on_screen = target_pids.iter().any(|&p| <Os as Windows>::main_window(p).is_some());
    let target_off_screen = target_pids.iter().any(|&p| platform::window::has_off_screen_window(p));
    let target_hidden = target_pids.iter().any(|&p| platform::window::app_hidden(p));
    Seen {
        front_app_pid: <Os as Windows>::foreground_pid(),
        top_window_pid: platform::window::front_window_owner(),
        target_pids,
        target_on_screen,
        target_off_screen,
        target_hidden,
        panel_visible: on_main(|| app::check_panel().is_some_and(|(_, visible, _)| visible)).unwrap_or(false),
    }
}

/// Run `js` in the panel's page and wait for its result, parsed.
fn panel_eval(js: String) -> Option<Value> {
    let (tx, rx) = mpsc::channel::<String>();
    let tx = Mutex::new(tx);
    let started = on_main(move || {
        app::check_panel_eval(&js, move |r| {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(r);
            }
        })
    })?;
    if !started {
        return None;
    }
    let text = rx.recv_timeout(MAIN_WAIT).ok()?;
    serde_json::from_str(&text).ok()
}

/// The texts of the toasts the panel shows.
fn toasts() -> Vec<String> {
    let js = "Array.from(document.querySelectorAll('#toasts .toast:not(.is-leaving)'), \
              (t) => t.innerText.replace(/\\s+/g, ' ').trim())";
    match panel_eval(js.to_string()) {
        Some(Value::Array(v)) => v.into_iter().filter_map(|t| t.as_str().map(str::to_string)).collect(),
        _ => Vec::new(),
    }
}

/// Show the panel and find the centre of `account`'s row, in screen points.
fn panel_target(account: &str) -> Result<(f64, f64), String> {
    on_main(app::check_show_accounts).ok_or("the main thread did not answer")?;
    let end = Instant::now() + PANEL_WAIT;
    let window = loop {
        if let Some(Some((w, true, true))) = on_main(app::check_panel) {
            break w;
        }
        if Instant::now() >= end {
            return Err("the panel was not shown with its page ready".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let id = serde_json::to_string(account).unwrap_or_default();
    let js = format!(
        "(() => {{ const row = document.querySelector('.acct[data-id=\"' + CSS.escape({id}) + '\"]'); \
         const b = row && (row.querySelector('.acct__main') || row); if (!b) return null; \
         b.scrollIntoView({{ block: 'nearest' }}); const r = b.getBoundingClientRect(); \
         return {{ x: r.left, y: r.top, w: r.width, h: r.height }}; }})()"
    );
    let end = Instant::now() + PANEL_WAIT;
    let rect = loop {
        // The page draws its rows shortly after it is shown.
        std::thread::sleep(Duration::from_millis(400));
        if let Some(r) = panel_eval(js.clone()).filter(|r| r["w"].as_f64().unwrap_or(0.0) > 0.0) {
            break r;
        }
        if Instant::now() >= end {
            return Err(format!("the panel shows no row for {account}"));
        }
    };
    let page = on_main(move || platform::panel::page_rect(window)).flatten().ok_or("the panel's page has no frame")?;
    let f = |k: &str| rect[k].as_f64().unwrap_or(0.0);
    Ok((f64::from(page.x) + f("x") + f("w") / 2.0, f64::from(page.y) + f("y") + f("h") / 2.0))
}

/// Hide the panel and find the centre of `account`'s avatar in the floating button, in screen
/// points.
fn bubble_target(account: &str) -> Result<(f64, f64), String> {
    on_main(app::check_hide_panel).ok_or("the main thread did not answer")?;
    std::thread::sleep(Duration::from_millis(500));
    let acc = account.to_string();
    let (rect, slot) = on_main(move || {
        let slot = app::check_account(&acc).and_then(|(_, _, slot, n)| slot.map(|s| (s, n)));
        (app::smoke_bubble_rect().map(|(r, _)| r), slot)
    })
    .ok_or("the main thread did not answer")?;
    let rect = rect.ok_or("the floating button is not shown")?;
    let (slot, n) = slot.ok_or_else(|| format!("the floating button shows no avatar for {account}"))?;
    let (cx, cy) = avatar_centre(n, slot).ok_or("no avatar slot")?;
    Ok((f64::from(rect.x) + f64::from(cx), f64::from(rect.y) + f64::from(cy)))
}

/// Run every step, then quit the app.
fn drive(plan: Plan) {
    let started = Instant::now();
    let mut ready = json!({});
    let end = Instant::now() + RUNNING_WAIT;
    loop {
        let ids = plan.wait_for_running.clone();
        let running: Vec<bool> =
            on_main(move || ids.iter().map(|id| app::check_running(id)).collect()).unwrap_or_default();
        let all = running.len() == plan.wait_for_running.len() && running.iter().all(|&r| r);
        if all || Instant::now() >= end {
            for (id, r) in plan.wait_for_running.iter().zip(running.iter().chain(std::iter::repeat(&false))) {
                ready[id] = json!(r);
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    log::info(&format!("focus check: accounts running at the start: {ready}"));
    let _ = std::fs::create_dir_all(&plan.sync);
    let mut results = Vec::new();
    let write = |results: &[Value], finished: bool| {
        let doc = json!({
            "version": brand::VERSION,
            "pid": std::process::id(),
            "accountsRunning": ready,
            "steps": results,
            "finished": finished,
            "elapsedMs": started.elapsed().as_millis() as u64,
        });
        write_file(&plan.out, &serde_json::to_string_pretty(&doc).unwrap_or_default());
    };
    write(&results, false);
    for step in &plan.steps {
        log::info(&format!("focus check: step {} waits for the windows", step.name));
        let result = run_step(&plan.sync, step);
        log::info(&format!("focus check: step {}: {} ({})", step.name, result["verdict"], result["why"]));
        results.push(result);
        write(&results, false);
        write_file(&plan.sync.join(format!("{}.done", step.name)), "{}");
    }
    write(&results, true);
    log::info("focus check: done");
    on_main(app::quit);
}

fn run_step(sync: &Path, step: &Step) -> Value {
    let go = wait_file(&sync.join(format!("{}.go", step.name)), GO_WAIT);
    let go_note: Value = go.as_deref().and_then(|t| serde_json::from_str(t).ok()).unwrap_or(Value::Null);
    let mut setup = match &go {
        None => Some("the script did not finish arranging the windows".to_string()),
        Some(_) => go_note["error"].as_str().map(str::to_string),
    };
    let account = step.account.clone();
    let info = on_main(move || app::check_account(&account)).flatten();
    let dir = info.as_ref().and_then(|(_, d, _, _)| d.clone());
    if info.is_none() && setup.is_none() {
        setup = Some(format!("Sessionmux lists no account {}", step.account));
    }
    if let Some(patch) = step.settings.clone() {
        if let Some(Err(e)) = on_main(move || app::smoke_set_settings(patch)) {
            setup.get_or_insert(format!("settings: {e}"));
        }
    }
    with_record(|r| {
        r.events.clear();
        r.opens.clear();
    });
    let log_from = log_len();
    let before = observe(dir.as_deref());

    let target = match (setup.is_some(), step.via) {
        (true, _) => Err("not prepared".to_string()),
        (false, Via::Panel) => panel_target(&step.account),
        (false, Via::Bubble) => bubble_target(&step.account),
        (false, Via::Posted) => Ok((0.0, 0.0)),
    };
    let mut target_error = if setup.is_none() { target.as_ref().err().cloned() } else { None };
    with_record(|r| r.clicks.clear());
    let target_file = sync.join(format!("{}.target.json", step.name));
    let mut clicked = None;
    match (&target, step.via) {
        (Ok(_), Via::Posted) => {
            write_file(&target_file, &json!({ "kind": "none" }).to_string());
            let account = step.account.clone();
            if let Some(Err(e)) = on_main(move || app::check_open(&account)) {
                target_error.get_or_insert(format!("open: {e}"));
            }
        }
        (Ok((x, y)), via) => {
            let kind = if via == Via::Panel { "panel" } else { "bubble" };
            log::info(&format!("focus check: step {} clicks the {kind} at ({x:.0}, {y:.0})", step.name));
            write_file(&target_file, &json!({ "kind": kind, "x": x, "y": y }).to_string());
            clicked = wait_file(&sync.join(format!("{}.clicked", step.name)), CLICK_WAIT)
                .map(|t| serde_json::from_str::<Clicked>(&t).unwrap_or_default());
        }
        (Err(e), _) => write_file(&target_file, &json!({ "kind": "none", "error": e }).to_string()),
    }

    let waiting = Instant::now();
    let open = loop {
        let found = with_record(|r| {
            r.opens.iter().find(|o| o["account"] == step.account.as_str() && o["via"] == step.via.surface()).cloned()
        })
        .flatten();
        if found.is_some() || setup.is_some() || target_error.is_some() || waiting.elapsed() >= OPEN_WAIT {
            break found;
        }
        // A click that never reached Sessionmux opens nothing: no need to wait out the open.
        let clicks = with_record(|r| r.clicks.len()).unwrap_or(0);
        if step.via != Via::Posted && clicks == 0 && waiting.elapsed() >= NO_CLICK_WAIT {
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    std::thread::sleep(SETTLE);
    let after = observe(dir.as_deref());
    let toasts = toasts();
    let (events, clicks) = with_record(|r| (r.events.clone(), r.clicks.clone())).unwrap_or_default();
    let (verdict, why) = judge(&Outcome {
        step,
        setup: setup.as_deref(),
        target: target_error.as_deref(),
        before: &before,
        clicked: clicked.as_ref(),
        clicks: clicks.len(),
        open: open.as_ref(),
        after: &after,
        toasts: &toasts,
    });
    json!({
        "name": step.name,
        "account": step.account,
        "via": step.via,
        "expect": step.expect,
        "verdict": verdict,
        "why": why,
        "setup": go_note,
        "target": target.as_ref().map(|&(x, y)| json!({ "x": x, "y": y })).unwrap_or(Value::Null),
        "clicked": clicked,
        "clicksReceived": clicks,
        "open": open,
        "before": before,
        "after": after,
        "toasts": toasts,
        "events": events,
        "log": log_since(log_from),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(via: Via, expect: Expect) -> Step {
        Step { name: "S".into(), account: "a".into(), via, expect, settings: None, window_before: None }
    }

    fn front(pids: &[Pid], on_screen: bool) -> Seen {
        Seen {
            front_app_pid: pids.first().copied(),
            top_window_pid: pids.first().copied(),
            target_pids: pids.to_vec(),
            target_on_screen: on_screen,
            ..Seen::default()
        }
    }

    fn open(via: Via, ok: bool, reason: &str, visible: bool) -> Value {
        json!({ "account": "a", "via": via.surface(), "note": "", "panelVisibleAtReply": visible,
                "reply": if ok { json!({ "ok": true }) } else { json!({ "ok": false, "error": "e", "reason": reason }) } })
    }

    fn verdict(
        s: &Step,
        clicked: Option<&Clicked>,
        clicks: usize,
        open: Option<&Value>,
        after: &Seen,
        toasts: &[String],
    ) -> &'static str {
        let before = Seen::default();
        judge(&Outcome { step: s, setup: None, target: None, before: &before, clicked, clicks, open, after, toasts }).0
    }

    #[test]
    fn a_click_that_was_not_delivered_is_unproven() {
        let s = step(Via::Panel, Expect::Front);
        let seen = front(&[7], true);
        let not_posted = Clicked { posted: false, detail: "not allowed".into() };
        let posted = Clicked { posted: true, detail: String::new() };
        assert_eq!(verdict(&s, None, 0, None, &seen, &[]), "unproven");
        assert_eq!(verdict(&s, Some(&not_posted), 0, None, &seen, &[]), "unproven");
        assert_eq!(verdict(&s, Some(&posted), 0, None, &seen, &[]), "unproven");
        // The click arrived and nothing happened: that is the failure the check looks for.
        assert_eq!(verdict(&s, Some(&posted), 1, None, &seen, &[]), "fail");
    }

    #[test]
    fn front_needs_an_ok_reply_and_the_window_in_front() {
        let s = step(Via::Bubble, Expect::Front);
        let c = Clicked { posted: true, detail: String::new() };
        let ok = open(Via::Bubble, true, "", false);
        assert_eq!(verdict(&s, Some(&c), 1, Some(&ok), &front(&[7], true), &[]), "pass");
        assert_eq!(verdict(&s, Some(&c), 1, Some(&ok), &front(&[7], false), &[]), "fail");
        let other = Seen { front_app_pid: Some(9), ..front(&[7], true) };
        assert_eq!(verdict(&s, Some(&c), 1, Some(&ok), &other, &[]), "fail");
        let failed = open(Via::Bubble, false, "focus-no-window", true);
        assert_eq!(verdict(&s, Some(&c), 1, Some(&failed), &front(&[7], true), &[]), "fail");
        // An open of the account from another surface does not count for the click.
        let elsewhere = open(Via::Panel, true, "", false);
        assert_eq!(verdict(&s, Some(&c), 1, Some(&elsewhere), &front(&[7], true), &[]), "fail");
        // Already in front before the click: the click proves nothing.
        let before = front(&[7], true);
        let out = Outcome {
            step: &s,
            setup: None,
            target: None,
            before: &before,
            clicked: Some(&c),
            clicks: 1,
            open: Some(&ok),
            after: &before,
            toasts: &[],
        };
        assert_eq!(judge(&out).0, "unproven");
    }

    #[test]
    fn minimised_needs_the_window_in_front_before_and_put_away_after() {
        let s = step(Via::Panel, Expect::Minimised);
        let c = Clicked { posted: true, detail: String::new() };
        let put = |reply: Value| json!({ "account": "a", "via": "panel", "note": "", "panelVisibleAtReply": true, "reply": reply });
        let minimised = put(json!({ "ok": true, "minimised": true }));
        let judged = |before: &Seen, open: &Value, after: &Seen, toasts: &[String]| {
            judge(&Outcome {
                step: &s,
                setup: None,
                target: None,
                before,
                clicked: Some(&c),
                clicks: 1,
                open: Some(open),
                after,
                toasts,
            })
            .0
        };
        let in_front = front(&[7], true);
        // Hidden: nothing of it on screen, another app in front.
        let hidden = Seen {
            front_app_pid: Some(3),
            top_window_pid: Some(3),
            target_pids: vec![7],
            target_off_screen: true,
            target_hidden: true,
            ..Seen::default()
        };
        assert_eq!(judged(&in_front, &minimised, &hidden, &[]), "pass");
        // Minimised to the Dock: the app may stay active, with no window on screen.
        let docked = Seen { front_app_pid: Some(7), target_hidden: false, ..hidden.clone() };
        assert_eq!(judged(&in_front, &minimised, &docked, &[]), "pass");
        // Hidden yet reported in front, or a window still on screen: not put away.
        let odd = Seen { front_app_pid: Some(7), ..hidden.clone() };
        assert_eq!(judged(&in_front, &minimised, &odd, &[]), "fail");
        assert_eq!(judged(&in_front, &minimised, &in_front, &[]), "fail");
        // Brought forward instead, a failure or a message in the panel.
        assert_eq!(judged(&in_front, &put(json!({ "ok": true })), &hidden, &[]), "fail");
        assert_eq!(judged(&in_front, &put(json!({ "ok": false, "error": "e", "reason": "r" })), &hidden, &[]), "fail");
        assert_eq!(judged(&in_front, &minimised, &hidden, &["Couldn't switch".to_string()]), "fail");
        // Not in front before the click: the click proves nothing.
        assert_eq!(judged(&hidden, &minimised, &hidden, &[]), "unproven");
    }

    #[test]
    fn a_visible_failure_needs_the_panel_and_a_message() {
        let s = step(Via::Panel, Expect::VisibleFailure);
        let c = Clicked { posted: true, detail: String::new() };
        let shown = Seen { panel_visible: true, ..Seen::default() };
        let msg = vec!["Couldn't switch to B".to_string()];
        let failed = open(Via::Panel, false, "focus-refused", true);
        assert_eq!(verdict(&s, Some(&c), 1, Some(&failed), &shown, &msg), "pass");
        assert_eq!(verdict(&s, Some(&c), 1, Some(&failed), &shown, &[]), "fail");
        assert_eq!(verdict(&s, Some(&c), 1, Some(&failed), &Seen::default(), &msg), "fail");
        assert_eq!(
            verdict(&s, Some(&c), 1, Some(&open(Via::Panel, false, "focus-refused", false)), &shown, &msg),
            "fail"
        );
        assert_eq!(verdict(&s, Some(&c), 1, Some(&open(Via::Panel, false, "", true)), &shown, &msg), "fail");
        assert_eq!(verdict(&s, Some(&c), 1, Some(&open(Via::Panel, true, "", true)), &shown, &msg), "fail");
    }

    #[test]
    fn posted_steps_and_recordings() {
        let posted = step(Via::Posted, Expect::Record);
        assert_eq!(
            verdict(&posted, None, 0, Some(&open(Via::Posted, false, "focus-refused", false)), &Seen::default(), &[]),
            "recorded"
        );
        assert_eq!(verdict(&posted, None, 0, None, &Seen::default(), &[]), "fail");
        let mut s = step(Via::Posted, Expect::Front);
        s.window_before = Some(false);
        let before = front(&[7], true);
        let o = open(Via::Posted, true, "", false);
        let out = Outcome {
            step: &s,
            setup: None,
            target: None,
            before: &before,
            clicked: None,
            clicks: 0,
            open: Some(&o),
            after: &before,
            toasts: &[],
        };
        assert_eq!(judge(&out).0, "unproven");
        let out = Outcome { setup: Some("no window"), ..out };
        assert_eq!(judge(&out).0, "unproven");
        let plain = step(Via::Panel, Expect::Front);
        let elsewhere = Seen::default();
        let out = Outcome { step: &plain, setup: None, target: Some("no row"), before: &elsewhere, ..out };
        assert_eq!(judge(&out).0, "fail");
    }

    #[test]
    fn plans_are_read_and_step_names_checked() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("plan.json");
        let plan = json!({
            "out": "/tmp/out.json", "sync": "/tmp/sync", "waitForRunning": ["main"],
            "steps": [{ "name": "S1", "account": "main", "via": "panel", "expect": "front", "windowBefore": true },
                      { "name": "S6", "account": "main", "via": "posted", "expect": "record" },
                      { "name": "S8", "account": "main", "via": "panel", "expect": "minimised" },
                      { "name": "S5", "account": "main", "via": "bubble", "expect": "visible-failure" }],
        });
        std::fs::write(&p, plan.to_string()).unwrap();
        let read = read_plan(&p).unwrap();
        assert_eq!(read.steps.len(), 4);
        assert_eq!(read.steps[2].expect, Expect::Minimised);
        assert_eq!(read.steps[3].expect, Expect::VisibleFailure);
        assert_eq!(read.steps[0].window_before, Some(true));
        let bad = json!({ "out": "/o", "sync": "/s", "steps": [{ "name": "../x", "account": "a", "via": "panel", "expect": "front" }] });
        std::fs::write(&p, bad.to_string()).unwrap();
        assert!(read_plan(&p).is_err());
    }
}
