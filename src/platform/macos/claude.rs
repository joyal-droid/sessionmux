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

//! Claude Desktop on macOS: find the app, start an account, bring its window forward, close it.
//!
//! - Claude is found by its bundle id through Launch Services, else in `/Applications` or
//!   `~/Applications`. Main keeps its data in `~/Library/Application Support/Claude`; every other
//!   account is a `--user-data-dir=` folder next to it.
//! - Accounts are started through Launch Services as a new instance of the app, so Claude is its
//!   own responsible process (its permission prompts are its own) and gets a clean environment.
//! - A second start of Claude on a data folder that already runs does not hand over to the
//!   running instance: it keeps running beside it. So Sessionmux never starts Claude on a folder
//!   that already runs, and a window that cannot be brought forward is never shown by starting
//!   its folder again.
//! - Bringing a window forward uses cooperative activation from the user's click or shortcut;
//!   activation does not un-minimise a window or switch Spaces, so when the user turned on
//!   "Bring minimised Claude windows to the front" and allowed Accessibility, the window is
//!   un-minimised and raised through Accessibility and activated again.
//! - Closing asks each instance to quit, waits up to 3 s, then ends what is left of each instance's
//!   process tree. Only Claude Desktop main processes are accepted, and other accounts' instances
//!   are never ended, whatever their parent is.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSRunningApplication, NSWorkspace, NSWorkspaceOpenConfiguration,
};
use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFRetained, CFString, CFType, CGSize};
use objc2_foundation::{NSArray, NSBundle, NSError, NSOperatingSystemVersion, NSProcessInfo, NSString, NSURL};

use super::process::{self, claude_mains, is_helper, pid_path, real_path, ClaudeExe, UDD};
use super::window::{front_window_owner, on_main_wait};
use crate::core::claude_app::{self, Located};
use crate::platform::{ClaudeInstall, Closer, Focus, FocusResult, Launcher, Os, Paths, Pid, Processes, Windows};
use crate::util::{json, log, paths};

/// Whether starting Claude again on a data folder that already runs hands over to the running
/// instance (which then shows its window or opens the link) and exits. It does not: the second
/// process keeps running beside the first. While this is false, nothing ever starts Claude on a
/// folder that already runs.
pub(crate) const RELAUNCH_HANDOFF: bool = false;

/// Claude Desktop's bundle id.
const CLAUDE_BUNDLE_ID: &str = "com.anthropic.claudefordesktop";
/// How long a start through Launch Services may take to report the new process.
const LAUNCH_WAIT: Duration = Duration::from_secs(10);
/// How long a polite quit may take before the rest of the process tree is ended.
const GRACEFUL_WAIT: Duration = Duration::from_secs(3);
/// How long ended processes may take to disappear.
const KILL_WAIT: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(100);
/// How long activation may take before the result is read.
const SETTLE: Duration = Duration::from_millis(300);
/// How long another thread waits for the main thread to pass activation on.
const MAIN_WAIT: Duration = Duration::from_secs(1);
/// How long one Accessibility request to Claude may take.
const AX_TIMEOUT_SECS: f32 = 1.0;
/// `kAXValueCGSizeType`.
const AX_VALUE_CG_SIZE: u32 = 2;
/// `kAXErrorSuccess`.
const AX_SUCCESS: i32 = 0;
/// `SZOMB`: the process has ended and waits for its parent to collect it.
const SZOMB: u32 = 5;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: Option<&CFDictionary>) -> u8;
    fn AXUIElementCreateApplication(pid: libc::pid_t) -> *mut CFType;
    fn AXUIElementSetMessagingTimeout(element: &CFType, timeout_secs: f32) -> i32;
    fn AXUIElementCopyAttributeValue(element: &CFType, attribute: &CFString, value: *mut *mut CFType) -> i32;
    fn AXUIElementSetAttributeValue(element: &CFType, attribute: &CFString, value: &CFType) -> i32;
    fn AXUIElementPerformAction(element: &CFType, action: &CFString) -> i32;
    fn AXValueGetValue(value: &CFType, value_type: u32, out: *mut std::ffi::c_void) -> u8;
    static kAXTrustedCheckOptionPrompt: &'static CFString;
}

fn macos_at_least(major: isize) -> bool {
    let v = NSOperatingSystemVersion { majorVersion: major, minorVersion: 0, patchVersion: 0 };
    NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(v)
}

fn ns_pid(pid: Pid) -> Option<libc::pid_t> {
    libc::pid_t::try_from(pid).ok().filter(|&p| p > 0)
}

// ───────────────────────────────────────────────────────────── locate ──

/// A string value of a bundle's Info.plist.
fn info_string(bundle: &NSBundle, key: &str) -> Option<String> {
    let v: Retained<AnyObject> = bundle.objectForInfoDictionaryKey(&NSString::from_str(key))?;
    v.downcast_ref::<NSString>().map(|s| s.to_string()).filter(|s| !s.trim().is_empty())
}

/// Claude Desktop installed at `bundle`, with Main's data folder in the user's Application
/// Support folder.
pub(crate) fn located_at(bundle: &Path) -> Result<Located, String> {
    let dir = real_path(&bundle.to_string_lossy());
    let ns = NSBundle::bundleWithPath(&NSString::from_str(&dir))
        .ok_or_else(|| format!("NSBundle: {dir} is not an app bundle"))?;
    let exe_name = info_string(&ns, "CFBundleExecutable").unwrap_or_else(|| "Claude".to_string());
    let exe = Path::new(&dir).join("Contents").join("MacOS").join(&exe_name);
    if !exe.is_file() {
        return Err(format!("Claude's program is missing: {}", exe.display()));
    }
    if let Some(v) = info_string(&ns, "CFBundleShortVersionString") {
        log::debug(&format!("claude_app: Claude {v} at {dir}"));
    }
    let main_data_dir = <Os as Paths>::config_root().join("Claude");
    let config = main_data_dir.join("claude_desktop_config.json");
    Ok(Located {
        kind: "bundle",
        exe: exe.to_string_lossy().into_owned(),
        install_dir: dir,
        aumid: None,
        family: None,
        main_data_dir: main_data_dir.to_string_lossy().into_owned(),
        main_config_candidates: vec![config.to_string_lossy().into_owned()],
    })
}

/// Where Claude may be installed, best first.
fn bundle_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let ws = NSWorkspace::sharedWorkspace();
    if let Some(url) = ws.URLForApplicationWithBundleIdentifier(&NSString::from_str(CLAUDE_BUNDLE_ID)) {
        if let Some(p) = url.path() {
            out.push(PathBuf::from(p.to_string()));
        }
    }
    out.push(PathBuf::from("/Applications/Claude.app"));
    out.push(<Os as Paths>::home().join("Applications").join("Claude.app"));
    out
}

// ───────────────────────────────────────────────────────────── launch ──

/// Start a new instance of the located Claude through Launch Services with `args`, and wait for
/// the system to report its pid.
fn open_instance(l: &Located, args: &[String]) -> Result<Pid, String> {
    if !Path::new(&l.install_dir).is_dir() || !Path::new(&l.exe).is_file() {
        return Err(format!("Claude Desktop was not found at {}.", l.install_dir));
    }
    let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&l.install_dir), true);
    let cfg = NSWorkspaceOpenConfiguration::configuration();
    // Always a new instance, also for Main: a plain open would bring forward whichever Claude runs.
    cfg.setCreatesNewApplicationInstance(true);
    cfg.setActivates(true);
    cfg.setAddsToRecentItems(false);
    cfg.setPromptsUserIfNeeded(true);
    let ns_args: Vec<Retained<NSString>> = args.iter().map(|a| NSString::from_str(a)).collect();
    cfg.setArguments(&NSArray::from_retained_slice(&ns_args));

    let (tx, rx) = mpsc::channel::<Result<Pid, String>>();
    let done = RcBlock::new(move |app: *mut NSRunningApplication, err: *mut NSError| {
        // SAFETY: Launch Services passes either a running app or an error, each valid for the
        // duration of this call (or null); neither reference is kept.
        let (app, err) = unsafe { (app.as_ref(), err.as_ref()) };
        let r = match (app, err) {
            (Some(a), _) => Pid::try_from(a.processIdentifier())
                .ok()
                .filter(|&p| p != 0)
                .ok_or_else(|| "Claude started without a process id".to_string()),
            (None, Some(e)) => {
                Err(format!("NSWorkspace openApplication: code {} ({})", e.code(), e.localizedDescription()))
            }
            (None, None) => Err("NSWorkspace openApplication: no result".to_string()),
        };
        let _ = tx.send(r);
    });
    NSWorkspace::sharedWorkspace().openApplicationAtURL_configuration_completionHandler(&url, &cfg, Some(&done));
    match rx.recv_timeout(LAUNCH_WAIT) {
        Ok(Ok(pid)) => Ok(pid),
        Ok(Err(e)) => Err(format!("Could not start Claude: {e}")),
        Err(_) => Err(format!("Claude did not start within {} s.", LAUNCH_WAIT.as_secs())),
    }
}

/// The main process already running on `data_dir` (None = Main), if any.
fn running_on(l: &Located, data_dir: Option<&str>) -> Option<Pid> {
    claude_mains(Some(l)).into_iter().find(|p| claude_app::matches_data_dir(p, data_dir)).map(|p| p.pid)
}

/// Refuse a start on a folder that already runs (a second instance would not hand over).
fn refuse_if_running(l: &Located, data_dir: Option<&str>) -> Result<(), String> {
    if RELAUNCH_HANDOFF {
        return Ok(());
    }
    match running_on(l, data_dir) {
        Some(pid) => {
            log::info(&format!("claude_app: not starting a second Claude next to pid {pid}"));
            Err("This account's Claude is already running. Click its Claude icon in the Dock to show its window."
                .to_string())
        }
        None => Ok(()),
    }
}

/// The checked `--user-data-dir=` argument for `data_dir`.
fn udd_arg(data_dir: &str) -> Result<String, String> {
    let d = data_dir.trim();
    let d = if d.len() > 1 { d.trim_end_matches('/') } else { d };
    if d.is_empty() || !d.starts_with('/') {
        return Err("launch: an absolute data folder is required".into());
    }
    Ok(format!("{UDD}{d}"))
}

// ───────────────────────────────────────────────────────────── focus ──

/// The steps of bringing a Claude window forward, so their order can be checked without a screen.
pub(crate) trait FocusSteps {
    /// Activate Sessionmux itself, inside the user's click or shortcut, so it holds activation it
    /// may pass on.
    fn activate_self(&mut self);
    /// Pass activation to `pid` and ask it to come forward with all its windows.
    fn pass_activation(&mut self, pid: Pid);
    /// Give the system time to act.
    fn settle(&mut self);
    /// True when `pid` is the app in front and has a window on screen.
    fn in_front(&mut self, pid: Pid) -> bool;
    /// True when the user turned on raising windows through Accessibility and allowed it.
    fn accessibility_allowed(&mut self) -> bool;
    /// Un-minimise and raise `pid`'s largest window through Accessibility.
    fn raise_with_accessibility(&mut self, pid: Pid) -> bool;
}

/// Bring `pid` forward: activation first; when that is not enough, the hand-off by a new start
/// (only where Claude hands a second start over), else the Accessibility raise when allowed.
pub(crate) fn focus_with(s: &mut impl FocusSteps, pid: Pid, handoff: bool) -> FocusResult {
    s.activate_self();
    s.pass_activation(pid);
    s.settle();
    if s.in_front(pid) {
        return FocusResult::Focused;
    }
    if handoff {
        return FocusResult::NeedsHandOff;
    }
    if s.accessibility_allowed() && s.raise_with_accessibility(pid) {
        s.pass_activation(pid);
        s.settle();
        if s.in_front(pid) {
            return FocusResult::Focused;
        }
    }
    FocusResult::Failed
}

/// The real focus steps: AppKit activation on the main thread, Accessibility from the caller's
/// thread.
struct SystemFocus;

impl FocusSteps for SystemFocus {
    fn activate_self(&mut self) {
        on_main_wait(MAIN_WAIT, |mtm| {
            let app = NSApplication::sharedApplication(mtm);
            if macos_at_least(14) {
                app.activate();
            } else {
                #[allow(deprecated)]
                app.activateIgnoringOtherApps(true);
            }
        });
    }

    fn pass_activation(&mut self, pid: Pid) {
        let Some(p) = ns_pid(pid) else { return };
        let sent = on_main_wait(MAIN_WAIT, move |mtm| {
            let target = NSRunningApplication::runningApplicationWithProcessIdentifier(p)?;
            if macos_at_least(14) {
                NSApplication::sharedApplication(mtm).yieldActivationToApplication(&target);
                let me = NSRunningApplication::currentApplication();
                Some(target.activateFromApplication_options(&me, NSApplicationActivationOptions::ActivateAllWindows))
            } else {
                #[allow(deprecated)]
                let opts = NSApplicationActivationOptions::ActivateAllWindows
                    | NSApplicationActivationOptions::ActivateIgnoringOtherApps;
                Some(target.activateWithOptions(opts))
            }
        })
        .flatten();
        log::debug(&format!("claude_app: activation of pid {pid} requested: {sent:?}"));
    }

    fn settle(&mut self) {
        std::thread::sleep(SETTLE);
    }

    fn in_front(&mut self, pid: Pid) -> bool {
        let front = front_window_owner() == Some(pid) || <Os as Windows>::foreground_pid() == Some(pid);
        front && <Os as Windows>::main_window(pid).is_some()
    }

    fn accessibility_allowed(&mut self) -> bool {
        raise_setting_on() && <Os as Focus>::accessibility_trusted(false) == Some(true)
    }

    fn raise_with_accessibility(&mut self, pid: Pid) -> bool {
        ax_raise(pid)
    }
}

/// The user's "Bring minimised Claude windows to the front" setting, read from the settings file.
fn raise_setting_on() -> bool {
    let v: Option<serde_json::Value> = json::read(&paths::store_file());
    v.and_then(|v| v.get("settings")?.get("raiseWindowsWithAccessibility")?.as_bool()).unwrap_or(false)
}

/// Take ownership of an object an Accessibility call returned with +1 (None for null).
fn owned(ptr: *mut CFType) -> Option<CFRetained<CFType>> {
    // SAFETY: the Create/Copy calls of Accessibility return a new reference the caller owns;
    // `CFRetained` releases it exactly once.
    NonNull::new(ptr).map(|p| unsafe { CFRetained::from_raw(p) })
}

/// An Accessibility attribute of `el`, owned.
fn ax_attr(el: &CFType, name: &str) -> Option<CFRetained<CFType>> {
    let mut value: *mut CFType = std::ptr::null_mut();
    // SAFETY: `el` is a live accessibility object, the name a live string, and `value` a writable
    // out-pointer that receives a new reference on success.
    let err = unsafe { AXUIElementCopyAttributeValue(el, &CFString::from_str(name), &mut value) };
    if err != AX_SUCCESS {
        return None;
    }
    owned(value)
}

/// The area of an Accessibility window, 0 when unknown.
fn ax_area(win: &CFType) -> f64 {
    let Some(v) = ax_attr(win, "AXSize") else { return 0.0 };
    let mut size = CGSize::default();
    // SAFETY: `v` is the window's size value; `size` is a writable `CGSize`, the type asked for.
    let ok = unsafe { AXValueGetValue(&v, AX_VALUE_CG_SIZE, (&mut size as *mut CGSize).cast()) };
    if ok != 0 {
        size.width * size.height
    } else {
        0.0
    }
}

/// Un-minimise `pid`'s largest window and raise it. True when the raise was accepted.
fn ax_raise(pid: Pid) -> bool {
    let Some(p) = ns_pid(pid) else { return false };
    // SAFETY: creating an accessibility object for a pid has no preconditions; it returns a new
    // reference, owned below.
    let Some(app) = owned(unsafe { AXUIElementCreateApplication(p) }) else { return false };
    // SAFETY: `app` is a live accessibility object; a failure only keeps the default timeout.
    unsafe { AXUIElementSetMessagingTimeout(&app, AX_TIMEOUT_SECS) };
    let Some(list) = ax_attr(&app, "AXWindows").and_then(|v| v.downcast::<CFArray>().ok()) else {
        log::debug(&format!("claude_app: no Accessibility windows for pid {pid}"));
        return false;
    };
    // SAFETY: the windows attribute is documented as an array of accessibility objects; they are
    // only passed back to Accessibility calls.
    let list: CFRetained<CFArray<CFType>> = unsafe { CFRetained::cast_unchecked(list) };
    let Some(win) = list.iter().map(|w| (ax_area(&w), w)).max_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, w)| w) else {
        return false;
    };
    let no: &CFType = CFBoolean::new(false);
    // SAFETY: `win` is a live accessibility object of Claude's, the names live strings and the
    // value a constant boolean.
    let unminimised = unsafe { AXUIElementSetAttributeValue(&win, &CFString::from_str("AXMinimized"), no) };
    // SAFETY: as above.
    let raised = unsafe { AXUIElementPerformAction(&win, &CFString::from_str("AXRaise")) };
    log::debug(&format!("claude_app: Accessibility raise of pid {pid}: unminimise {unminimised}, raise {raised}"));
    raised == AX_SUCCESS
}

// ───────────────────────────────────────────────────────────── close ──

/// When `pid` started, while it runs (an ended process waiting to be collected counts as gone).
fn running_since(pid: Pid) -> Option<u64> {
    let t = <Os as Processes>::start_time(pid)?;
    let zombie = ns_pid(pid).is_some_and(|p| {
        // SAFETY: `proc_bsdinfo` is plain old data; an all-zero value is valid and overwritten.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        // SAFETY: `info` is a live, writable record of exactly `size` bytes.
        let n = unsafe {
            libc::proc_pidinfo(p, libc::PROC_PIDTBSDINFO, 0, (&mut info as *mut libc::proc_bsdinfo).cast(), size)
        };
        n == size && info.pbi_status == SZOMB
    });
    (!zombie).then_some(t)
}

/// True while the process that started at `t` still runs under `pid`.
fn same_process(pid: Pid, t: u64) -> bool {
    running_since(pid) == Some(t)
}

/// Descendants of `root` in `table` (pid, parent, start time), deepest first, with their start
/// times. A child that started before its parent is skipped (the parent's pid was reused). Only
/// children for which `keep` is true are returned, and the walk stops at any other child, so
/// nothing reached through an unrelated process is ever included. `me` is never included.
pub(crate) fn descendants(
    root: Pid,
    table: &[(Pid, Pid, u64)],
    me: Pid,
    keep: &mut dyn FnMut(Pid) -> bool,
) -> Vec<(Pid, u64)> {
    let start: HashMap<Pid, u64> = table.iter().map(|&(p, _, t)| (p, t)).collect();
    let mut out = Vec::new();
    let mut seen: HashSet<Pid> = HashSet::from([root]);
    let mut queue = VecDeque::from([root]);
    while let Some(cur) = queue.pop_front() {
        let parent_t = start.get(&cur).copied();
        for &(pid, ppid, t) in table {
            if ppid != cur || pid == 0 || pid == me || seen.contains(&pid) {
                continue;
            }
            if parent_t.is_some_and(|pt| t < pt) {
                continue;
            }
            seen.insert(pid);
            if !keep(pid) {
                continue;
            }
            out.push((pid, t));
            queue.push_back(pid);
        }
    }
    out.reverse();
    out
}

/// The `.app` folder an executable belongs to (`…/X.app/Contents/MacOS/x` -> `…/X.app`).
fn bundle_of(exe: &str) -> Option<String> {
    let at = exe.rfind(".app/Contents/MacOS/")?;
    Some(exe[..at + 4].to_string())
}

/// True for an executable inside a Claude data folder (`Claude` or `Claude-*` in the profiles
/// folder), such as the command-line programs a Claude instance keeps there.
fn in_claude_data(exe: &str) -> bool {
    let root = <Os as Paths>::norm_path(&<Os as Paths>::profiles_root().to_string_lossy());
    let exe = <Os as Paths>::norm_path(exe);
    let Some(rest) = exe.strip_prefix(&root).and_then(|r| r.strip_prefix('/')) else { return false };
    let first = rest.split('/').next().unwrap_or("");
    first == "Claude" || first.starts_with("Claude-")
}

/// Ask `pid` to quit the way the Dock's Quit does; a process that is not an app gets SIGTERM.
fn ask_to_quit(pid: Pid) {
    let Some(p) = ns_pid(pid) else { return };
    match NSRunningApplication::runningApplicationWithProcessIdentifier(p) {
        Some(app) => {
            if !app.terminate() {
                log::debug(&format!("claude_app: quit request to pid {pid} was not sent"));
            }
        }
        None => {
            // SAFETY: sending SIGTERM to a pid checked as a Claude process just before.
            unsafe { libc::kill(p, libc::SIGTERM) };
        }
    }
}

fn kill_now(pid: Pid) {
    if let Some(p) = ns_pid(pid) {
        // SAFETY: the caller checked that `pid` still names the same process just before.
        if unsafe { libc::kill(p, libc::SIGKILL) } != 0 {
            log::debug(&format!("claude_app: ending pid {pid} failed: {}", std::io::Error::last_os_error()));
        }
    }
}

fn wait_gone(targets: &[(Pid, u64)], within: Duration) -> Vec<(Pid, u64)> {
    let deadline = Instant::now() + within;
    loop {
        let left: Vec<(Pid, u64)> = targets.iter().copied().filter(|&(p, t)| same_process(p, t)).collect();
        if left.is_empty() || Instant::now() >= deadline {
            return left;
        }
        std::thread::sleep(POLL);
    }
}

/// Close Claude main processes: a polite quit, up to 3 s, then the rest of each tree is ended.
fn close(pids: &[Pid]) -> Result<(), String> {
    let located = claude_app::locate().ok();
    let claude = ClaudeExe::new(located.as_ref());
    let me = std::process::id();

    let mut targets: Vec<(Pid, u64)> = Vec::new();
    let mut bundles: HashMap<Pid, String> = HashMap::new();
    for &pid in pids {
        if pid == 0 || pid == me || targets.iter().any(|t| t.0 == pid) {
            continue;
        }
        let Some(exe) = pid_path(pid) else { continue };
        if !claude.matches_any(&exe) || <Os as Processes>::argv(pid).is_some_and(|a| is_helper(&a)) {
            log::warn(&format!("claude_app: close skips pid {pid}, not a Claude Desktop main process"));
            continue;
        }
        let Some(t) = running_since(pid) else { continue };
        if let Some(b) = bundle_of(&exe) {
            bundles.insert(pid, b);
        }
        targets.push((pid, t));
    }
    if targets.is_empty() {
        return Ok(());
    }

    for &(pid, t) in &targets {
        if same_process(pid, t) {
            ask_to_quit(pid);
        }
    }
    let left = wait_gone(&targets, GRACEFUL_WAIT);
    if left.is_empty() {
        log::info(&format!("claude_app: closed {pids:?}"));
        return Ok(());
    }

    // Other accounts' instances are never part of this account's tree, whatever their parent is.
    let set: HashSet<Pid> = targets.iter().map(|t| t.0).collect();
    let other_mains: HashSet<Pid> =
        claude_mains(located.as_ref()).into_iter().map(|p| p.pid).filter(|p| !set.contains(p)).collect();
    let table = process::table();
    for &(pid, t) in &left {
        let bundle = bundles.get(&pid).cloned();
        let mut keep = |cpid: Pid| -> bool {
            if other_mains.contains(&cpid) {
                return false;
            }
            let exe = pid_path(cpid).unwrap_or_default();
            if claude.matches_any(&exe) && !<Os as Processes>::argv(cpid).is_some_and(|a| is_helper(&a)) {
                return false; // a Claude main process started later, by something in this tree
            }
            let ok = bundle.as_deref().is_some_and(|b| <Os as Paths>::is_under(&exe, b)) || in_claude_data(&exe);
            if !ok {
                log::debug(&format!("claude_app: close leaves pid {cpid} and what it started alone"));
            }
            ok
        };
        for (cpid, ct) in descendants(pid, &table, me, &mut keep) {
            if same_process(cpid, ct) {
                kill_now(cpid);
            }
        }
        if same_process(pid, t) {
            kill_now(pid);
        }
    }
    let failed = wait_gone(&left, KILL_WAIT);
    if failed.is_empty() {
        log::info(&format!("claude_app: closed {pids:?} (ended {} after the polite quit)", left.len()));
        Ok(())
    } else {
        let ids: Vec<Pid> = failed.iter().map(|f| f.0).collect();
        Err(format!("Could not close Claude process(es) {ids:?}"))
    }
}

impl ClaudeInstall for Os {
    /// By bundle id through Launch Services, else `/Applications`, else `~/Applications`.
    fn locate() -> Result<Located, String> {
        let mut last = None;
        for b in bundle_candidates() {
            if !b.is_dir() {
                continue;
            }
            match located_at(&b) {
                Ok(l) => return Ok(l),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| "Claude Desktop is not installed (no Claude.app found).".to_string()))
    }
}

impl Focus for Os {
    fn focus(pid: Pid) -> FocusResult {
        if !<Os as Processes>::alive(pid) {
            return FocusResult::Failed;
        }
        let r = focus_with(&mut SystemFocus, pid, RELAUNCH_HANDOFF);
        log::debug(&format!("claude_app: focus pid {pid}: {r:?}"));
        r
    }

    /// Whether Accessibility is allowed for Sessionmux; with `prompt`, the system shows its grant
    /// prompt when it is not.
    fn accessibility_trusted(prompt: bool) -> Option<bool> {
        if !prompt {
            // SAFETY: a plain query without arguments.
            return Some(unsafe { AXIsProcessTrusted() } != 0);
        }
        // SAFETY: the option key is a constant string exported by the system.
        let key: &CFString = unsafe { kAXTrustedCheckOptionPrompt };
        let yes: &CFType = CFBoolean::new(true);
        let opts = CFDictionary::<CFString, CFType>::from_slices(&[key], &[yes]);
        // SAFETY: `opts` is a live dictionary with the documented key and a boolean value.
        let trusted = unsafe { AXIsProcessTrustedWithOptions(Some(opts.as_opaque())) } != 0;
        Some(trusted)
    }
}

impl Launcher for Os {
    /// A new instance with no data folder; refused while Main already runs.
    fn launch_main(l: &Located) -> Result<Option<Pid>, String> {
        refuse_if_running(l, None)?;
        let pid = open_instance(l, &[])?;
        log::info(&format!("claude_app: launched Main, pid {pid}"));
        Ok(Some(pid))
    }

    /// A new instance on `data_dir`; refused while that folder already runs.
    fn launch(l: &Located, data_dir: &str) -> Result<Pid, String> {
        let arg = udd_arg(data_dir)?;
        let dir = &arg[UDD.len()..];
        refuse_if_running(l, Some(dir))?;
        let pid = open_instance(l, std::slice::from_ref(&arg))?;
        log::info(&format!("claude_app: launched pid {pid} for {dir}"));
        Ok(pid)
    }

    /// Only where a second start hands its link to the running instance of the same folder;
    /// otherwise the link would open in a second instance, so this is refused.
    fn launch_url(l: &Located, data_dir: Option<&str>, url: &str) -> Result<(), String> {
        if !RELAUNCH_HANDOFF {
            return Err("Claude on macOS does not pass a link from a new start to the running Claude.".into());
        }
        if url.trim().is_empty() {
            return Err("open url: the link must be non-empty".into());
        }
        let mut args = Vec::new();
        if let Some(d) = data_dir.filter(|d| !d.trim().is_empty()) {
            args.push(udd_arg(d)?);
        }
        args.push(url.to_string());
        open_instance(l, &args).map(|_| ())
    }
}

impl Closer for Os {
    /// Quit each instance like the Dock's Quit, wait up to 3 s, then end what is left of its
    /// process tree.
    fn close(pids: &[Pid]) -> Result<(), String> {
        close(pids)
    }
}

/// A stand-in Claude.app for tests, built from `tests/fixtures/macos/fakeclaude.c` with the
/// system C compiler.
#[cfg(test)]
pub(crate) mod fake {
    use std::path::PathBuf;
    use std::process::Command;

    const SOURCE: &str = include_str!("../../../tests/fixtures/macos/fakeclaude.c");
    /// The bundle id of the stand-in, so the system never mistakes it for the real Claude.
    pub(crate) const BUNDLE_ID: &str = "test.sessionmux.fakeclaude";
    /// A link scheme the stand-in declares, for link-handler tests.
    pub(crate) const SCHEME: &str = "sessionmux-test";

    pub(crate) struct FakeClaude {
        /// The folder holding `Claude.app` and the files the stand-in writes.
        pub(crate) root: PathBuf,
        pub(crate) bundle: PathBuf,
        _dir: tempfile::TempDir,
    }

    fn plist() -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>Claude</string>
  <key>CFBundleIdentifier</key><string>{BUNDLE_ID}</string>
  <key>CFBundleName</key><string>Claude</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.0.1</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>CFBundleURLTypes</key>
  <array><dict>
    <key>CFBundleURLName</key><string>Sessionmux test link</string>
    <key>CFBundleURLSchemes</key><array><string>{SCHEME}</string></array>
  </dict></array>
</dict>
</plist>
"#
        )
    }

    /// Build the stand-in in a new temporary folder. Err(reason) when it cannot be built here
    /// (no C compiler); the caller skips its test with that reason.
    pub(crate) fn build() -> Result<FakeClaude, String> {
        if !Command::new("cc").arg("--version").output().is_ok_and(|o| o.status.success()) {
            return Err("no C compiler (cc) on this machine".into());
        }
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let root = dir.path().canonicalize().map_err(|e| e.to_string())?;
        let bundle = root.join("Claude.app");
        let macos = bundle.join("Contents").join("MacOS");
        std::fs::create_dir_all(&macos).map_err(|e| e.to_string())?;
        std::fs::write(bundle.join("Contents").join("Info.plist"), plist()).map_err(|e| e.to_string())?;
        let src = root.join("fakeclaude.c");
        std::fs::write(&src, SOURCE).map_err(|e| e.to_string())?;
        let out = Command::new("cc")
            .args(["-Wall", "-O0", "-o"])
            .arg(macos.join("Claude"))
            .arg(&src)
            .args(["-framework", "AppKit", "-lobjc"])
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            panic!("the stand-in Claude did not compile:\n{}", String::from_utf8_lossy(&out.stderr));
        }
        Ok(FakeClaude { root, bundle, _dir: dir })
    }

    impl FakeClaude {
        /// The arguments main process `pid` recorded, once it has.
        pub(crate) fn recorded_args(&self, pid: u32) -> Option<Vec<String>> {
            let s = std::fs::read_to_string(self.root.join(format!("argv-{pid}.txt"))).ok()?;
            Some(s.lines().map(str::to_string).collect())
        }

        /// The links main process `pid` was asked to open.
        #[allow(dead_code)]
        pub(crate) fn recorded_urls(&self, pid: u32) -> Vec<String> {
            std::fs::read_to_string(self.root.join(format!("url-{pid}.txt")))
                .map(|s| s.lines().map(str::to_string).collect())
                .unwrap_or_default()
        }

        /// A data folder next to the bundle.
        pub(crate) fn data_dir(&self, name: &str) -> PathBuf {
            let d = self.root.join(name);
            let _ = std::fs::create_dir_all(&d);
            d
        }
    }

    /// Wait up to `secs` for `f` to hold.
    pub(crate) fn wait_for(secs: u64, mut f: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        loop {
            if f() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{self, wait_for};
    use super::*;

    // ── focus order ──

    #[derive(Default)]
    struct Script {
        steps: Vec<String>,
        /// `in_front` answers, in order; false once used up.
        front: VecDeque<bool>,
        ax_allowed: bool,
        ax_raised: bool,
    }

    impl FocusSteps for Script {
        fn activate_self(&mut self) {
            self.steps.push("activate-self".into());
        }
        fn pass_activation(&mut self, pid: Pid) {
            self.steps.push(format!("pass-{pid}"));
        }
        fn settle(&mut self) {
            self.steps.push("settle".into());
        }
        fn in_front(&mut self, pid: Pid) -> bool {
            self.steps.push(format!("front?-{pid}"));
            self.front.pop_front().unwrap_or(false)
        }
        fn accessibility_allowed(&mut self) -> bool {
            self.steps.push("ax-allowed?".into());
            self.ax_allowed
        }
        fn raise_with_accessibility(&mut self, pid: Pid) -> bool {
            self.steps.push(format!("ax-raise-{pid}"));
            self.ax_raised
        }
    }

    fn script(front: &[bool], ax_allowed: bool, ax_raised: bool) -> Script {
        Script { front: front.iter().copied().collect(), ax_allowed, ax_raised, ..Script::default() }
    }

    #[test]
    fn activation_alone_is_enough_for_a_visible_window() {
        let mut s = script(&[true], true, true);
        assert_eq!(focus_with(&mut s, 7, false), FocusResult::Focused);
        assert_eq!(s.steps, ["activate-self", "pass-7", "settle", "front?-7"]);
    }

    #[test]
    fn hand_off_comes_before_accessibility_where_claude_hands_over() {
        let mut s = script(&[false], true, true);
        assert_eq!(focus_with(&mut s, 7, true), FocusResult::NeedsHandOff);
        assert_eq!(s.steps, ["activate-self", "pass-7", "settle", "front?-7"]);
    }

    #[test]
    fn accessibility_raise_then_activation_again() {
        let mut s = script(&[false, true], true, true);
        assert_eq!(focus_with(&mut s, 7, false), FocusResult::Focused);
        assert_eq!(
            s.steps,
            [
                "activate-self",
                "pass-7",
                "settle",
                "front?-7",
                "ax-allowed?",
                "ax-raise-7",
                "pass-7",
                "settle",
                "front?-7"
            ]
        );
        let mut s = script(&[false, false], true, true);
        assert_eq!(focus_with(&mut s, 7, false), FocusResult::Failed);
    }

    #[test]
    fn without_accessibility_the_result_is_a_failure_notice() {
        let mut s = script(&[false], false, true);
        assert_eq!(focus_with(&mut s, 7, false), FocusResult::Failed);
        assert_eq!(s.steps, ["activate-self", "pass-7", "settle", "front?-7", "ax-allowed?"]);
        let mut s = script(&[false], true, false);
        assert_eq!(focus_with(&mut s, 7, false), FocusResult::Failed);
        assert_eq!(s.steps.last().map(String::as_str), Some("ax-raise-7"));
    }

    #[test]
    fn the_real_focus_never_hands_off() {
        if RELAUNCH_HANDOFF {
            return;
        }
        assert_ne!(Os::focus(u32::MAX), FocusResult::NeedsHandOff);
        assert_eq!(Os::focus(0), FocusResult::Failed);
    }

    // ── pure helpers ──

    #[test]
    fn data_folder_argument() {
        assert_eq!(udd_arg("/a/Claude-X/").unwrap(), "--user-data-dir=/a/Claude-X");
        assert_eq!(udd_arg("  /a b/Claude-X ").unwrap(), "--user-data-dir=/a b/Claude-X");
        assert!(udd_arg("").is_err());
        assert!(udd_arg("relative/Claude-X").is_err());
    }

    #[test]
    fn bundle_and_data_folders() {
        assert_eq!(bundle_of("/A/Claude.app/Contents/MacOS/Claude").as_deref(), Some("/A/Claude.app"));
        assert_eq!(
            bundle_of("/A/Claude.app/Contents/Frameworks/H.app/Contents/MacOS/H").as_deref(),
            Some("/A/Claude.app/Contents/Frameworks/H.app")
        );
        assert_eq!(bundle_of("/usr/bin/node"), None);
        let root = <Os as Paths>::profiles_root();
        assert!(in_claude_data(&root.join("Claude/claude-code/2.0/claude").to_string_lossy()));
        assert!(in_claude_data(&root.join("Claude-Work/x").to_string_lossy()));
        assert!(!in_claude_data(&root.join("Other/x").to_string_lossy()));
        assert!(!in_claude_data("/usr/local/bin/node"));
    }

    #[test]
    fn tree_walk_guards_against_pid_reuse() {
        // pid, parent, start
        let table = [
            (10, 1, 100),
            (11, 10, 110),
            (12, 11, 120),
            (13, 10, 90), // older than its "parent": that pid was reused
            (14, 10, 130),
            (15, 14, 140), // below a process that is not kept
            (16, 10, 150), // ourselves
        ];
        let mut keep = |p: Pid| p != 14;
        let got = descendants(10, &table, 16, &mut keep);
        assert_eq!(got, vec![(12, 120), (11, 110)]);
        assert!(descendants(99, &table, 0, &mut |_| true).is_empty());
    }

    #[test]
    fn starting_on_a_running_folder_is_refused_by_design() {
        if RELAUNCH_HANDOFF {
            return;
        }
        let l = Located { exe: "/nowhere/Claude.app/Contents/MacOS/Claude".into(), ..Located::default() };
        assert!(Os::launch_url(&l, None, "claude://x").is_err());
        assert!(Os::launch_url(&l, Some("/tmp/Claude-X"), "claude://x").is_err());
    }

    #[test]
    fn a_missing_install_is_not_started() {
        let l = Located {
            exe: "/nowhere/Claude.app/Contents/MacOS/Claude".into(),
            install_dir: "/nowhere/Claude.app".into(),
            ..Located::default()
        };
        assert!(Os::launch(&l, "/tmp/Claude-X").is_err());
        assert!(Os::launch_main(&l).is_err());
        assert!(located_at(Path::new("/nowhere/Claude.app")).is_err());
    }

    #[test]
    fn locating_claude_never_fails_badly() {
        // Claude may or may not be installed on this machine.
        if let Ok(l) = <Os as ClaudeInstall>::locate() {
            assert_eq!(l.kind, "bundle");
            assert!(Path::new(&l.exe).is_file());
            assert!(l.main_data_dir.ends_with("Library/Application Support/Claude"));
        }
    }

    #[test]
    fn accessibility_query_answers() {
        assert!(Os::accessibility_trusted(false).is_some());
    }

    #[test]
    fn close_leaves_other_programs_alone() {
        let mut child = std::process::Command::new("/bin/sh").args(["-c", "sleep 30; :"]).spawn().unwrap();
        let pid = child.id();
        assert!(Os::close(&[pid, 0, std::process::id()]).is_ok());
        assert!(Os::alive(pid), "a program that is not Claude Desktop is never closed");
        let _ = child.kill();
        let _ = child.wait();
    }

    // ── the stand-in Claude ──

    /// Main processes and helpers of the stand-in running on `dir`.
    fn children_of(pid: Pid) -> Vec<Pid> {
        process::table().into_iter().filter(|&(_, pp, _)| pp == pid).map(|(p, _, _)| p).collect()
    }

    fn gone(pid: Pid, t: u64) -> bool {
        !same_process(pid, t)
    }

    #[test]
    fn stand_in_claude_starts_lists_and_quits() {
        let f = match fake::build() {
            Ok(f) => f,
            Err(why) => {
                println!("skipped: {why}");
                return;
            }
        };
        let l = located_at(&f.bundle).expect("locate the stand-in");
        assert_eq!(l.kind, "bundle");
        assert!(l.exe.ends_with("Claude.app/Contents/MacOS/Claude"));
        let dir = f.data_dir("Claude-Test");
        let dir_s = dir.to_string_lossy().into_owned();

        let pid = Os::launch(&l, &dir_s).expect("start through Launch Services");
        assert!(Os::alive(pid));
        assert!(wait_for(10, || f.recorded_args(pid).is_some()), "the stand-in recorded its arguments");
        let recorded = f.recorded_args(pid).unwrap();
        assert!(recorded.contains(&format!("--user-data-dir={dir_s}")), "{recorded:?}");
        assert_eq!(Os::argv(pid), Some(recorded.clone()));
        assert!(wait_for(10, || children_of(pid).len() >= 2), "helpers started");
        let helpers = children_of(pid);
        let t = Os::start_time(pid).expect("start time");

        let listed = Os::list_claude(Some(&l));
        let mine: Vec<_> = listed.iter().filter(|p| claude_app::matches_data_dir(p, Some(&dir_s))).collect();
        assert_eq!(mine.len(), 1, "{listed:?}");
        assert_eq!(mine[0].pid, pid);
        assert!(listed.iter().all(|p| !helpers.contains(&p.pid)), "helpers are not listed");
        for h in &helpers {
            assert!(Os::argv(*h).is_some_and(|a| is_helper(&a)));
            assert_eq!(Os::parent(*h), Some(pid));
        }
        assert_eq!(Os::start_time(pid), Some(t));

        if super::super::window::on_screen().is_empty() {
            println!("no window server session: window lookup not checked");
        } else {
            assert!(wait_for(10, || Os::main_window(pid).is_some()), "the stand-in's window is found");
        }

        // No second instance on a running folder.
        if !RELAUNCH_HANDOFF {
            assert!(Os::launch(&l, &dir_s).is_err());
            std::thread::sleep(Duration::from_millis(500));
            let again = Os::list_claude(Some(&l));
            assert_eq!(again.iter().filter(|p| claude_app::matches_data_dir(p, Some(&dir_s))).count(), 1);
        }

        let started = Instant::now();
        Os::close(&[pid]).expect("close");
        assert!(gone(pid, t));
        println!("polite quit took {} ms", started.elapsed().as_millis());
        assert!(wait_for(5, || helpers.iter().all(|&h| !Os::alive(h) || Os::parent(h) != Some(pid))));
        assert!(wait_for(5, || helpers.iter().all(|&h| !Os::alive(h))), "helpers ended with their app");
    }

    #[test]
    fn stand_in_main_account_starts_once() {
        let f = match fake::build() {
            Ok(f) => f,
            Err(why) => {
                println!("skipped: {why}");
                return;
            }
        };
        let l = located_at(&f.bundle).unwrap();
        let pid = Os::launch_main(&l).expect("start Main").expect("a pid");
        assert!(wait_for(10, || f.recorded_args(pid).is_some()));
        let listed = Os::list_claude(Some(&l));
        let p = listed.iter().find(|p| p.pid == pid).expect("Main is listed");
        assert_eq!(p.data_dir, None);
        assert!(claude_app::matches_data_dir(p, None));
        if !RELAUNCH_HANDOFF {
            assert!(Os::launch_main(&l).is_err(), "Main is not started twice");
        }
        Os::close(&[pid]).expect("close");
        assert!(!Os::alive(pid) || Os::start_time(pid).is_none());
    }

    #[test]
    fn stand_in_that_ignores_quit_has_its_tree_ended() {
        let f = match fake::build() {
            Ok(f) => f,
            Err(why) => {
                println!("skipped: {why}");
                return;
            }
        };
        let l = located_at(&f.bundle).unwrap();
        let dir = f.data_dir("Claude-Stubborn");
        std::fs::write(dir.join("ignore-quit"), b"").unwrap();
        let pid = Os::launch(&l, &dir.to_string_lossy()).expect("start");
        assert!(wait_for(10, || children_of(pid).len() >= 2));
        let t = Os::start_time(pid).unwrap();
        let helpers: Vec<(Pid, u64)> =
            children_of(pid).into_iter().filter_map(|h| Os::start_time(h).map(|ht| (h, ht))).collect();

        let started = Instant::now();
        Os::close(&[pid]).expect("close");
        assert!(started.elapsed() >= GRACEFUL_WAIT, "the polite quit was given its time");
        assert!(gone(pid, t));
        for (h, ht) in helpers {
            assert!(wait_for(2, || gone(h, ht)), "helper {h} was ended with its app");
        }
    }
}
