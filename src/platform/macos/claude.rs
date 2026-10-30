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
//!   `~/Applications`; a copy Launch Services names on a mounted disk image, in the Trash or
//!   in a translocated folder comes after those two. Main keeps its data in `~/Library/Application Support/Claude`; every other
//!   account is a `--user-data-dir=` folder next to it.
//! - Accounts are started through Launch Services as a new instance of the app, so Claude is its
//!   own responsible process (its permission prompts are its own) and gets a clean environment.
//! - A second start of Claude on a data folder that already runs does not hand over to the
//!   running instance: it keeps running beside it. So Sessionmux never starts Claude on a folder
//!   that already runs, and a window that cannot be brought forward is never shown by starting
//!   its folder again.
//! - Bringing a window forward uses cooperative activation: the user's click, menu choice or
//!   shortcut makes Sessionmux the active app inside that event, and the focus worker waits
//!   briefly for that to complete before it yields the activation to Claude. Activation does
//!   not un-minimise a window or switch Spaces, so when the user turned on "Bring minimised
//!   Claude windows to the front" and allowed Accessibility, the window is un-minimised, raised
//!   and made main through Accessibility, the app is put in front, and activated again.
//! - An instance with no window on screen is asked to show itself the way a click on its Dock
//!   icon does (the reopen Apple Event, sent to its process only; the only instance of a copy of
//!   Claude is reached through Launch Services instead, which needs no Automation permission).
//!   Claude then opens its window again. That never starts another instance.
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
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSRunningApplication, NSWorkspace, NSWorkspaceOpenConfiguration,
};
use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFRetained, CFString, CFType, CGSize};
use objc2_foundation::{NSArray, NSBundle, NSError, NSOperatingSystemVersion, NSProcessInfo, NSString, NSURL};

use super::process::{self, any_claude_mains, claude_mains, is_helper, pid_path, real_path, ClaudeExe, UDD};
use super::url_handler::{
    send_event_to_pid, AeSendError, AE_REOPEN_APPLICATION, CORE_EVENT_CLASS, ERR_NEEDS_CONSENT, ERR_NOT_PERMITTED,
};
use super::window::{app_hidden, front_window_owner, has_off_screen_window, on_main_wait};
use crate::core::claude_app::{self, Located};
use crate::platform::{
    ClaudeInstall, Closer, Focus, FocusFailure, FocusResult, Launcher, Os, Paths, Pid, Processes, Windows,
};
use crate::util::{json, log, paths};

/// Whether starting Claude again on a data folder that already runs hands over to the running
/// instance (which then shows its window or opens the link) and exits. It does not, whether the
/// second start goes through Launch Services or runs the executable directly: the second process
/// keeps running beside the first on the same folder. While this is false, nothing ever starts
/// Claude on a folder that already runs.
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
/// How long passing activation on waits for Sessionmux to be the active app.
const ACTIVE_WAIT: Duration = Duration::from_millis(500);
/// How often that is checked, and how long one check may wait for the main thread.
const ACTIVE_POLL: Duration = Duration::from_millis(25);
const ACTIVE_ASK_WAIT: Duration = Duration::from_millis(250);
/// How long a minimised window may take to leave the screen.
const MINIMISE_WAIT: Duration = Duration::from_millis(1000);
/// How long an instance asked to show itself may take to put a window on screen.
const REOPEN_WAIT: Duration = Duration::from_millis(1500);
/// How long Launch Services may take to reach a running instance.
const REOPEN_LAUNCH_WAIT: Duration = Duration::from_secs(5);
/// How long one Accessibility request to Claude may take.
const AX_TIMEOUT_SECS: f32 = 1.0;
/// `kAXValueCGSizeType`.
const AX_VALUE_CG_SIZE: u32 = 2;
/// `kAXErrorSuccess`.
const AX_SUCCESS: i32 = 0;
/// `kAXErrorNoValue`: the attribute has no value.
const AX_NO_VALUE: i32 = -25212;
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

/// True for a copy of an app that is not where apps are installed: on a mounted disk image, in
/// the Trash, or run from a translocated (quarantined, read-only) folder.
fn is_transient_copy(p: &Path) -> bool {
    let s = p.to_string_lossy();
    s.starts_with("/Volumes/") || s.contains("/.Trash/") || s.contains("/AppTranslocation/")
}

/// Where Claude may be installed, best first: the copy Launch Services names for the bundle id,
/// then `/Applications`, then `~/Applications`; a transient copy named by Launch Services goes
/// last.
fn order_candidates(registered: Option<PathBuf>, home: &Path) -> Vec<PathBuf> {
    let fixed = [PathBuf::from("/Applications/Claude.app"), home.join("Applications").join("Claude.app")];
    let mut out = Vec::new();
    match registered {
        Some(r) if is_transient_copy(&r) => {
            out.extend(fixed);
            out.push(r);
        }
        Some(r) => {
            out.push(r);
            out.extend(fixed);
        }
        None => out.extend(fixed),
    }
    out
}

/// Where Claude may be installed, best first.
fn bundle_candidates() -> Vec<PathBuf> {
    let ws = NSWorkspace::sharedWorkspace();
    let registered = ws
        .URLForApplicationWithBundleIdentifier(&NSString::from_str(CLAUDE_BUNDLE_ID))
        .and_then(|url| url.path())
        .map(|p| PathBuf::from(p.to_string()));
    order_candidates(registered, &<Os as Paths>::home())
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
    open_app(&url, &cfg, LAUNCH_WAIT)
}

/// Open the app at `url` through Launch Services with `cfg`, and wait up to `wait` for the
/// system to report the process it reached.
fn open_app(url: &NSURL, cfg: &NSWorkspaceOpenConfiguration, wait: Duration) -> Result<Pid, String> {
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
    NSWorkspace::sharedWorkspace().openApplicationAtURL_configuration_completionHandler(url, cfg, Some(&done));
    match rx.recv_timeout(wait) {
        Ok(Ok(pid)) => Ok(pid),
        Ok(Err(e)) => Err(format!("Could not start Claude: {e}")),
        Err(_) => Err(format!("Claude did not start within {} s.", wait.as_secs())),
    }
}

/// The main process already running on `data_dir` (None = Main), if any.
/// Instances started from any copy of Claude.app count: a second copy on the same folder would
/// not hand over either.
fn running_on(l: &Located, data_dir: Option<&str>) -> Option<Pid> {
    any_claude_mains(Some(l)).into_iter().find(|p| claude_app::matches_data_dir(p, data_dir)).map(|p| p.pid)
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

/// Whether the opt-in Accessibility raise may be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AxState {
    Allowed,
    /// The user did not turn on "Bring minimised Claude windows to the front".
    Off,
    /// The setting is on, but the system does not trust Sessionmux for Accessibility.
    Untrusted,
}

impl AxState {
    fn name(self) -> &'static str {
        match self {
            AxState::Allowed => "allowed",
            AxState::Off => "setting-off",
            AxState::Untrusted => "untrusted",
        }
    }
}

/// Where a process's windows are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WinState {
    /// A normal window is on screen.
    OnScreen,
    /// It has a normal window, but none on screen (minimised, on another Space, ordered out).
    OffScreen,
    /// It has no normal window at all (closed).
    None,
}

impl WinState {
    fn name(self) -> &'static str {
        match self {
            WinState::OnScreen => "on-screen",
            WinState::OffScreen => "off-screen",
            WinState::None => "none",
        }
    }
}

/// Outcome of asking a running instance to show itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reopen {
    /// The request reached the process.
    Sent,
    /// The user did not allow Sessionmux to control Claude (Automation).
    Denied,
    /// The process did not answer in time (stopped, hung or busy).
    NoAnswer,
    Failed,
}

/// The steps of bringing a Claude window forward, so their order can be checked without a screen.
pub(crate) trait FocusSteps {
    /// Activate Sessionmux on the main thread, so it holds activation it may pass on. Nothing on
    /// other threads: the click, menu choice or shortcut took the activation already.
    fn activate_self(&mut self);
    /// Show `pid` again when it is hidden (as Hide in its menu leaves it). True when it was
    /// hidden.
    fn unhide(&mut self, pid: Pid) -> bool;
    /// Pass activation to `pid` and ask it to come forward with all its windows.
    fn pass_activation(&mut self, pid: Pid);
    /// Give the system time to act.
    fn settle(&mut self);
    /// True when `pid` is the app in front and has a window on screen.
    fn in_front(&mut self, pid: Pid) -> bool;
    /// Whether raising windows through Accessibility is turned on and allowed.
    fn accessibility(&mut self) -> AxState;
    /// Un-minimise and raise `pid`'s largest minimised window (else its largest) through
    /// Accessibility and make it the main window of the app in front.
    fn raise_with_accessibility(&mut self, pid: Pid) -> bool;
    /// Where `pid`'s windows are.
    fn window_state(&mut self, pid: Pid) -> WinState;
    /// Ask `pid` to show itself, as a click on its Dock icon does; it opens a window when it has
    /// none.
    fn reopen(&mut self, pid: Pid) -> Reopen;
    /// Wait a little for `pid` to put a window on screen. True when it did.
    fn wait_for_window(&mut self, pid: Pid) -> bool;
}

/// Bring `pid` forward: activation first; when that is not enough, the hand-off by a new start
/// (only where Claude hands a second start over), else the Accessibility raise when allowed,
/// else, when no window of it is on screen, a request to the instance to show itself.
pub(crate) fn focus_with(s: &mut impl FocusSteps, pid: Pid, handoff: bool) -> FocusResult {
    s.activate_self();
    s.unhide(pid);
    s.pass_activation(pid);
    s.settle();
    if s.in_front(pid) {
        return FocusResult::Focused;
    }
    if handoff {
        return FocusResult::NeedsHandOff;
    }
    let ax = s.accessibility();
    if ax == AxState::Allowed && s.raise_with_accessibility(pid) {
        s.pass_activation(pid);
        s.settle();
        if s.in_front(pid) {
            return FocusResult::Focused;
        }
    }
    let mut reopen = None;
    if s.window_state(pid) != WinState::OnScreen {
        let r = s.reopen(pid);
        reopen = Some(r);
        if r == Reopen::Sent && s.wait_for_window(pid) {
            if s.in_front(pid) {
                return FocusResult::Focused;
            }
            if ax == AxState::Allowed {
                s.raise_with_accessibility(pid);
            }
            s.pass_activation(pid);
            s.settle();
            if s.in_front(pid) {
                return FocusResult::Focused;
            }
        }
    }
    FocusResult::Failed(failure(ax, reopen, s.window_state(pid)))
}

/// The steps of putting away the window in front, so their order can be checked without a screen.
pub(crate) trait MinimiseSteps {
    /// Whether minimising windows through Accessibility is turned on and allowed.
    fn accessibility(&mut self) -> AxState;
    /// Minimise `pid`'s window in front through Accessibility. True when that was accepted.
    fn minimise_with_accessibility(&mut self, pid: Pid) -> bool;
    /// Wait a little for `pid` to have no window on screen. True when it has none.
    fn wait_until_gone(&mut self, pid: Pid) -> bool;
}

/// Put away `pid`'s window that is in front: minimised to the Dock through Accessibility, only
/// when raising through it is allowed (so the next click can un-minimise it). Without that,
/// nothing is tried: macOS refuses to hide another app for Sessionmux, and the attempt can move
/// the activation to a third app, so the click then brings the window forward as before.
pub(crate) fn minimise_with(s: &mut impl MinimiseSteps, pid: Pid) -> FocusResult {
    if s.accessibility() == AxState::Allowed && s.minimise_with_accessibility(pid) && s.wait_until_gone(pid) {
        return FocusResult::Minimised;
    }
    FocusResult::Failed(FocusFailure::Refused)
}

/// Why a window did not come forward, from what was tried and where its windows are now.
/// A refused or unanswered reopen request explains a window that is still not on screen before
/// any setting does: neither Accessibility setting can open a closed window.
fn failure(ax: AxState, reopen: Option<Reopen>, win: WinState) -> FocusFailure {
    match (win, ax, reopen) {
        (WinState::OffScreen | WinState::None, _, Some(Reopen::Denied)) => FocusFailure::AutomationDenied,
        (WinState::OffScreen | WinState::None, _, Some(Reopen::NoAnswer)) => FocusFailure::NotResponding,
        (WinState::OnScreen | WinState::OffScreen, AxState::Untrusted, _) => FocusFailure::AccessibilityUntrusted,
        (WinState::OnScreen, _, _) => FocusFailure::Refused,
        (WinState::OffScreen, AxState::Off, _) => FocusFailure::AccessibilityOff,
        (WinState::OffScreen, _, _) => FocusFailure::Refused,
        (WinState::None, _, _) => FocusFailure::NoWindow,
    }
}

/// Make Sessionmux the active app, when it is not. Called from the handler of the user's click,
/// menu choice or shortcut: there the system grants it, and Sessionmux can then pass the
/// activation on to Claude. On macOS 14 and later the request completes shortly after.
pub(crate) fn take_activation(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    if app.isActive() {
        return;
    }
    if macos_at_least(14) {
        app.activate();
    } else {
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }
}

/// What one focus attempt saw, for its log line.
#[derive(Default)]
struct Report {
    activation: Vec<String>,
    ax: Vec<String>,
    reopen: Option<String>,
}

impl Report {
    fn line(&self) -> String {
        let list = |v: &[String]| if v.is_empty() { "n/a".to_string() } else { v.join(", ") };
        format!(
            "activation=[{}] ax=[{}] reopen={}",
            list(&self.activation),
            list(&self.ax),
            self.reopen.as_deref().unwrap_or("n/a")
        )
    }
}

/// The real focus steps: AppKit activation on the main thread, Accessibility and the reopen
/// request from the caller's thread.
#[derive(Default)]
struct SystemFocus {
    report: Report,
}

/// Whether Sessionmux is the active app, polled from another thread for up to [`ACTIVE_WAIT`]
/// (the activation taken in the user's click completes shortly after it). Returns the answer
/// and how long the wait took; None when the main thread does not answer.
fn wait_until_active() -> (Option<bool>, Duration) {
    let start = Instant::now();
    if let Some(mtm) = MainThreadMarker::new() {
        return (Some(NSApplication::sharedApplication(mtm).isActive()), start.elapsed());
    }
    loop {
        let active = on_main_wait(ACTIVE_ASK_WAIT, |mtm| NSApplication::sharedApplication(mtm).isActive());
        match active {
            Some(false) if start.elapsed() < ACTIVE_WAIT => std::thread::sleep(ACTIVE_POLL),
            _ => return (active, start.elapsed()),
        }
    }
}

impl FocusSteps for SystemFocus {
    fn activate_self(&mut self) {
        if let Some(mtm) = MainThreadMarker::new() {
            take_activation(mtm);
        }
    }

    fn unhide(&mut self, pid: Pid) -> bool {
        let shown = unhide_app(pid);
        if let Some(sent) = shown {
            log::debug(&format!("claude_app: pid {pid} was hidden; shown again: {sent}"));
            self.report.activation.push(format!("unhide={sent}"));
        }
        shown.is_some()
    }

    /// macOS 14 and later: only the active app can pass activation on, so this waits briefly for
    /// Sessionmux to be active, then yields to `pid` and asks it to activate.
    fn pass_activation(&mut self, pid: Pid) {
        let Some(p) = ns_pid(pid) else { return };
        let modern = macos_at_least(14);
        let (active, waited) = if modern { wait_until_active() } else { (None, Duration::ZERO) };
        let sent = on_main_wait(MAIN_WAIT, move |mtm| {
            let target = NSRunningApplication::runningApplicationWithProcessIdentifier(p)?;
            if modern {
                NSApplication::sharedApplication(mtm).yieldActivationToApplication(&target);
                let me = NSRunningApplication::currentApplication();
                Some(target.activateFromApplication_options(&me, NSApplicationActivationOptions::ActivateAllWindows))
            } else {
                #[allow(deprecated)]
                let opts = NSApplicationActivationOptions::ActivateAllWindows
                    | NSApplicationActivationOptions::ActivateIgnoringOtherApps;
                Some(target.activateWithOptions(opts))
            }
        });
        let sent = match sent {
            None => "no-main-thread".to_string(),
            Some(None) => "not-a-running-app".to_string(),
            Some(Some(b)) => b.to_string(),
        };
        let active = active.map_or_else(|| "unknown".to_string(), |a| a.to_string());
        let note = format!("active_at_yield={active} wait_ms={} sent={sent}", waited.as_millis());
        log::debug(&format!("claude_app: activation of pid {pid}: {note}"));
        self.report.activation.push(note);
    }

    fn settle(&mut self) {
        std::thread::sleep(SETTLE);
    }

    fn in_front(&mut self, pid: Pid) -> bool {
        let front = front_window_owner() == Some(pid) || <Os as Windows>::foreground_pid() == Some(pid);
        front && <Os as Windows>::main_window(pid).is_some()
    }

    fn accessibility(&mut self) -> AxState {
        let state = if !raise_setting_on() {
            AxState::Off
        } else if <Os as Focus>::accessibility_trusted(false) == Some(true) {
            AxState::Allowed
        } else {
            AxState::Untrusted
        };
        if state != AxState::Allowed {
            log::debug(&format!("claude_app: Accessibility raise not used: {}", state.name()));
            self.report.ax.push(format!("off:{}", state.name()));
        }
        state
    }

    fn raise_with_accessibility(&mut self, pid: Pid) -> bool {
        let (ok, note) = ax_raise(pid);
        log::debug(&format!("claude_app: Accessibility raise of pid {pid}: {note}"));
        self.report.ax.push(note);
        ok
    }

    fn window_state(&mut self, pid: Pid) -> WinState {
        window_state(pid)
    }

    fn reopen(&mut self, pid: Pid) -> Reopen {
        let (r, note) = reopen_instance(pid);
        log::info(&format!("claude_app: reopen pid {pid}: {r:?} ({note})"));
        self.report.reopen = Some(format!("{r:?} ({note})"));
        r
    }

    fn wait_for_window(&mut self, pid: Pid) -> bool {
        let deadline = Instant::now() + REOPEN_WAIT;
        loop {
            if <Os as Windows>::main_window(pid).is_some() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(POLL);
        }
    }
}

/// Show the app `pid` again, on the main thread, when it is hidden. None when it was not hidden
/// (or the app is unknown); else whether the request was accepted.
fn unhide_app(pid: Pid) -> Option<bool> {
    let p = ns_pid(pid)?;
    on_main_wait(MAIN_WAIT, move |_| {
        let app = NSRunningApplication::runningApplicationWithProcessIdentifier(p)?;
        if !app.isHidden() {
            return None;
        }
        Some(app.unhide())
    })
    .flatten()
}

/// The real minimise steps.
#[derive(Default)]
struct SystemMinimise {
    notes: Vec<String>,
}

impl MinimiseSteps for SystemMinimise {
    fn accessibility(&mut self) -> AxState {
        let state = if !raise_setting_on() {
            AxState::Off
        } else if <Os as Focus>::accessibility_trusted(false) == Some(true) {
            AxState::Allowed
        } else {
            AxState::Untrusted
        };
        self.notes.push(format!("ax={}", state.name()));
        state
    }

    fn minimise_with_accessibility(&mut self, pid: Pid) -> bool {
        let (ok, note) = ax_minimise(pid);
        self.notes.push(format!("ax-minimise [{note}]"));
        ok
    }

    fn wait_until_gone(&mut self, pid: Pid) -> bool {
        let deadline = Instant::now() + MINIMISE_WAIT;
        loop {
            if !showing(pid) {
                return true;
            }
            if Instant::now() >= deadline {
                self.notes.push("still on screen".into());
                return false;
            }
            std::thread::sleep(POLL);
        }
    }
}

/// True when `pid` has a window on screen and its app is not hidden.
fn showing(pid: Pid) -> bool {
    !app_hidden(pid) && <Os as Windows>::main_window(pid).is_some()
}

/// Where `pid`'s windows are, read from the window server's list.
fn window_state(pid: Pid) -> WinState {
    if <Os as Windows>::main_window(pid).is_some() {
        WinState::OnScreen
    } else if has_off_screen_window(pid) {
        WinState::OffScreen
    } else {
        WinState::None
    }
}

/// The `.app` folder `pid` runs from, when it is the only running app with its bundle id. Launch
/// Services finds a running app by its bundle id, so only then does opening that folder reach
/// exactly this instance.
fn sole_instance_bundle(pid: Pid) -> Option<String> {
    let p = ns_pid(pid)?;
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(p)?;
    let id = app.bundleIdentifier()?;
    let all = NSRunningApplication::runningApplicationsWithBundleIdentifier(&id);
    if all.len() != 1 || all.iter().any(|a| a.processIdentifier() != p) {
        return None;
    }
    let path = app.bundleURL()?.path()?.to_string();
    Some(real_path(&path))
}

/// Ask the running instance `pid` to show itself, as a click on its Dock icon does. When it is
/// the only instance running from its copy of Claude, Launch Services does that (it activates the
/// instance and needs no Automation permission). Otherwise, or when that fails, the reopen Apple
/// Event is sent to that process only; the user is asked once for Automation permission. Returns
/// the outcome and what was tried, for the log.
fn reopen_instance(pid: Pid) -> (Reopen, String) {
    let mut tried = Vec::new();
    if let Some(bundle) = sole_instance_bundle(pid) {
        match reopen_by_launch(pid, &bundle) {
            Ok(()) => return (Reopen::Sent, "launch services".into()),
            Err(e) => tried.push(format!("launch services: {e}")),
        }
    }
    let mut sent = send_event_to_pid(pid, CORE_EVENT_CLASS, AE_REOPEN_APPLICATION, None, false);
    if sent == Err(AeSendError::NotPermitted(ERR_NEEDS_CONSENT)) {
        tried.push(format!("apple event: {}", ae_error(&AeSendError::NotPermitted(ERR_NEEDS_CONSENT))));
        sent = send_event_to_pid(pid, CORE_EVENT_CLASS, AE_REOPEN_APPLICATION, None, true);
    }
    let r = match &sent {
        Ok(()) => Reopen::Sent,
        Err(AeSendError::NotPermitted(_)) => Reopen::Denied,
        Err(AeSendError::NoAnswer(_)) => Reopen::NoAnswer,
        Err(_) => Reopen::Failed,
    };
    tried.push(match &sent {
        Ok(()) => "apple event".into(),
        Err(e) => format!("apple event: {}", ae_error(e)),
    });
    (r, tried.join("; "))
}

fn ae_error(e: &AeSendError) -> String {
    match e {
        AeSendError::NotPermitted(code) if *code == ERR_NOT_PERMITTED => format!("not permitted ({code})"),
        AeSendError::NotPermitted(code) => format!("not asked yet ({code})"),
        AeSendError::Gone => "process gone".into(),
        AeSendError::NoAnswer(s) | AeSendError::Failed(s) => s.clone(),
    }
}

/// Open the copy of Claude at `bundle` through Launch Services without a new instance: its only
/// running instance, `pid`, is activated and asked to show itself. Should Launch Services start
/// a new process instead, that one is asked to quit again at once; a process that already ran is
/// never touched.
fn reopen_by_launch(pid: Pid, bundle: &str) -> Result<(), String> {
    let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(bundle), true);
    let cfg = NSWorkspaceOpenConfiguration::configuration();
    cfg.setCreatesNewApplicationInstance(false);
    cfg.setActivates(true);
    cfg.setAddsToRecentItems(false);
    cfg.setPromptsUserIfNeeded(false);
    let before: HashSet<Pid> = any_claude_mains(None).into_iter().map(|p| p.pid).collect();
    match open_app(&url, &cfg, REOPEN_LAUNCH_WAIT) {
        Ok(got) if got == pid => Ok(()),
        Ok(other) if before.contains(&other) => Err(format!("reached pid {other}")),
        Ok(other) => {
            log::warn(&format!("claude_app: reopening pid {pid} started pid {other}; asking it to quit"));
            ask_to_quit(other);
            Err(format!("started pid {other}"))
        }
        Err(e) => Err(e),
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

/// An Accessibility attribute of `el`, owned, or the error code.
fn ax_attr_or_code(el: &CFType, name: &str) -> Result<CFRetained<CFType>, i32> {
    let mut value: *mut CFType = std::ptr::null_mut();
    // SAFETY: `el` is a live accessibility object, the name a live string, and `value` a writable
    // out-pointer that receives a new reference on success.
    let err = unsafe { AXUIElementCopyAttributeValue(el, &CFString::from_str(name), &mut value) };
    if err != AX_SUCCESS {
        return Err(err);
    }
    owned(value).ok_or(AX_NO_VALUE)
}

/// An Accessibility attribute of `el`, owned.
fn ax_attr(el: &CFType, name: &str) -> Option<CFRetained<CFType>> {
    ax_attr_or_code(el, name).ok()
}

/// Set a boolean Accessibility attribute of `el`; the result code.
fn ax_set_bool(el: &CFType, name: &str, value: bool) -> i32 {
    let v: &CFType = CFBoolean::new(value);
    // SAFETY: `el` is a live accessibility object, the name a live string and the value a
    // constant boolean.
    unsafe { AXUIElementSetAttributeValue(el, &CFString::from_str(name), v) }
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

/// True when an Accessibility window is minimised.
fn ax_minimised(win: &CFType) -> bool {
    ax_attr(win, "AXMinimized").and_then(|v| v.downcast::<CFBoolean>().ok()).is_some_and(|b| b.as_bool())
}

/// Un-minimise `pid`'s largest minimised window (else its largest), raise it, make it the app's
/// main window and the app the one in front. True when the raise or the move to the front was accepted; with the result
/// codes of every call, for the log.
fn ax_raise(pid: Pid) -> (bool, String) {
    let Some(p) = ns_pid(pid) else { return (false, "bad pid".into()) };
    // SAFETY: creating an accessibility object for a pid has no preconditions; it returns a new
    // reference, owned below.
    let Some(app) = owned(unsafe { AXUIElementCreateApplication(p) }) else {
        return (false, "no application element".into());
    };
    // SAFETY: `app` is a live accessibility object; a failure only keeps the default timeout.
    unsafe { AXUIElementSetMessagingTimeout(&app, AX_TIMEOUT_SECS) };
    let list = match ax_attr_or_code(&app, "AXWindows") {
        Ok(v) => v,
        Err(code) => return (false, format!("windows error {code}")),
    };
    let Ok(list) = list.downcast::<CFArray>() else { return (false, "windows is not a list".into()) };
    // SAFETY: the windows attribute is documented as an array of accessibility objects; they are
    // only passed back to Accessibility calls.
    let list: CFRetained<CFArray<CFType>> = unsafe { CFRetained::cast_unchecked(list) };
    let count = list.len();
    // A minimised window comes back first (the one a click on the account in front put away),
    // else the largest.
    let largest = |only_minimised: bool| {
        list.iter()
            .filter(|w| !only_minimised || ax_minimised(w))
            .map(|w| (ax_area(&w), w))
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, w)| w)
    };
    let Some(win) = largest(true).or_else(|| largest(false)) else {
        return (false, "windows 0".into());
    };
    let unminimised = ax_set_bool(&win, "AXMinimized", false);
    // SAFETY: `win` is a live accessibility object of Claude's and the action name a live string.
    let raised = unsafe { AXUIElementPerformAction(&win, &CFString::from_str("AXRaise")) };
    let main = ax_set_bool(&win, "AXMain", true);
    let frontmost = ax_set_bool(&app, "AXFrontmost", true);
    let note = format!("windows {count}, unminimise {unminimised}, raise {raised}, main {main}, frontmost {frontmost}");
    (raised == AX_SUCCESS || frontmost == AX_SUCCESS, note)
}

/// Minimise `pid`'s focused window (else its main one, else its largest) through Accessibility.
/// True when the request was accepted; with the result codes, for the log.
fn ax_minimise(pid: Pid) -> (bool, String) {
    let Some(p) = ns_pid(pid) else { return (false, "bad pid".into()) };
    // SAFETY: creating an accessibility object for a pid has no preconditions; it returns a new
    // reference, owned below.
    let Some(app) = owned(unsafe { AXUIElementCreateApplication(p) }) else {
        return (false, "no application element".into());
    };
    // SAFETY: `app` is a live accessibility object; a failure only keeps the default timeout.
    unsafe { AXUIElementSetMessagingTimeout(&app, AX_TIMEOUT_SECS) };
    let (win, which) = match ax_attr(&app, "AXFocusedWindow") {
        Some(w) => (w, "focused"),
        None => match ax_attr(&app, "AXMainWindow") {
            Some(w) => (w, "main"),
            None => {
                let list = match ax_attr_or_code(&app, "AXWindows") {
                    Ok(v) => v,
                    Err(code) => return (false, format!("windows error {code}")),
                };
                let Ok(list) = list.downcast::<CFArray>() else { return (false, "windows is not a list".into()) };
                // SAFETY: the windows attribute is documented as an array of accessibility
                // objects; they are only passed back to Accessibility calls.
                let list: CFRetained<CFArray<CFType>> = unsafe { CFRetained::cast_unchecked(list) };
                let largest = list.iter().map(|w| (ax_area(&w), w)).max_by(|a, b| a.0.total_cmp(&b.0));
                let Some((_, w)) = largest else { return (false, "windows 0".into()) };
                (w, "largest")
            }
        },
    };
    let code = ax_set_bool(&win, "AXMinimized", true);
    (code == AX_SUCCESS, format!("{which} window, minimise {code}"))
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
    /// Make Sessionmux the active app before [`Focus::focus`] runs on a worker thread. The
    /// click, menu choice or shortcut usually did this already, inside its own event; here it
    /// only covers a request that came without one.
    fn prepare_focus(_pid: Pid) {
        match MainThreadMarker::new() {
            Some(mtm) => take_activation(mtm),
            None => log::debug("claude_app: activation can only be prepared on the main thread"),
        }
    }

    /// Bring `pid` forward and log one line with what was tried and what was seen.
    fn focus(pid: Pid) -> FocusResult {
        if !<Os as Processes>::alive(pid) {
            log::info(&format!("claude_app: focus pid {pid}: not running"));
            return FocusResult::Failed(FocusFailure::Gone);
        }
        let mut s = SystemFocus::default();
        let mut r = focus_with(&mut s, pid, RELAUNCH_HANDOFF);
        if matches!(r, FocusResult::Failed(_)) && !<Os as Processes>::alive(pid) {
            r = FocusResult::Failed(FocusFailure::Gone);
        }
        let front = <Os as Windows>::foreground_pid().or_else(front_window_owner);
        let front = front.map_or_else(|| "unknown".to_string(), |p| p.to_string());
        log::info(&format!(
            "claude_app: focus pid {pid}: {r:?} {} front_pid={front} window={}",
            s.report.line(),
            window_state(pid).name()
        ));
        r
    }

    fn showing(pid: Pid) -> bool {
        showing(pid)
    }

    /// Put away `pid`'s window in front and log one line with what was tried.
    fn minimise(pid: Pid) -> FocusResult {
        if !<Os as Processes>::alive(pid) {
            return FocusResult::Failed(FocusFailure::Gone);
        }
        let mut s = SystemMinimise::default();
        let r = minimise_with(&mut s, pid);
        log::info(&format!("claude_app: minimise pid {pid}: {r:?} [{}]", s.notes.join(", ")));
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
    use std::sync::{Mutex, MutexGuard};

    /// Held by every stand-in while it exists: their windows compete for the front, so tests that
    /// run stand-ins take turns.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

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
        _turn: MutexGuard<'static, ()>,
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
        let turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
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
        Ok(FakeClaude { root, bundle, _dir: dir, _turn: turn })
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

        /// Give this stand-in a bundle id of its own (before it is started).
        pub(crate) fn set_bundle_id(&self, id: &str) {
            let plist = self.bundle.join("Contents").join("Info.plist");
            let text = std::fs::read_to_string(&plist).expect("Info.plist").replace(BUNDLE_ID, id);
            std::fs::write(&plist, text).expect("Info.plist");
        }

        /// How often main process `pid` was asked to show itself.
        pub(crate) fn recorded_reopens(&self, pid: u32) -> usize {
            std::fs::read_to_string(self.root.join(format!("reopen-{pid}.txt"))).map_or(0, |s| s.lines().count())
        }

        /// How many main processes of the stand-in have started so far.
        pub(crate) fn started_count(&self) -> usize {
            std::fs::read_dir(&self.root).map_or(0, |d| {
                d.filter_map(Result::ok)
                    .filter(|e| {
                        let n = e.file_name().to_string_lossy().into_owned();
                        n.starts_with("argv-") && n.ends_with(".txt")
                    })
                    .count()
            })
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

    struct Script {
        steps: Vec<String>,
        /// Whether the app is hidden when the focus starts.
        hidden: bool,
        /// `in_front` answers, in order; false once used up.
        front: VecDeque<bool>,
        ax: AxState,
        ax_raised: bool,
        /// `window_state` answers, in order; the last one repeats.
        windows: VecDeque<WinState>,
        reopen: Reopen,
        window_after_reopen: bool,
    }

    impl Default for Script {
        fn default() -> Script {
            Script {
                steps: Vec::new(),
                hidden: false,
                front: VecDeque::new(),
                ax: AxState::Off,
                ax_raised: false,
                windows: VecDeque::from([WinState::OnScreen]),
                reopen: Reopen::Sent,
                window_after_reopen: false,
            }
        }
    }

    impl FocusSteps for Script {
        fn activate_self(&mut self) {
            self.steps.push("activate-self".into());
        }
        fn unhide(&mut self, pid: Pid) -> bool {
            // Only a hidden app has anything to do here, so the other scripts stay as they are.
            let was = std::mem::take(&mut self.hidden);
            if was {
                self.steps.push(format!("unhide-{pid}"));
            }
            was
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
        fn accessibility(&mut self) -> AxState {
            self.steps.push("ax?".into());
            self.ax
        }
        fn raise_with_accessibility(&mut self, pid: Pid) -> bool {
            self.steps.push(format!("ax-raise-{pid}"));
            self.ax_raised
        }
        fn window_state(&mut self, pid: Pid) -> WinState {
            self.steps.push(format!("windows?-{pid}"));
            if self.windows.len() > 1 {
                self.windows.pop_front().unwrap_or(WinState::None)
            } else {
                self.windows.front().copied().unwrap_or(WinState::None)
            }
        }
        fn reopen(&mut self, pid: Pid) -> Reopen {
            self.steps.push(format!("reopen-{pid}"));
            self.reopen
        }
        fn wait_for_window(&mut self, pid: Pid) -> bool {
            self.steps.push(format!("wait-window-{pid}"));
            self.window_after_reopen
        }
    }

    fn script(front: &[bool], ax: AxState, ax_raised: bool) -> Script {
        Script { front: front.iter().copied().collect(), ax, ax_raised, ..Script::default() }
    }

    fn failed(f: FocusFailure) -> FocusResult {
        FocusResult::Failed(f)
    }

    #[test]
    fn activation_alone_is_enough_for_a_visible_window() {
        let mut s = script(&[true], AxState::Allowed, true);
        assert_eq!(focus_with(&mut s, 7, false), FocusResult::Focused);
        assert_eq!(s.steps, ["activate-self", "pass-7", "settle", "front?-7"]);
    }

    #[test]
    fn a_hidden_instance_is_shown_again_before_activation() {
        let mut s = Script { hidden: true, front: VecDeque::from([true]), ..Script::default() };
        assert_eq!(focus_with(&mut s, 7, false), FocusResult::Focused);
        assert_eq!(s.steps, ["activate-self", "unhide-7", "pass-7", "settle", "front?-7"]);
    }

    // ── minimise order ──

    struct Put {
        steps: Vec<String>,
        ax: AxState,
        ax_ok: bool,
        /// `wait_until_gone` answers, in order; false once used up.
        gone: VecDeque<bool>,
    }

    impl MinimiseSteps for Put {
        fn accessibility(&mut self) -> AxState {
            self.steps.push("ax?".into());
            self.ax
        }
        fn minimise_with_accessibility(&mut self, pid: Pid) -> bool {
            self.steps.push(format!("ax-minimise-{pid}"));
            self.ax_ok
        }
        fn wait_until_gone(&mut self, pid: Pid) -> bool {
            self.steps.push(format!("gone?-{pid}"));
            self.gone.pop_front().unwrap_or(false)
        }
    }

    fn put(ax: AxState, ax_ok: bool, gone: &[bool]) -> Put {
        Put { steps: Vec::new(), ax, ax_ok, gone: gone.iter().copied().collect() }
    }

    #[test]
    fn minimise_uses_accessibility_when_allowed() {
        let mut s = put(AxState::Allowed, true, &[true]);
        assert_eq!(minimise_with(&mut s, 7), FocusResult::Minimised);
        assert_eq!(s.steps, ["ax?", "ax-minimise-7", "gone?-7"]);
    }

    #[test]
    fn minimise_tries_nothing_without_accessibility() {
        for ax in [AxState::Off, AxState::Untrusted] {
            let mut s = put(ax, true, &[true]);
            assert_eq!(minimise_with(&mut s, 7), failed(FocusFailure::Refused));
            assert_eq!(s.steps, ["ax?"]);
        }
    }

    #[test]
    fn a_window_that_stays_on_screen_is_reported() {
        let mut s = put(AxState::Allowed, false, &[]);
        assert_eq!(minimise_with(&mut s, 7), failed(FocusFailure::Refused));
        assert_eq!(s.steps, ["ax?", "ax-minimise-7"]);
        let mut s = put(AxState::Allowed, true, &[false]);
        assert_eq!(minimise_with(&mut s, 7), failed(FocusFailure::Refused));
        assert_eq!(s.steps, ["ax?", "ax-minimise-7", "gone?-7"]);
    }

    #[test]
    fn the_real_minimise_leaves_a_missing_process_alone() {
        assert_eq!(Os::minimise(0), failed(FocusFailure::Gone));
        assert!(!Os::showing(0));
    }

    #[test]
    fn hand_off_comes_before_accessibility_where_claude_hands_over() {
        let mut s = script(&[false], AxState::Allowed, true);
        assert_eq!(focus_with(&mut s, 7, true), FocusResult::NeedsHandOff);
        assert_eq!(s.steps, ["activate-self", "pass-7", "settle", "front?-7"]);
    }

    #[test]
    fn accessibility_raise_then_activation_again() {
        let mut s = script(&[false, true], AxState::Allowed, true);
        assert_eq!(focus_with(&mut s, 7, false), FocusResult::Focused);
        assert_eq!(
            s.steps,
            ["activate-self", "pass-7", "settle", "front?-7", "ax?", "ax-raise-7", "pass-7", "settle", "front?-7"]
        );
        // Raised but still not in front: refused, and no reopen (the window exists).
        let mut s = script(&[false, false], AxState::Allowed, true);
        assert_eq!(focus_with(&mut s, 7, false), failed(FocusFailure::Refused));
        assert!(!s.steps.iter().any(|x| x.starts_with("reopen")), "{:?}", s.steps);
    }

    #[test]
    fn a_visible_window_that_stays_behind_is_refused_or_needs_accessibility() {
        let mut s = script(&[false], AxState::Off, true);
        assert_eq!(focus_with(&mut s, 7, false), failed(FocusFailure::Refused));
        assert_eq!(s.steps, ["activate-self", "pass-7", "settle", "front?-7", "ax?", "windows?-7", "windows?-7"]);
        let mut s = script(&[false], AxState::Untrusted, true);
        assert_eq!(focus_with(&mut s, 7, false), failed(FocusFailure::AccessibilityUntrusted));
        assert!(!s.steps.iter().any(|x| x.starts_with("ax-raise")), "{:?}", s.steps);
    }

    #[test]
    fn a_closed_window_is_reopened_and_brought_forward() {
        // Activation is not enough; the reopened window is already in front.
        let mut s = Script {
            front: VecDeque::from([false, true]),
            windows: VecDeque::from([WinState::None, WinState::OnScreen]),
            window_after_reopen: true,
            ..Script::default()
        };
        assert_eq!(focus_with(&mut s, 7, false), FocusResult::Focused);
        assert_eq!(
            s.steps,
            [
                "activate-self",
                "pass-7",
                "settle",
                "front?-7",
                "ax?",
                "windows?-7",
                "reopen-7",
                "wait-window-7",
                "front?-7"
            ]
        );

        // The reopened window came up behind: activation (and the raise, when allowed) again.
        let mut s = Script {
            front: VecDeque::from([false, false, true]),
            ax: AxState::Allowed,
            ax_raised: false,
            windows: VecDeque::from([WinState::None, WinState::OnScreen]),
            window_after_reopen: true,
            ..Script::default()
        };
        assert_eq!(focus_with(&mut s, 7, false), FocusResult::Focused);
        assert_eq!(
            s.steps,
            [
                "activate-self",
                "pass-7",
                "settle",
                "front?-7",
                "ax?",
                "ax-raise-7",
                "windows?-7",
                "reopen-7",
                "wait-window-7",
                "front?-7",
                "ax-raise-7",
                "pass-7",
                "settle",
                "front?-7"
            ]
        );
    }

    #[test]
    fn an_accepted_raise_that_leaves_the_window_off_screen_still_reopens() {
        let mut s = Script {
            front: VecDeque::from([false, false, true]),
            ax: AxState::Allowed,
            ax_raised: true,
            windows: VecDeque::from([WinState::OffScreen, WinState::OnScreen]),
            window_after_reopen: true,
            ..Script::default()
        };
        assert_eq!(focus_with(&mut s, 7, false), FocusResult::Focused);
        assert_eq!(
            s.steps,
            [
                "activate-self",
                "pass-7",
                "settle",
                "front?-7",
                "ax?",
                "ax-raise-7",
                "pass-7",
                "settle",
                "front?-7",
                "windows?-7",
                "reopen-7",
                "wait-window-7",
                "front?-7"
            ]
        );
    }

    #[test]
    fn reopen_failures_have_their_own_reasons() {
        let closed = |reopen: Reopen| Script {
            windows: VecDeque::from([WinState::None]),
            reopen,
            window_after_reopen: false,
            ..Script::default()
        };
        let mut s = closed(Reopen::Sent);
        assert_eq!(focus_with(&mut s, 7, false), failed(FocusFailure::NoWindow));
        assert_eq!(s.steps.last().map(String::as_str), Some("windows?-7"));
        assert!(s.steps.contains(&"wait-window-7".to_string()));

        let mut s = closed(Reopen::Denied);
        assert_eq!(focus_with(&mut s, 7, false), failed(FocusFailure::AutomationDenied));
        assert!(!s.steps.contains(&"wait-window-7".to_string()), "nothing to wait for");

        let mut s = closed(Reopen::Failed);
        assert_eq!(focus_with(&mut s, 7, false), failed(FocusFailure::NoWindow));

        let mut s = closed(Reopen::NoAnswer);
        assert_eq!(focus_with(&mut s, 7, false), failed(FocusFailure::NotResponding));

        // A closed window counted as off screen, the user refused Automation: the advice is
        // Automation, not the Accessibility setting.
        let mut s =
            Script { windows: VecDeque::from([WinState::OffScreen]), reopen: Reopen::Denied, ..Script::default() };
        assert_eq!(focus_with(&mut s, 7, false), failed(FocusFailure::AutomationDenied));

        // A minimised window with the setting off: reopen tried, then the setting is the advice.
        let mut s = Script { windows: VecDeque::from([WinState::OffScreen]), ..Script::default() };
        assert_eq!(focus_with(&mut s, 7, false), failed(FocusFailure::AccessibilityOff));
        assert!(s.steps.contains(&"reopen-7".to_string()));
    }

    #[test]
    fn failure_reasons() {
        use AxState::{Allowed, Off, Untrusted};
        use WinState::{OffScreen, OnScreen};
        assert_eq!(failure(Off, None, OnScreen), FocusFailure::Refused);
        assert_eq!(failure(Allowed, None, OnScreen), FocusFailure::Refused);
        assert_eq!(failure(Untrusted, None, OnScreen), FocusFailure::AccessibilityUntrusted);
        assert_eq!(failure(Off, Some(Reopen::Sent), OffScreen), FocusFailure::AccessibilityOff);
        assert_eq!(failure(Off, None, OffScreen), FocusFailure::AccessibilityOff);
        assert_eq!(failure(Untrusted, Some(Reopen::Sent), OffScreen), FocusFailure::AccessibilityUntrusted);
        assert_eq!(failure(Allowed, Some(Reopen::Sent), OffScreen), FocusFailure::Refused);
        assert_eq!(failure(Off, Some(Reopen::Sent), WinState::None), FocusFailure::NoWindow);
        assert_eq!(failure(Off, Some(Reopen::Failed), WinState::None), FocusFailure::NoWindow);
        // A refused or unanswered reopen wins over any Accessibility advice.
        for ax in [Off, Allowed, Untrusted] {
            for win in [OffScreen, WinState::None] {
                assert_eq!(failure(ax, Some(Reopen::Denied), win), FocusFailure::AutomationDenied);
                assert_eq!(failure(ax, Some(Reopen::NoAnswer), win), FocusFailure::NotResponding);
            }
        }
    }

    #[test]
    fn the_real_focus_never_hands_off() {
        if RELAUNCH_HANDOFF {
            return;
        }
        assert_ne!(Os::focus(u32::MAX), FocusResult::NeedsHandOff);
        assert_eq!(Os::focus(0), failed(FocusFailure::Gone));
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
    fn installed_copies_come_before_transient_ones() {
        let home = Path::new("/Users/u");
        let apps = PathBuf::from("/Applications/Claude.app");
        let user_apps = PathBuf::from("/Users/u/Applications/Claude.app");
        assert_eq!(order_candidates(None, home), vec![apps.clone(), user_apps.clone()]);
        let elsewhere = PathBuf::from("/Applications/Tools/Claude.app");
        assert_eq!(order_candidates(Some(elsewhere.clone()), home), vec![elsewhere, apps.clone(), user_apps.clone()]);
        for transient in [
            "/Volumes/Claude/Claude.app",
            "/Users/u/.Trash/Claude.app",
            "/private/var/folders/x/T/AppTranslocation/1234/d/Claude.app",
        ] {
            let t = PathBuf::from(transient);
            assert!(is_transient_copy(&t), "{transient}");
            assert_eq!(order_candidates(Some(t.clone()), home), vec![apps.clone(), user_apps.clone(), t]);
        }
        assert!(!is_transient_copy(Path::new("/Applications/Claude.app")));
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
    fn a_tool_started_with_claudes_program_is_not_an_instance() {
        let f = match fake::build() {
            Ok(f) => f,
            Err(why) => {
                println!("skipped: {why}");
                return;
            }
        };
        let l = located_at(&f.bundle).unwrap();
        let dir = f.data_dir("Claude-Tool");
        std::fs::write(dir.join("spawn-tool"), b"").unwrap();
        let dir_s = dir.to_string_lossy().into_owned();
        let pid = Os::launch(&l, &dir_s).expect("start");
        assert!(wait_for(10, || children_of(pid).len() >= 3), "helpers and the tool started");
        let tool = children_of(pid)
            .into_iter()
            .find(|&c| Os::argv(c).is_some_and(|a| !is_helper(&a)))
            .expect("the tool runs without --type=");
        let listed = Os::list_claude(Some(&l));
        assert!(listed.iter().all(|p| p.pid != tool), "the tool is not listed: {listed:?}");
        let mine: Vec<Pid> =
            listed.iter().filter(|p| claude_app::matches_data_dir(p, Some(&dir_s))).map(|p| p.pid).collect();
        assert_eq!(mine, vec![pid]);
        Os::close(&[pid]).expect("close");
        assert!(wait_for(5, || !Os::alive(tool)), "the tool ended with its app");
    }

    /// Run `f` on a thread of its own and wait up to 60 s; None when it did not return in time
    /// (a permission prompt nobody answers).
    fn in_time<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> Option<R> {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(Duration::from_secs(60)).ok()
    }

    #[test]
    fn reopen_reaches_only_the_targeted_instance() {
        let f = match fake::build() {
            Ok(f) => f,
            Err(why) => {
                println!("skipped: {why}");
                return;
            }
        };
        let l = located_at(&f.bundle).unwrap();
        let dir_a = f.data_dir("Claude-ReopenA");
        std::fs::write(dir_a.join("start-windowless"), b"").unwrap();
        let dir_b = f.data_dir("Claude-ReopenB");
        let pid_a = Os::launch(&l, &dir_a.to_string_lossy()).expect("start A");
        let pid_b = Os::launch(&l, &dir_b.to_string_lossy()).expect("start B");
        assert!(wait_for(10, || f.recorded_args(pid_a).is_some() && f.recorded_args(pid_b).is_some()));
        // Two instances of one copy: Launch Services cannot tell them apart, so none is chosen.
        assert_eq!(sole_instance_bundle(pid_a), None);

        let sent = in_time(move || send_event_to_pid(pid_a, CORE_EVENT_CLASS, AE_REOPEN_APPLICATION, None, true));
        match sent {
            None => println!("Automation: the permission prompt was left open on this machine; delivery not checked"),
            Some(Err(AeSendError::NotPermitted(code))) => {
                println!("Automation: not allowed on this machine ({code}); delivery not checked")
            }
            Some(Err(e)) => panic!("reopen to A failed: {e:?}"),
            Some(Ok(())) => {
                println!("Automation: allowed on this machine");
                assert!(wait_for(10, || f.recorded_reopens(pid_a) == 1), "A was asked to show itself");
                std::thread::sleep(Duration::from_millis(500));
                assert_eq!(f.recorded_reopens(pid_b), 0, "B was not");
                if !super::super::window::on_screen().is_empty() {
                    assert!(wait_for(10, || Os::main_window(pid_a).is_some()), "A opened its window");
                }
            }
        }
        let _ = Os::close(&[pid_a, pid_b]);
    }

    /// What the system lists for `pid`, for a failed assertion.
    fn running_app_note(pid: Pid) -> String {
        let Some(app) = ns_pid(pid).and_then(NSRunningApplication::runningApplicationWithProcessIdentifier) else {
            return format!("pid {pid} is not a running app");
        };
        let id = app.bundleIdentifier();
        let same = id.as_ref().map_or(0, |id| NSRunningApplication::runningApplicationsWithBundleIdentifier(id).len());
        let url = app.bundleURL().and_then(|u| u.path()).map(|p| p.to_string());
        format!("pid {pid}: bundle id {id:?}, {same} running with it, at {url:?}")
    }

    #[test]
    fn focus_reopens_an_instance_without_a_window() {
        let f = match fake::build() {
            Ok(f) => f,
            Err(why) => {
                println!("skipped: {why}");
                return;
            }
        };
        if super::super::window::on_screen().is_empty() {
            println!("skipped: no window server session");
            return;
        }
        // A bundle id of its own: Launch Services reaches a running app by its id, and other tests
        // run stand-ins with the shared one at the same time.
        f.set_bundle_id(&format!("test.sessionmux.windowless.{}", std::process::id()));
        let l = located_at(&f.bundle).unwrap();
        let dir = f.data_dir("Claude-Windowless");
        std::fs::write(dir.join("start-windowless"), b"").unwrap();
        let dir_s = dir.to_string_lossy().into_owned();
        let pid = Os::launch(&l, &dir_s).expect("start");
        assert!(wait_for(10, || f.recorded_args(pid).is_some()));
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(window_state(pid), WinState::None, "the stand-in starts without a window");
        // Launch Services may list a new app a moment after its process starts.
        let want = real_path(&f.bundle.to_string_lossy());
        wait_for(5, || sole_instance_bundle(pid).is_some());
        assert_eq!(sole_instance_bundle(pid), Some(want), "{}", running_app_note(pid));

        let r = in_time(move || Os::focus(pid)).expect("focus returned in time");
        let front = front_window_owner();
        assert!(f.recorded_reopens(pid) >= 1, "the instance was asked to show itself ({r:?})");
        assert!(Os::main_window(pid).is_some(), "its window is on screen ({r:?})");
        assert_eq!(r, FocusResult::Focused, "front window owner {front:?}, target {pid}");
        // Reopening never starts another instance.
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(f.started_count(), 1, "no second instance was started");
        let on_dir = Os::list_claude(Some(&l)).into_iter().filter(|p| claude_app::matches_data_dir(p, Some(&dir_s)));
        assert_eq!(on_dir.count(), 1);
        let _ = Os::close(&[pid]);
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
