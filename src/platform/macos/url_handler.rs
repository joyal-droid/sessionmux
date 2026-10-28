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

//! The per-user handler of a link scheme on macOS, and delivering sign-in links to one Claude
//! instance.
//!
//! - The handler is the app Launch Services opens for `<scheme>://` links, identified by the
//!   app's path. Sessionmux takes it with `NSWorkspace setDefaultApplicationAtURL:
//!   toOpenURLsWithScheme:` and gives it back the same way.
//! - Changes run only on worker threads: from macOS 26.4 on every change shows a system "Use /
//!   Keep" prompt and the change completes only when the user answers, which can take minutes.
//!   A change that has no answer yet is pending, not failed; one the user refused is declined.
//! - Claude makes itself the `claude://` handler again whenever one of its instances starts
//!   (within a few tens of milliseconds of its process appearing).
//! - A link is delivered as a get-URL Apple Event sent to the process id of the account's Claude,
//!   so it reaches that instance and not whichever one Launch Services would pick. The first send
//!   asks the user once for Automation permission. A second start of Claude on a folder that
//!   already runs does not hand over to the running instance, so links are never sent that way.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2_app_kit::NSWorkspace;
use objc2_core_foundation::CFString;
use objc2_foundation::{NSBundle, NSError, NSOperatingSystemVersion, NSProcessInfo, NSString, NSURL};

use super::claude::{located_at, RELAUNCH_HANDOFF};
use super::process::claude_mains;
use crate::brand;
use crate::core::claude_app::{self, Located};
use crate::platform::{
    ForwardError, HandlerChange, HandlerId, Launcher, Misc, Os, Paths, Pid, Processes, UrlHandler, Windows,
};
use crate::util::log;

/// Whether a new start of Claude with a link hands the link to the running instance of the same
/// data folder. Not relied on: a second start keeps running beside the first.
const RELAUNCH_PASSES_LINK: bool = false;

/// How long a handler change may take to report its result before it counts as pending.
const COMPLETION_WAIT: Duration = Duration::from_secs(5);
/// How long Launch Services may take to report a finished change.
const READ_WAIT: Duration = Duration::from_secs(2);
/// How long a Claude started to receive a link may take to show its window.
const WINDOW_WAIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(100);
/// How long one Apple Event may take to be accepted, in ticks of 1/60 s (5 s).
const SEND_TIMEOUT_TICKS: libc::c_long = 300;
/// How long sending one Apple Event may take in all when nobody is asked for permission.
const SEND_WAIT: Duration = Duration::from_secs(5);
/// How long the user may take to answer the Automation prompt before the send gives up (the
/// prompt stays open and its answer still counts for the next send).
const ASK_WAIT: Duration = Duration::from_secs(120);

/// A four-character Apple Event code as the number the system uses.
const fn fourcc(c: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*c)
}

/// `kInternetEventClass` and `kAEGetURL`: "open this link".
const INTERNET_EVENT_CLASS: u32 = fourcc(b"GURL");
const AE_GET_URL: u32 = fourcc(b"GURL");
/// `kCoreEventClass` and `kAEReopenApplication`: "show yourself" (what a click on the Dock icon
/// of a running app sends).
pub(crate) const CORE_EVENT_CLASS: u32 = fourcc(b"aevt");
pub(crate) const AE_REOPEN_APPLICATION: u32 = fourcc(b"rapp");
/// `keyDirectObject`: the link itself.
const KEY_DIRECT_OBJECT: u32 = fourcc(b"----");
/// `typeKernelProcessID`: an address given as a process id.
const TYPE_KERNEL_PROCESS_ID: u32 = fourcc(b"kpid");
/// `typeUTF8Text`.
const TYPE_UTF8_TEXT: u32 = fourcc(b"utf8");
/// `kAutoGenerateReturnID` and `kAnyTransactionID`.
const AUTO_GENERATE_RETURN_ID: i16 = -1;
const ANY_TRANSACTION_ID: i32 = 0;
/// `kAENoReply | kAECanSwitchLayer`: do not wait for an answer; the receiver may come forward.
const SEND_MODE: i32 = 0x0000_0001 | 0x0000_0040;
/// `errAEEventNotPermitted`: the user has not allowed Sessionmux to send events to the target.
pub(crate) const ERR_NOT_PERMITTED: i32 = -1743;
/// `errAEEventWouldRequireUserConsent`: the user has not been asked yet.
pub(crate) const ERR_NEEDS_CONSENT: i32 = -1744;
/// `procNotFound`: the target process is gone.
const ERR_PROC_NOT_FOUND: i32 = -600;
/// `errAETimeout`: the target did not answer in time.
const ERR_AE_TIMEOUT: i32 = -1712;
/// `userCanceledErr`.
const ERR_USER_CANCELED: isize = -128;
/// `NSUserCancelledError`.
const NS_USER_CANCELLED: isize = 3072;

/// Room for one `AEDesc` (a type code and a data pointer, packed or not). Only the Apple Event
/// functions read or write it; it is never interpreted here.
#[repr(C, align(8))]
struct AeDesc([u8; 16]);

#[link(name = "CoreServices", kind = "framework")]
extern "C" {
    fn LSSetDefaultHandlerForURLScheme(scheme: &CFString, handler_bundle_id: &CFString) -> i32;
    fn AEInitializeDesc(desc: *mut AeDesc);
    fn AEDisposeDesc(desc: *mut AeDesc) -> i16;
    fn AECreateDesc(type_code: u32, data: *const c_void, size: libc::c_long, result: *mut AeDesc) -> i16;
    fn AECreateAppleEvent(
        event_class: u32,
        event_id: u32,
        target: *const AeDesc,
        return_id: i16,
        transaction_id: i32,
        result: *mut AeDesc,
    ) -> i16;
    fn AEPutParamPtr(event: *mut AeDesc, keyword: u32, type_code: u32, data: *const c_void, size: libc::c_long) -> i16;
    fn AESendMessage(event: *const AeDesc, reply: *mut AeDesc, send_mode: i32, timeout_ticks: libc::c_long) -> i32;
    fn AEDeterminePermissionToAutomateTarget(target: *const AeDesc, event_class: u32, event_id: u32, ask: u8) -> i32;
}

impl AeDesc {
    /// An empty descriptor, ready to be filled by an Apple Event function.
    fn new() -> AeDesc {
        let mut d = AeDesc([0; 16]);
        // SAFETY: `d` has room for a descriptor; initialising it only writes the null type and a
        // null data pointer.
        unsafe { AEInitializeDesc(&mut d) };
        d
    }
}

impl Drop for AeDesc {
    fn drop(&mut self) {
        // SAFETY: the descriptor was initialised in `new` and then only filled by the Apple Event
        // functions; disposing frees what they allocated and is harmless for an empty one.
        unsafe { AEDisposeDesc(self) };
    }
}

fn os_at_least(major: isize, minor: isize) -> bool {
    let v = NSOperatingSystemVersion { majorVersion: major, minorVersion: minor, patchVersion: 0 };
    NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(v)
}

fn trim_slash(p: &str) -> &str {
    if p.len() > 1 {
        p.trim_end_matches('/')
    } else {
        p
    }
}

/// True for a folder whose name ends in `.app`.
fn is_app_path(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")) && p.is_dir()
}

/// The bundle id of the app at `path`, if it is a readable bundle.
fn bundle_id(path: &Path) -> Option<String> {
    let b = NSBundle::bundleWithPath(&NSString::from_str(&path.to_string_lossy()))?;
    b.bundleIdentifier().map(|s| s.to_string()).filter(|s| !s.is_empty())
}

/// This copy's `Sessionmux.app`, when it runs from one (a development build has none).
fn our_bundle() -> Option<PathBuf> {
    let main = NSBundle::mainBundle();
    let path = PathBuf::from(main.bundlePath().to_string());
    (is_app_path(&path) && main.bundleIdentifier().is_some()).then_some(path)
}

/// The app at `path` is Sessionmux: this copy's bundle, or any bundle with Sessionmux's id.
fn is_us_at(path: &str) -> bool {
    let path = trim_slash(path.trim());
    if path.is_empty() {
        return false;
    }
    if our_bundle().is_some_and(|b| <Os as Paths>::same_path(&b.to_string_lossy(), path)) {
        return true;
    }
    bundle_id(Path::new(path)).is_some_and(|id| id == brand::BUNDLE_ID)
}

/// The Claude.app of a located Claude: its install folder, else the bundle around its program.
fn claude_bundle(l: &Located) -> Option<PathBuf> {
    let dir = l.install_dir.trim();
    if !dir.is_empty() {
        return Some(PathBuf::from(dir)).filter(|p| is_app_path(p));
    }
    // <bundle>/Contents/MacOS/<program>
    Path::new(l.exe.trim()).ancestors().nth(3).map(Path::to_path_buf).filter(|p| is_app_path(p))
}

/// Where a restore gives the links back to: the saved app while it still exists and is not
/// Sessionmux, else the located Claude; None when neither is usable.
fn restore_target(prev: Option<&HandlerId>, l: Option<&Located>) -> Option<PathBuf> {
    prev.map(|h| PathBuf::from(trim_slash(h.0.trim())))
        .filter(|p| is_app_path(p) && !is_us_at(&p.to_string_lossy()))
        .or_else(|| l.and_then(claude_bundle))
}

/// The handler of `scheme` reads as the app at `app`.
fn reads_as(scheme: &str, app: &Path) -> bool {
    <Os as UrlHandler>::current(scheme).is_some_and(|h| <Os as Paths>::same_path(&h.0, &app.to_string_lossy()))
}

/// Read the handler every 100 ms for up to `wait` until it is `app`.
fn wait_until_reads(scheme: &str, app: &Path, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        if reads_as(scheme, app) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL);
    }
}

/// What the system answered to a handler change.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Answer {
    /// Nothing yet (a prompt is open, or the system is slow).
    None,
    Ok,
    Err {
        domain: String,
        code: isize,
    },
}

impl Answer {
    /// The user chose to keep the other app.
    fn is_refusal(&self) -> bool {
        match self {
            Answer::Err { domain, code } => {
                (domain == "NSCocoaErrorDomain" && *code == NS_USER_CANCELLED) || *code == ERR_USER_CANCELED
            }
            _ => false,
        }
    }
}

/// The outcome of a change from the system's answer and whether the handler reads as asked.
/// Err(text) for an unexpected error.
fn outcome(answer: &Answer, reads: bool) -> Result<HandlerChange, String> {
    if reads {
        return Ok(HandlerChange::Done);
    }
    match answer {
        Answer::None => Ok(HandlerChange::Pending),
        // Finished while another app is still the handler: the user kept it.
        Answer::Ok => Ok(HandlerChange::Declined),
        a if a.is_refusal() => Ok(HandlerChange::Declined),
        Answer::Err { domain, code } => Err(format!("NSWorkspace setDefaultApplication: {domain} code {code}")),
    }
}

/// The older Launch Services call, by bundle id. True when the handler then reads as `app`.
fn set_by_bundle_id(scheme: &str, app: &Path) -> Result<(), String> {
    let id = bundle_id(app).ok_or_else(|| format!("{} has no bundle id", app.display()))?;
    // SAFETY: both arguments are live strings for the duration of the call.
    let status = unsafe { LSSetDefaultHandlerForURLScheme(&CFString::from_str(scheme), &CFString::from_str(&id)) };
    if status != 0 {
        return Err(format!("LSSetDefaultHandlerForURLScheme: code {status}"));
    }
    if wait_until_reads(scheme, app, READ_WAIT) {
        Ok(())
    } else {
        Err("LSSetDefaultHandlerForURLScheme: the handler did not change".into())
    }
}

/// Make the app at `app` the handler of `scheme`. Waits for the system's answer, so the app asks
/// only from worker threads. On the main thread (the command-line `--end-relay`, which has no
/// window) the change runs on a helper thread and is waited for.
fn change_handler(scheme: &str, app: &Path) -> HandlerChange {
    if MainThreadMarker::new().is_none() {
        return change_handler_here(scheme, app);
    }
    let (s, a) = (scheme.to_string(), app.to_path_buf());
    std::thread::spawn(move || change_handler_here(&s, &a))
        .join()
        .unwrap_or_else(|_| HandlerChange::Failed("the link handler change stopped unexpectedly".into()))
}

/// [`change_handler`] on the calling thread.
fn change_handler_here(scheme: &str, app: &Path) -> HandlerChange {
    if reads_as(scheme, app) {
        return HandlerChange::Done;
    }
    let (tx, rx) = mpsc::channel::<Answer>();
    let done = RcBlock::new(move |err: *mut NSError| {
        // SAFETY: the system passes null or an error that is valid for the duration of this call;
        // only its domain and code are copied.
        let a = match unsafe { err.as_ref() } {
            None => Answer::Ok,
            Some(e) => Answer::Err { domain: e.domain().to_string(), code: e.code() },
        };
        let _ = tx.send(a);
    });
    let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&app.to_string_lossy()), true);
    NSWorkspace::sharedWorkspace().setDefaultApplicationAtURL_toOpenURLsWithScheme_completionHandler(
        &url,
        &NSString::from_str(scheme),
        Some(&done),
    );
    let answer = rx.recv_timeout(COMPLETION_WAIT).unwrap_or(Answer::None);
    let reads = wait_until_reads(scheme, app, READ_WAIT);
    let r = match outcome(&answer, reads) {
        Ok(r) => r,
        // Where every change asks the user, a second call would show a second prompt.
        Err(e) if <Os as UrlHandler>::changes_need_consent() => HandlerChange::Failed(e),
        Err(e) => match set_by_bundle_id(scheme, app) {
            Ok(()) => HandlerChange::Done,
            Err(e2) => HandlerChange::Failed(format!("{e}; {e2}")),
        },
    };
    log::info(&format!("url handler: {scheme} -> {}: {r:?}", app.display()));
    r
}

// ───────────────────────────────────────────────────────────── forward ──

/// The ways a link reaches one Claude instance, in the order tried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Forward {
    /// A get-URL Apple Event sent to the instance's process id (asks for Automation once).
    AppleEvent,
    /// A new start of Claude on the instance's data folder with the link, which the running
    /// instance would take over. Only where Claude hands a second start over.
    Relaunch,
}

/// The ways to try, best first.
pub(crate) fn forward_order() -> &'static [Forward] {
    if RELAUNCH_HANDOFF && RELAUNCH_PASSES_LINK {
        &[Forward::AppleEvent, Forward::Relaunch]
    } else {
        &[Forward::AppleEvent]
    }
}

/// `l` with its bundle filled in when only Claude's program is known.
fn full_located(l: &Located) -> Result<Located, String> {
    if !l.install_dir.trim().is_empty() {
        return Ok(l.clone());
    }
    let bundle = claude_bundle(l).ok_or_else(|| "Claude Desktop was not found".to_string())?;
    located_at(&bundle)
}

/// The main process of the Claude instance on `data_dir` (None = Main): `pid` when it still is
/// that instance, else the one running on the folder, else a newly started one.
fn target_pid(l: &Located, data_dir: Option<&str>, pid: Option<Pid>) -> Result<Pid, String> {
    let mains = claude_mains(Some(l));
    if let Some(p) = pid.filter(|&p| mains.iter().any(|m| m.pid == p && claude_app::matches_data_dir(m, data_dir))) {
        return Ok(p);
    }
    if let Some(m) = mains.iter().find(|m| claude_app::matches_data_dir(m, data_dir)) {
        return Ok(m.pid);
    }
    let started = match data_dir.map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => <Os as Launcher>::launch(l, d)?,
        None => <Os as Launcher>::launch_main(l)?.ok_or("Claude started without a process id")?,
    };
    let deadline = Instant::now() + WINDOW_WAIT;
    while <Os as Windows>::main_window(started).is_none() && Instant::now() < deadline {
        if !<Os as Processes>::alive(started) {
            return Err("Claude ended right after it started".into());
        }
        std::thread::sleep(POLL);
    }
    log::info(&format!("url handler: started Claude (pid {started}) to receive a link"));
    Ok(started)
}

/// Why an Apple Event to one process was not sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AeSendError {
    /// The user refused Automation for this target (`-1743`), or has not been asked yet and no
    /// prompt was allowed (`-1744`). Carries the code.
    NotPermitted(i32),
    /// The target process is gone.
    Gone,
    /// The target did not answer in time, with what timed out.
    NoAnswer(String),
    /// Any other failure, with the API name and code.
    Failed(String),
}

/// Send the Apple Event `class`/`id` to process `pid` and nothing else, without waiting for an
/// answer; the receiver may come forward. `text` is put in as the direct object (UTF-8). With
/// `ask`, the user is asked for Automation permission the first time (blocking while the prompt
/// is open, up to [`ASK_WAIT`]); without it, a target the user was never asked about is
/// `NotPermitted(-1744)`. The system's permission check waits on the target, so a stopped or
/// hung process gives up after [`SEND_WAIT`] instead of blocking the caller.
pub(crate) fn send_event_to_pid(
    pid: Pid,
    class: u32,
    id: u32,
    text: Option<&str>,
    ask: bool,
) -> Result<(), AeSendError> {
    let text = text.map(str::to_owned);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("apple-event".into())
        .spawn(move || {
            let _ = tx.send(send_event_now(pid, class, id, text.as_deref(), ask));
        })
        .map_err(|e| AeSendError::Failed(format!("apple event thread: {e}")))?;
    let wait = if ask { ASK_WAIT } else { SEND_WAIT };
    rx.recv_timeout(wait)
        .unwrap_or_else(|_| Err(AeSendError::NoAnswer(format!("pid {pid} did not answer within {} s", wait.as_secs()))))
}

fn send_event_now(pid: Pid, class: u32, id: u32, text: Option<&str>, ask: bool) -> Result<(), AeSendError> {
    let p = libc::pid_t::try_from(pid).ok().filter(|&p| p > 0).ok_or_else(|| AeSendError::Failed("bad pid".into()))?;
    let mut target = AeDesc::new();
    // SAFETY: the data is the 4-byte pid this frame owns; `target` is an initialised descriptor
    // that receives a copy of it.
    let err = unsafe { AECreateDesc(TYPE_KERNEL_PROCESS_ID, (&p as *const libc::pid_t).cast(), 4, &mut target) };
    if err != 0 {
        return Err(AeSendError::Failed(format!("AECreateDesc: code {err}")));
    }
    // SAFETY: `target` is a valid address descriptor; the call only reads it.
    let perm = unsafe { AEDeterminePermissionToAutomateTarget(&target, class, id, u8::from(ask)) };
    match perm {
        0 => {}
        ERR_NOT_PERMITTED | ERR_NEEDS_CONSENT => return Err(AeSendError::NotPermitted(perm)),
        ERR_PROC_NOT_FOUND => return Err(AeSendError::Gone),
        other => log::debug(&format!("apple event: Automation check for pid {pid}: code {other}")),
    }
    let mut event = AeDesc::new();
    // SAFETY: `target` is a valid address descriptor and `event` an initialised descriptor that
    // receives the new event.
    let err =
        unsafe { AECreateAppleEvent(class, id, &target, AUTO_GENERATE_RETURN_ID, ANY_TRANSACTION_ID, &mut event) };
    if err != 0 {
        return Err(AeSendError::Failed(format!("AECreateAppleEvent: code {err}")));
    }
    if let Some(text) = text {
        let len = libc::c_long::try_from(text.len()).map_err(|_| AeSendError::Failed("text too long".into()))?;
        // SAFETY: `event` is the event created above; the data is the text's bytes, valid for the
        // call, and copied into the event.
        let err = unsafe { AEPutParamPtr(&mut event, KEY_DIRECT_OBJECT, TYPE_UTF8_TEXT, text.as_ptr().cast(), len) };
        if err != 0 {
            return Err(AeSendError::Failed(format!("AEPutParamPtr: code {err}")));
        }
    }
    let mut reply = AeDesc::new();
    // SAFETY: `event` is a complete Apple Event and `reply` an initialised descriptor; with no
    // reply asked for, the system leaves it empty.
    let status = unsafe { AESendMessage(&event, &mut reply, SEND_MODE, SEND_TIMEOUT_TICKS) };
    match status {
        0 => Ok(()),
        ERR_NOT_PERMITTED | ERR_NEEDS_CONSENT => Err(AeSendError::NotPermitted(status)),
        ERR_PROC_NOT_FOUND => Err(AeSendError::Gone),
        ERR_AE_TIMEOUT => Err(AeSendError::NoAnswer(format!("AESendMessage to pid {pid}: code {status}"))),
        other => Err(AeSendError::Failed(format!("AESendMessage to pid {pid}: code {other}"))),
    }
}

/// Send `url` as a get-URL Apple Event to process `pid` and nothing else. Asks the user for
/// Automation permission the first time (blocking while the prompt is open).
fn send_get_url(pid: Pid, url: &str) -> Result<(), ForwardError> {
    send_event_to_pid(pid, INTERNET_EVENT_CLASS, AE_GET_URL, Some(url), true).map_err(|e| match e {
        AeSendError::NotPermitted(_) => ForwardError::NotPermitted,
        AeSendError::Gone => ForwardError::Failed(format!("Claude (pid {pid}) is no longer running")),
        AeSendError::Failed(e) if e == "text too long" => ForwardError::Failed("link too long".into()),
        AeSendError::NoAnswer(e) | AeSendError::Failed(e) => ForwardError::Failed(e),
    })
}

/// Deliver `url` to the Claude instance of `data_dir` by an Apple Event to its process.
fn forward_by_event(l: &Located, data_dir: Option<&str>, pid: Option<Pid>, url: &str) -> Result<(), ForwardError> {
    let l = full_located(l)?;
    let pid = target_pid(&l, data_dir, pid)?;
    send_get_url(pid, url)
}

impl UrlHandler for Os {
    /// The app Launch Services opens for `<scheme>://` links, by its path.
    fn current(scheme: &str) -> Option<HandlerId> {
        let probe = NSURL::URLWithString(&NSString::from_str(&format!("{scheme}://x")))?;
        let app = NSWorkspace::sharedWorkspace().URLForApplicationToOpenURL(&probe)?;
        let path = app.path()?.to_string();
        Some(HandlerId(trim_slash(&path).to_string()))
    }

    /// Make this copy's Sessionmux.app the handler. Worker threads only.
    fn take(scheme: &str) -> HandlerChange {
        if let Some(why) = <Os as UrlHandler>::unavailable() {
            return HandlerChange::Failed(why);
        }
        match our_bundle() {
            Some(app) => change_handler(scheme, &app),
            None => HandlerChange::Failed("Sessionmux.app was not found".into()),
        }
    }

    /// Give the links back to the saved app, else the located Claude. With neither, nothing
    /// changes. Worker threads only.
    fn restore(scheme: &str, prev: Option<&HandlerId>, l: Option<&Located>) -> HandlerChange {
        match restore_target(prev, l) {
            Some(app) => change_handler(scheme, &app),
            None => {
                log::info(&format!("url handler: no app to give {scheme} links back to"));
                HandlerChange::Done
            }
        }
    }

    fn is_us(h: &HandlerId) -> bool {
        is_us_at(&h.0)
    }

    /// From macOS 26.4 on, the system asks the user about every handler change.
    fn changes_need_consent() -> bool {
        os_at_least(26, 4)
    }

    /// Quitting and logging out never change the handler (a prompt would outlive the app); the
    /// next start gives it back.
    fn restore_at_exit() -> bool {
        false
    }

    /// Claude makes itself the handler again when an instance starts; one sign-in takes it back
    /// once, and never where that would ask the user again.
    fn retake_limit() -> Option<u32> {
        if <Os as UrlHandler>::changes_need_consent() {
            Some(0)
        } else {
            Some(1)
        }
    }

    /// Routing needs the app bundle at a place it stays in.
    fn unavailable() -> Option<String> {
        if our_bundle().is_none() {
            return Some("Sign-in routing needs Sessionmux.app: run Sessionmux.app".into());
        }
        if <Os as Misc>::translocated() {
            return Some("Move Sessionmux to your Applications folder and open it from there".into());
        }
        None
    }

    /// Send the link to the instance of `data_dir` (started first when it does not run), trying
    /// each way of [`forward_order`] in turn.
    fn forward(l: &Located, data_dir: Option<&str>, pid: Option<Pid>, url: &str) -> Result<(), ForwardError> {
        let mut last = Err(ForwardError::Failed("no way to send links to Claude".into()));
        for way in forward_order() {
            last = match way {
                Forward::AppleEvent => forward_by_event(l, data_dir, pid, url),
                Forward::Relaunch => <Os as Launcher>::launch_url(l, data_dir, url).map_err(ForwardError::from),
            };
            match &last {
                Ok(()) => return Ok(()),
                Err(e) => log::warn(&format!("url handler: sending a link by {way:?} failed: {e}")),
            }
        }
        last
    }

    /// Links from Launch Services arrive as an Apple Event carrying one link, so no arguments
    /// can be injected there; nothing is signalled.
    fn signal_bad_link() {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::macos::claude::fake::{self, wait_for};
    use crate::platform::Closer;

    fn err(domain: &str, code: isize) -> Answer {
        Answer::Err { domain: domain.into(), code }
    }

    #[test]
    fn change_outcomes() {
        assert_eq!(outcome(&Answer::Ok, true), Ok(HandlerChange::Done));
        assert_eq!(outcome(&Answer::None, true), Ok(HandlerChange::Done));
        assert_eq!(outcome(&err("X", 5), true), Ok(HandlerChange::Done));
        // No answer yet is not a failure.
        assert_eq!(outcome(&Answer::None, false), Ok(HandlerChange::Pending));
        assert_eq!(outcome(&Answer::Ok, false), Ok(HandlerChange::Declined));
        assert_eq!(outcome(&err("NSCocoaErrorDomain", NS_USER_CANCELLED), false), Ok(HandlerChange::Declined));
        assert_eq!(outcome(&err("NSOSStatusErrorDomain", ERR_USER_CANCELED), false), Ok(HandlerChange::Declined));
        assert!(outcome(&err("NSOSStatusErrorDomain", -10814), false).is_err());
        assert!(outcome(&err("NSPOSIXErrorDomain", NS_USER_CANCELLED), false).is_err());
    }

    #[test]
    fn only_the_apple_event_forward_is_used() {
        if RELAUNCH_HANDOFF && RELAUNCH_PASSES_LINK {
            return;
        }
        assert_eq!(forward_order(), &[Forward::AppleEvent]);
    }

    #[test]
    fn consent_follows_the_system_version() {
        assert_eq!(Os::changes_need_consent(), os_at_least(26, 4));
        assert_eq!(Os::retake_limit(), Some(if Os::changes_need_consent() { 0 } else { 1 }));
        assert!(!Os::restore_at_exit());
        Os::signal_bad_link();
    }

    #[test]
    fn a_build_outside_the_app_cannot_route() {
        // The test program is not inside Sessionmux.app.
        assert!(our_bundle().is_none());
        assert!(Os::unavailable().is_some_and(|w| w.contains("Sessionmux.app")));
        assert!(matches!(Os::take(fake::SCHEME), HandlerChange::Failed(_)));
    }

    /// A minimal app bundle at `dir/<name>.app` with bundle id `id`.
    fn bundle(dir: &Path, name: &str, id: &str) -> PathBuf {
        let app = dir.join(format!("{name}.app"));
        std::fs::create_dir_all(app.join("Contents").join("MacOS")).unwrap();
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
             <key>CFBundleIdentifier</key><string>{id}</string>\
             <key>CFBundleExecutable</key><string>{name}</string>\
             <key>CFBundlePackageType</key><string>APPL</string></dict></plist>\n"
        );
        std::fs::write(app.join("Contents").join("Info.plist"), plist).unwrap();
        app
    }

    #[test]
    fn sessionmux_is_recognised_by_its_bundle_id() {
        let d = tempfile::tempdir().unwrap();
        let ours = bundle(d.path(), "Sessionmux", brand::BUNDLE_ID);
        let other = bundle(d.path(), "Other", "test.sessionmux.other");
        assert!(Os::is_us(&HandlerId(ours.to_string_lossy().into_owned())));
        assert!(Os::is_us(&HandlerId(format!("{}/", ours.display()))));
        assert!(!Os::is_us(&HandlerId(other.to_string_lossy().into_owned())));
        assert!(!Os::is_us(&HandlerId(String::new())));
        assert!(!Os::is_us(&HandlerId("/nowhere/Sessionmux.app".into())));
    }

    #[test]
    fn restore_targets() {
        let d = tempfile::tempdir().unwrap();
        let ours = bundle(d.path(), "Sessionmux", brand::BUNDLE_ID);
        let saved = bundle(d.path(), "Saved", "test.sessionmux.saved");
        let claude = bundle(d.path(), "Claude", "test.sessionmux.claude");
        let l =
            Located { exe: claude.join("Contents/MacOS/Claude").to_string_lossy().into_owned(), ..Located::default() };
        let h = |p: &Path| HandlerId(p.to_string_lossy().into_owned());

        assert_eq!(restore_target(Some(&h(&saved)), Some(&l)), Some(saved.clone()));
        // Sessionmux itself, or an app that is gone, is never given the links back.
        assert_eq!(restore_target(Some(&h(&ours)), Some(&l)), Some(claude.clone()));
        assert_eq!(restore_target(Some(&HandlerId("/nowhere/Old.app".into())), Some(&l)), Some(claude.clone()));
        assert_eq!(restore_target(None, Some(&l)), Some(claude.clone()));
        let with_dir = Located { install_dir: claude.to_string_lossy().into_owned(), ..Located::default() };
        assert_eq!(restore_target(None, Some(&with_dir)), Some(claude.clone()));
        assert_eq!(restore_target(None, None), None);
        assert_eq!(restore_target(Some(&h(&ours)), None), None);
        // Nothing usable: nothing changes, and that is not an error.
        assert_eq!(Os::restore(fake::SCHEME, None, None), HandlerChange::Done);
    }

    #[test]
    fn current_handler_of_an_unknown_scheme_is_none() {
        assert_eq!(Os::current("sessionmux-test-unclaimed-scheme"), None);
    }

    #[link(name = "CoreServices", kind = "framework")]
    extern "C" {
        fn LSRegisterURL(url: *const c_void, update: u8) -> i32;
        fn LSCopyDefaultHandlerForURLScheme(scheme: &CFString) -> *const CFString;
    }

    const LSREGISTER: &str = concat!(
        "/System/Library/Frameworks/CoreServices.framework/Frameworks/",
        "LaunchServices.framework/Support/lsregister"
    );

    /// Tell Launch Services about the app at `app`, including the schemes it declares.
    fn register(app: &Path) {
        let forced = std::process::Command::new(LSREGISTER).arg("-f").arg(app).status();
        if !forced.is_ok_and(|s| s.success()) {
            let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&app.to_string_lossy()), true);
            // SAFETY: an NSURL is accepted wherever a CFURL is; `url` lives for the call.
            let status = unsafe { LSRegisterURL(objc2::rc::Retained::as_ptr(&url).cast(), 1) };
            assert_eq!(status, 0, "LSRegisterURL {}", app.display());
        }
    }

    /// Removes the apps from Launch Services when dropped, before their folder goes.
    struct Unregister(Vec<PathBuf>);

    impl Drop for Unregister {
        fn drop(&mut self) {
            for app in &self.0 {
                let _ = std::process::Command::new(LSREGISTER).arg("-u").arg(app).status();
            }
        }
    }

    /// Copy the stand-in Claude.app to `dir/<name>.app` under a bundle id no other copy has.
    /// Launch Services keeps a scheme's handler by bundle id, so copies that share one (other
    /// tests build their own stand-ins) could be read back in place of this one.
    fn app_copy(f: &fake::FakeClaude, dir: &Path, name: &str) -> PathBuf {
        let other = dir.join(format!("{name}.app"));
        let status = std::process::Command::new("/bin/cp").arg("-R").arg(&f.bundle).arg(&other).status().unwrap();
        assert!(status.success());
        let plist = other.join("Contents").join("Info.plist");
        let id = format!("test.sessionmux.{}.{}", name.to_ascii_lowercase(), std::process::id());
        let text = std::fs::read_to_string(&plist).unwrap().replace(fake::BUNDLE_ID, &id);
        std::fs::write(&plist, text).unwrap();
        // Sign the whole bundle again so its signature covers the changed Info.plist; Launch
        // Services does not hand a scheme to a bundle whose signature does not match.
        let signed = std::process::Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(&other)
            .output()
            .unwrap();
        assert!(signed.status.success(), "codesign: {}", String::from_utf8_lossy(&signed.stderr));
        other
    }

    /// What Launch Services knows about the test scheme, for a failure message.
    fn scheme_report(apps: &[&Path]) -> String {
        // SAFETY: the scheme string lives for the call; the result follows the create rule and
        // is only read here (and left to the end of the test process).
        let default = unsafe { LSCopyDefaultHandlerForURLScheme(&CFString::from_str(fake::SCHEME)).as_ref() }
            .map(|id| id.to_string());
        let dump = std::process::Command::new(LSREGISTER).arg("-dump").output();
        let claims = match dump {
            Ok(o) => {
                let text = String::from_utf8_lossy(&o.stdout);
                let lines: Vec<&str> = text.lines().filter(|l| l.contains(fake::SCHEME)).take(8).collect();
                let mut found = Vec::new();
                for app in apps {
                    let name = app.to_string_lossy();
                    found.push(format!("{}: {}", name, text.contains(name.as_ref())));
                }
                format!("{} lines name the scheme {lines:?}; apps listed: {found:?}", lines.len())
            }
            Err(e) => format!("lsregister -dump: {e}"),
        };
        format!(
            "current {:?}; https handler {:?}; default bundle id {default:?}; {claims}",
            Os::current(fake::SCHEME),
            Os::current("https"),
        )
    }

    /// Wait for a change result: done now, or pending and then reading as asked.
    fn settles(r: HandlerChange, app: &Path) -> bool {
        match r {
            HandlerChange::Done => reads_as(fake::SCHEME, app),
            HandlerChange::Pending => wait_for(10, || reads_as(fake::SCHEME, app)),
            other => panic!(
                "handler change to {}: {other:?}; the handler reads as {:?}",
                app.display(),
                Os::current(fake::SCHEME)
            ),
        }
    }

    #[test]
    fn test_scheme_round_trip() {
        if Os::changes_need_consent() {
            println!("skipped: every handler change asks the user on this macOS version (26.4 or later)");
            return;
        }
        let f = match fake::build() {
            Ok(f) => f,
            Err(why) => {
                println!("skipped: {why}");
                return;
            }
        };
        // Launch Services offers no handler from apps under the temporary folder, so the copies
        // go in a folder of their own under ~/Applications, removed (and unregistered) at the end.
        let apps = PathBuf::from(std::env::var_os("HOME").expect("HOME")).join("Applications");
        std::fs::create_dir_all(&apps).unwrap();
        let d = tempfile::Builder::new().prefix("sessionmux-test-").tempdir_in(&apps).unwrap();
        let first = app_copy(&f, d.path(), "First");
        let second = app_copy(&f, d.path(), "Second");
        let _unregister = Unregister(vec![first.clone(), second.clone()]);
        register(&first);
        register(&second);
        // Taking the scheme does not need a handler to exist yet; when Launch Services offers
        // none, the take below fails with what it knows about the scheme.
        if !wait_for(10, || Os::current(fake::SCHEME).is_some()) {
            println!("no handler offered for the test scheme yet: {}", scheme_report(&[&first, &second]));
        }

        let (a, b) = (first.clone(), second.clone());
        let worker = std::thread::spawn(move || {
            assert!(
                settles(change_handler(fake::SCHEME, &a), &a),
                "first app takes the test scheme: {}",
                scheme_report(&[&a, &b])
            );
            let now = Os::current(fake::SCHEME).expect("a handler");
            assert!(<Os as Paths>::same_path(&now.0, &a.to_string_lossy()), "{now:?}");
            assert!(!Os::is_us(&now));

            assert!(settles(change_handler(fake::SCHEME, &b), &b), "second app takes the test scheme");
            let saved = HandlerId(a.to_string_lossy().into_owned());
            assert!(settles(Os::restore(fake::SCHEME, Some(&saved), None), &a), "restored to the saved app");

            // A saved app that is gone: the located Claude gets the links.
            let l = located_at(&b).expect("locate the second app");
            let gone = HandlerId("/nowhere/Gone.app".into());
            assert!(settles(Os::restore(fake::SCHEME, Some(&gone), Some(&l)), &b), "restored to Claude");
            // Already the handler: done without a change.
            assert_eq!(change_handler(fake::SCHEME, &b), HandlerChange::Done);
        });
        worker.join().expect("round trip on a worker thread");
    }

    /// Main processes of the stand-in, by data folder.
    fn pid_on(l: &Located, dir: &str) -> Option<Pid> {
        claude_mains(Some(l)).into_iter().find(|p| claude_app::matches_data_dir(p, Some(dir))).map(|p| p.pid)
    }

    /// Forward on a worker thread; None when it did not return in time (a permission prompt
    /// nobody answers).
    fn forward_in_time(l: &Located, dir: &str, pid: Option<Pid>, url: &str) -> Option<Result<(), ForwardError>> {
        let (l, dir, url) = (l.clone(), dir.to_string(), url.to_string());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(Os::forward(&l, Some(&dir), pid, &url));
        });
        rx.recv_timeout(Duration::from_secs(60)).ok()
    }

    #[test]
    fn apple_event_reaches_only_the_targeted_instance() {
        let f = match fake::build() {
            Ok(f) => f,
            Err(why) => {
                println!("skipped: {why}");
                return;
            }
        };
        let l = located_at(&f.bundle).expect("locate the stand-in");
        let dir_a = f.data_dir("Claude-A").to_string_lossy().into_owned();
        let dir_b = f.data_dir("Claude-B").to_string_lossy().into_owned();
        let pid_a = Os::launch(&l, &dir_a).expect("start A");
        let pid_b = Os::launch(&l, &dir_b).expect("start B");
        assert!(wait_for(10, || f.recorded_args(pid_a).is_some() && f.recorded_args(pid_b).is_some()));
        let _ = wait_for(10, || Os::main_window(pid_a).is_some() && Os::main_window(pid_b).is_some());

        let url_a = format!("{}://forward/one", fake::SCHEME);
        let result = forward_in_time(&l, &dir_a, Some(pid_a), &url_a);
        match result {
            None => println!("Automation: the permission prompt was left open on this machine; delivery not checked"),
            Some(Err(ForwardError::NotPermitted)) => {
                println!("Automation: sending events to Claude is not allowed on this machine; delivery not checked")
            }
            Some(Err(ForwardError::Failed(e))) => panic!("forward to A failed: {e}"),
            Some(Ok(())) => {
                println!("Automation: allowed on this machine");
                assert!(wait_for(10, || f.recorded_urls(pid_a).contains(&url_a)), "A received its link");
                std::thread::sleep(Duration::from_millis(500));
                assert!(f.recorded_urls(pid_b).is_empty(), "B received nothing");

                // Without a pid, the instance is found by its data folder.
                let url_b = format!("{}://forward/two", fake::SCHEME);
                assert_eq!(forward_in_time(&l, &dir_b, None, &url_b).expect("in time"), Ok(()));
                assert!(wait_for(10, || f.recorded_urls(pid_b).contains(&url_b)), "B received its link");
                assert_eq!(f.recorded_urls(pid_a), vec![url_a.clone()], "A received only its own link");

                // A pid that is not this folder's instance is not used.
                let url_c = format!("{}://forward/three", fake::SCHEME);
                assert_eq!(forward_in_time(&l, &dir_a, Some(pid_b), &url_c).expect("in time"), Ok(()));
                assert!(wait_for(10, || f.recorded_urls(pid_a).contains(&url_c)));
                assert!(!f.recorded_urls(pid_b).contains(&url_c));

                // A folder that does not run is started to receive the link.
                let dir_c = f.data_dir("Claude-C").to_string_lossy().into_owned();
                let url_d = format!("{}://forward/four", fake::SCHEME);
                assert_eq!(forward_in_time(&l, &dir_c, None, &url_d).expect("in time"), Ok(()));
                let pid_c = pid_on(&l, &dir_c).expect("C was started");
                assert!(wait_for(10, || f.recorded_urls(pid_c).contains(&url_d)), "C received its link");
                let _ = Os::close(&[pid_c]);
            }
        }
        let _ = Os::close(&[pid_a, pid_b]);
    }

    #[test]
    fn forwarding_without_claude_fails_cleanly() {
        let l = Located { exe: "/nowhere/Claude.app/Contents/MacOS/Claude".into(), ..Located::default() };
        assert!(matches!(Os::forward(&l, None, None, "claude://x"), Err(ForwardError::Failed(_))));
        assert!(matches!(send_get_url(0, "claude://x"), Err(ForwardError::Failed(_))));
    }
}
