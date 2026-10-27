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

//! Claude Desktop on Windows: locate the install, launch an account, bring its window forward,
//! close it.
//!
//! - `locate` reads the per-user AppX repository in the registry (no Get-AppxPackage), else the
//!   direct installer's `%LOCALAPPDATA%\AnthropicClaude`.
//! - `close` force-terminates descendants only when their image is `claude.exe` or their exe
//!   lives inside a Claude folder (%APPDATA%\Claude*, %LOCALAPPDATA%\Claude-Data,
//!   %LOCALAPPDATA%\Packages\Claude_*, %LOCALAPPDATA%\AnthropicClaude, WindowsApps\Claude_*).
//!   Other descendants (MCP servers, shells) are left to exit when their pipes close, and the
//!   walk stops at them: nothing below a non-Claude intermediate is killed, and Desktop main
//!   processes of other accounts are never killed whatever their parent is.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::path::Path;
use std::time::{Duration, Instant};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, HANDLE, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::{
    AttachThreadInput, CreateProcessW, GetCurrentProcessId, GetCurrentThreadId, TerminateProcess, WaitForSingleObject,
    PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    PROCESS_TERMINATE, STARTUPINFOW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{keybd_event, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, PostMessageW,
    SetForegroundWindow, ShowWindow, ASFW_ANY, SW_RESTORE, SW_SHOWNORMAL, WM_CLOSE,
};

use super::process::{
    creation_time, image_path, in_claude_folder, is_claude_image, open_process, snapshot, Entry, Owned, UDD,
};
use super::win::{from_wide, wide};
use super::window::{hwnd_of, main_window, main_windows};
use crate::core::claude_app::{self, Located};
use crate::platform::{ClaudeInstall, Closer, Focus, FocusResult, Launcher, Os, Pid};
use crate::util::{log, paths};

const GRACEFUL_WAIT: Duration = Duration::from_secs(3);
const PACKAGES_KEY: &str =
    r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages";

// ───────────────────────────────────────────────────────────── package keys ──

/// A parsed AppX package key name `Claude_<ver>_<arch>__<publisherId>`.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PackageKey {
    version: Vec<u64>,
    pub(super) arch: String,
    pub(super) publisher_id: String,
}

impl PackageKey {
    /// "Claude_<publisherId>"
    fn family(&self) -> String {
        format!("Claude_{}", self.publisher_id)
    }
}

/// Parse `Claude_3.6556.2.0_x64__pzs8sxrjxfjjc`. Resource/bundle keys (`..._neutral_split...`)
/// and other packages whose name merely starts with "Claude" are rejected.
pub(super) fn parse_package_key(name: &str) -> Option<PackageKey> {
    let rest = name.strip_prefix("Claude_")?;
    let (ver, rest) = rest.split_once('_')?;
    let (arch, pubid) = rest.split_once("__")?;
    if arch.is_empty() || arch.contains('_') || pubid.is_empty() || pubid.contains('_') {
        return None;
    }
    if !pubid.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let version = parse_version(ver)?;
    Some(PackageKey { version, arch: arch.to_string(), publisher_id: pubid.to_string() })
}

/// "3.6556.2.0" → [3, 6556, 2, 0]; anything non-numeric → None.
fn parse_version(v: &str) -> Option<Vec<u64>> {
    if v.is_empty() {
        return None;
    }
    v.split('.').map(|p| p.parse::<u64>().ok()).collect()
}

/// Compare numerically, missing parts count as 0 ("1.2" == "1.2.0").
fn cmp_version(a: &[u64], b: &[u64]) -> std::cmp::Ordering {
    let n = a.len().max(b.len());
    for i in 0..n {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        match x.cmp(&y) {
            std::cmp::Ordering::Equal => continue,
            o => return o,
        }
    }
    std::cmp::Ordering::Equal
}

// ───────────────────────────────────────────────────────────── locate ──

fn reg_string(key: HKEY, subkey: &str, value: &str) -> Option<String> {
    let sk = wide(subkey);
    let vn = wide(value);
    let mut buf = vec![0u16; 1024];
    let mut cb = (buf.len() * 2) as u32;
    // SAFETY: sk/vn are NUL-terminated and live; buf is writable for cb bytes.
    let r = unsafe {
        RegGetValueW(
            key,
            PCWSTR(sk.as_ptr()),
            PCWSTR(vn.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut cb),
        )
    };
    if r != ERROR_SUCCESS {
        return None;
    }
    buf.truncate(cb as usize / 2);
    let s = from_wide(&buf);
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// (key name, PackageRootFolder) of every `Claude_*` package in the per-user repository.
fn registry_packages() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let path = wide(PACKAGES_KEY);
    let mut hkey = HKEY::default();
    // SAFETY: path is NUL-terminated; hkey is a valid out-pointer.
    let r = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), 0, KEY_READ, &mut hkey) };
    if r != ERROR_SUCCESS {
        return out;
    }
    let mut i = 0u32;
    loop {
        let mut name = vec![0u16; 512];
        let mut len = name.len() as u32;
        // SAFETY: hkey is open; name is writable for len u16s; optional args are None/null.
        let r = unsafe { RegEnumKeyExW(hkey, i, PWSTR(name.as_mut_ptr()), &mut len, None, PWSTR::null(), None, None) };
        if r == ERROR_NO_MORE_ITEMS {
            break;
        }
        i += 1;
        if r != ERROR_SUCCESS {
            if i > 100_000 {
                break;
            }
            continue;
        }
        name.truncate(len as usize);
        let key = String::from_utf16_lossy(&name);
        if key.starts_with("Claude_") {
            if let Some(root) = reg_string(hkey, &key, "PackageRootFolder") {
                out.push((key, root));
            }
        }
    }
    // SAFETY: hkey was opened above and is closed exactly once.
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    out
}

/// Choose the highest-version package whose `app\Claude.exe` exists.
fn pick_package(pkgs: &[(String, String)], exists: impl Fn(&str) -> bool) -> Option<(PackageKey, String)> {
    let mut cands: Vec<(PackageKey, String)> =
        pkgs.iter().filter_map(|(k, root)| parse_package_key(k).map(|pk| (pk, root.clone()))).collect();
    cands.sort_by(|a, b| cmp_version(&b.0.version, &a.0.version));
    cands.into_iter().find(|(_, root)| exists(&Path::new(root).join("app").join("Claude.exe").to_string_lossy()))
}

fn locate_uncached() -> Result<Located, String> {
    let local = paths::local_app_data();
    let roaming_claude = paths::app_data().join("Claude");

    if let Some((pk, root)) = pick_package(&registry_packages(), |p| Path::new(p).is_file()) {
        let family = pk.family();
        let family_lower = family.to_lowercase();
        let install_dir = root.trim_end_matches('\\').to_string();
        let exe = Path::new(&install_dir).join("app").join("Claude.exe");
        let new_data = local.join("Claude-Data").join(&family_lower);
        let local_cache = local.join("Packages").join(&family).join("LocalCache").join("Roaming").join("Claude");
        let main_data_dir = [&new_data, &local_cache, &roaming_claude]
            .into_iter()
            .find(|p| p.is_dir())
            .cloned()
            .unwrap_or_else(|| new_data.clone());
        let mut candidates: Vec<String> = Vec::new();
        for dir in [&main_data_dir, &roaming_claude, &local_cache, &new_data] {
            let f = dir.join("claude_desktop_config.json");
            let s = f.to_string_lossy().into_owned();
            if f.is_file() && !candidates.iter().any(|c| c.eq_ignore_ascii_case(&s)) {
                candidates.push(s);
            }
        }
        return Ok(Located {
            kind: "store",
            exe: exe.to_string_lossy().into_owned(),
            install_dir,
            aumid: Some(format!("{}!Claude", family)),
            family: Some(family),
            main_data_dir: main_data_dir.to_string_lossy().into_owned(),
            main_config_candidates: candidates,
        });
    }

    let inst_dir = local.join("AnthropicClaude");
    let inst_exe = inst_dir.join("claude.exe");
    if inst_exe.is_file() {
        let f = roaming_claude.join("claude_desktop_config.json");
        let candidates = if f.is_file() { vec![f.to_string_lossy().into_owned()] } else { Vec::new() };
        return Ok(Located {
            kind: "installer",
            exe: inst_exe.to_string_lossy().into_owned(),
            install_dir: inst_dir.to_string_lossy().into_owned(),
            aumid: None,
            family: None,
            main_data_dir: roaming_claude.to_string_lossy().into_owned(),
            main_config_candidates: candidates,
        });
    }
    Err("Claude Desktop is not installed (no Store package and no AnthropicClaude folder).".to_string())
}

// ───────────────────────────────────────────────────────────── focus ──

/// Bring the process's main window to the foreground. True if it is the foreground window now.
fn focus(pid: u32) -> bool {
    let h = main_window(pid);
    if h == 0 {
        return false;
    }
    let hwnd = hwnd_of(h);
    // SAFETY: all calls below take a window handle that may become invalid at any time; user32
    // validates handles and simply fails in that case. No memory is passed.
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
        if GetForegroundWindow() != hwnd {
            // Foreground lock: a synthetic Alt tap makes Windows accept the next request.
            keybd_event(0x12, 0, KEYBD_EVENT_FLAGS(0), 0);
            keybd_event(0x12, 0, KEYEVENTF_KEYUP, 0);
            let _ = SetForegroundWindow(hwnd);
        }
        if GetForegroundWindow() != hwnd {
            // Last resort: attach to the foreground thread's input queue.
            let fg = GetForegroundWindow();
            let fg_thread = GetWindowThreadProcessId(fg, None);
            let me = GetCurrentThreadId();
            if fg_thread != 0 && fg_thread != me {
                let _ = AttachThreadInput(me, fg_thread, true);
                let _ = BringWindowToTop(hwnd);
                let _ = SetForegroundWindow(hwnd);
                let _ = AttachThreadInput(me, fg_thread, false);
            } else {
                let _ = BringWindowToTop(hwnd);
                let _ = SetForegroundWindow(hwnd);
            }
        }
        GetForegroundWindow() == hwnd
    }
}

// ───────────────────────────────────────────────────────────── launch ──

/// Launch the Main account: `shell:AppsFolder\<aumid>` for the Store build, else the exe.
fn launch_main(l: &Located) -> Result<(), String> {
    let target = match l.aumid.as_deref() {
        Some(a) if !a.is_empty() => format!("shell:AppsFolder\\{}", a),
        _ => l.exe.clone(),
    };
    let op = wide("open");
    let file = wide(&target);
    // SEE_MASK_NOASYNC: we run on a short-lived worker thread, so ShellExecute must finish its
    // work before returning instead of handing it to a thread that is about to exit.
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: PCWSTR(op.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: COM init for ShellExecute on whatever thread we run on; balanced below only when
    // it succeeded (S_OK or S_FALSE).
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    // SAFETY: sei is fully initialised; op/file are NUL-terminated buffers alive for the call.
    let r = unsafe { ShellExecuteExW(&mut sei) };
    if com.is_ok() {
        // SAFETY: balances the successful CoInitializeEx above on this thread.
        unsafe { CoUninitialize() };
    }
    match r {
        Ok(()) => {
            log::info(&format!("claude_app: launched Main via {}", target));
            Ok(())
        }
        Err(e) => Err(format!("Could not start Claude ({}): {}", target, e.message())),
    }
}

/// Launch Claude Desktop with `--user-data-dir="<dir>"`. Returns the new pid.
fn launch(l: &Located, data_dir: &str) -> Result<u32, String> {
    let dir = data_dir.trim();
    if dir.is_empty() {
        return Err("launch: dataDir required".into());
    }
    if dir.contains('"') {
        return Err("launch: dataDir must not contain quotes".into());
    }
    // A trailing backslash would escape the closing quote in the command line.
    let dir = dir.trim_end_matches('\\');
    if dir.is_empty() {
        return Err("launch: dataDir required".into());
    }
    if !Path::new(&l.exe).is_file() {
        return Err(format!("launch: exe not found: {}", l.exe));
    }
    let pid = spawn(&l.exe, &build_launch_cmdline(&l.exe, dir))?;
    log::info(&format!("claude_app: launched pid {} for {}", pid, dir));
    Ok(pid)
}

/// CreateProcessW(`exe`, `cmdline`) without waiting; returns the new pid.
fn spawn(exe: &str, cmdline: &str) -> Result<u32, String> {
    let app = wide(exe);
    let mut cmd = wide(cmdline);
    let si = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, ..Default::default() };
    let mut pi = PROCESS_INFORMATION::default();
    // SAFETY: app is NUL-terminated; cmd is a writable NUL-terminated buffer (CreateProcessW may
    // modify it); si/pi are valid structs that live across the call.
    let r = unsafe {
        CreateProcessW(
            PCWSTR(app.as_ptr()),
            PWSTR(cmd.as_mut_ptr()),
            None,
            None,
            false,
            PROCESS_CREATION_FLAGS(0),
            None,
            PCWSTR::null(),
            &si,
            &mut pi,
        )
    };
    match r {
        Ok(()) => {
            drop(Owned(pi.hThread));
            drop(Owned(pi.hProcess));
            Ok(pi.dwProcessId)
        }
        Err(e) => Err(format!("Could not start Claude: {}", e.message())),
    }
}

fn build_launch_cmdline(exe: &str, dir: &str) -> String {
    format!("\"{}\" {}\"{}\"", exe, UDD, dir)
}

/// Command line that hands a `claude://` link to Claude Desktop:
/// `"<exe>" [--user-data-dir="<dir>"] "<url>"`. Pure (unit-tested). Quotes are refused in all
/// three parts; trailing backslashes of the dir are trimmed (as in [`launch`]) and those of the
/// url are doubled, so neither can escape its closing quote.
pub(super) fn build_url_cmdline(exe: &str, data_dir: Option<&str>, url: &str) -> Result<String, String> {
    if exe.trim().is_empty() || exe.contains('"') {
        return Err("open url: invalid exe path".into());
    }
    if url.is_empty() || url.contains('"') {
        return Err("open url: the link must be non-empty and must not contain quotes".into());
    }
    let mut s = match data_dir {
        None => format!("\"{}\"", exe),
        Some(d) => {
            let d = d.trim();
            if d.contains('"') {
                return Err("open url: dataDir must not contain quotes".into());
            }
            let d = d.trim_end_matches('\\');
            if d.is_empty() {
                return Err("open url: dataDir required".into());
            }
            build_launch_cmdline(exe, d)
        }
    };
    let trailing = url.len() - url.trim_end_matches('\\').len();
    s.push_str(" \"");
    s.push_str(url);
    s.push_str(&"\\".repeat(trailing));
    s.push('"');
    Ok(s)
}

/// Start `"<exe>" [--user-data-dir="<dir>"] "<url>"` (CreateProcessW, no wait). Claude Desktop's
/// single-instance lock then hands the url to the running instance of that data dir, if any.
/// Returns the pid of the (usually short-lived) process.
pub(super) fn launch_url(exe: &str, data_dir: Option<&str>, url: &str) -> Result<u32, String> {
    let cmd = build_url_cmdline(exe, data_dir, url)?;
    if !Path::new(exe).is_file() {
        return Err(format!("open url: exe not found: {}", exe));
    }
    spawn(exe, &cmd)
}

// ───────────────────────────────────────────────────────────── close ──

fn exited(h: HANDLE) -> bool {
    // SAFETY: h is a live process handle opened with SYNCHRONIZE.
    unsafe { WaitForSingleObject(h, 0) == WAIT_OBJECT_0 }
}

/// Descendants of `root` (deepest first). A child must not be older than its parent (pid-reuse
/// guard). Only children for which `keep(pid, name)` is true are returned, and traversal stops
/// at the first child that is not kept, so nothing reached through a non-Claude intermediate
/// (a terminal, an editor, another tool) is ever included. Returns (pid, creation time, name).
fn descendants(
    root: u32,
    root_created: Option<u64>,
    snap: &[Entry],
    self_pid: u32,
    keep: &mut dyn FnMut(u32, &str) -> bool,
) -> Vec<(u32, u64, String)> {
    let mut created: HashMap<u32, Option<u64>> = HashMap::new();
    created.insert(root, root_created);
    let mut ctime = |pid: u32| -> Option<u64> {
        *created
            .entry(pid)
            .or_insert_with(|| open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).and_then(|h| creation_time(h.0)))
    };
    let mut order = Vec::new();
    let mut seen: HashSet<u32> = [root].into_iter().collect();
    let mut queue = std::collections::VecDeque::from([root]);
    while let Some(cur) = queue.pop_front() {
        let parent_t = ctime(cur);
        for e in snap.iter().filter(|e| e.ppid == cur) {
            if e.pid == 0 || e.pid == self_pid || seen.contains(&e.pid) {
                continue;
            }
            let t = match ctime(e.pid) {
                Some(t) => t,
                None => continue, // cannot open it → cannot verify or terminate it
            };
            if let Some(pt) = parent_t {
                if t < pt {
                    continue; // older than its "parent": the parent pid was reused
                }
            }
            seen.insert(e.pid);
            if !keep(e.pid, &e.name) {
                continue; // non-Claude intermediate: neither kill it nor walk below it
            }
            order.push((e.pid, t, e.name.clone()));
            queue.push_back(e.pid);
        }
    }
    order.reverse();
    order
}

/// Close Claude processes: WM_CLOSE to each main window, wait ≤3 s, then terminate the whole
/// tree (descendants deepest-first, then the root). Only `claude.exe` roots are touched.
fn kill(pids: &[u32]) -> Result<(), String> {
    // SAFETY: GetCurrentProcessId has no preconditions.
    let self_pid = unsafe { GetCurrentProcessId() };
    let (app_data, local_app_data) =
        (paths::app_data().to_string_lossy().into_owned(), paths::local_app_data().to_string_lossy().into_owned());

    // Open every root first: the handle pins the pid so it cannot be reused under us.
    let mut targets: Vec<(u32, Owned, Option<u64>)> = Vec::new();
    for &pid in pids {
        if pid == 0 || pid == self_pid || targets.iter().any(|t| t.0 == pid) {
            continue;
        }
        let h = match open_process(pid, PROCESS_TERMINATE | PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION) {
            Some(h) => h,
            None => continue, // already gone (or not ours)
        };
        match image_path(h.0) {
            Some(p) if is_claude_image(&p) => {}
            other => {
                log::warn(&format!("claude_app: kill skipping non-Claude pid {} ({:?})", pid, other));
                continue;
            }
        }
        let t = creation_time(h.0);
        targets.push((pid, h, t));
    }
    if targets.is_empty() {
        return Ok(());
    }

    let set: HashSet<u32> = targets.iter().map(|t| t.0).collect();
    for (pid, hwnd) in main_windows(&set) {
        // SAFETY: posting a message to a possibly-stale window handle is harmless.
        if let Err(e) = unsafe { PostMessageW(hwnd_of(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) } {
            log::debug(&format!("claude_app: WM_CLOSE to pid {} failed: {}", pid, e.message()));
        }
    }

    let deadline = Instant::now() + GRACEFUL_WAIT;
    while Instant::now() < deadline && targets.iter().any(|t| !exited(t.1 .0)) {
        std::thread::sleep(Duration::from_millis(150));
    }

    // Claude Desktop hides to the tray instead of exiting, so force the remaining trees.
    let alive: Vec<&(u32, Owned, Option<u64>)> = targets.iter().filter(|t| !exited(t.1 .0)).collect();
    if alive.is_empty() {
        return Ok(());
    }
    // Desktop main processes of other accounts (e.g. launched by a tool running inside this
    // account's tree) are never part of this account's tree, whatever their parent is.
    let other_mains: HashSet<u32> =
        claude_app::list_processes().into_iter().map(|p| p.pid).filter(|p| !set.contains(p)).collect();
    let snap = snapshot();
    let mut failed: Vec<u32> = Vec::new();
    let mut keep = |cpid: u32, name: &str| -> bool {
        if other_mains.contains(&cpid) {
            log::debug(&format!("claude_app: kill leaves other Desktop instance {} alone", cpid));
            return false;
        }
        let exe =
            open_process(cpid, PROCESS_QUERY_LIMITED_INFORMATION).and_then(|h| image_path(h.0)).unwrap_or_default();
        let ok = is_claude_image(name) || is_claude_image(&exe) || in_claude_folder(&exe, &app_data, &local_app_data);
        if !ok {
            log::debug(&format!("claude_app: kill leaves descendant {} ({}) and its subtree alone", cpid, name));
        }
        ok
    };
    for (pid, h, t) in alive {
        for (cpid, ct, _name) in descendants(*pid, *t, &snap, self_pid, &mut keep) {
            let ch = match open_process(cpid, PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION) {
                Some(h) => h,
                None => continue,
            };
            if creation_time(ch.0) != Some(ct) {
                continue; // pid reused since the snapshot
            }
            // SAFETY: ch is a live handle with PROCESS_TERMINATE to the verified process.
            if let Err(e) = unsafe { TerminateProcess(ch.0, 1) } {
                log::debug(&format!("claude_app: terminate {} failed: {}", cpid, e.message()));
            }
        }
        if !exited(h.0) {
            // SAFETY: h is the root's handle opened with PROCESS_TERMINATE; it pins the pid.
            if let Err(e) = unsafe { TerminateProcess(h.0, 1) } {
                log::warn(&format!("claude_app: terminate root {} failed: {}", pid, e.message()));
            }
        }
        // SAFETY: h is live with SYNCHRONIZE; wait up to 2 s for the termination to complete.
        let _ = unsafe { WaitForSingleObject(h.0, 2000) };
        if !exited(h.0) {
            failed.push(*pid);
        }
    }
    if failed.is_empty() {
        log::info(&format!("claude_app: closed {:?}", pids));
        Ok(())
    } else {
        Err(format!("Could not close Claude process(es) {:?}", failed))
    }
}

impl ClaudeInstall for Os {
    /// Store package via the registry, else the direct installer.
    fn locate() -> Result<Located, String> {
        locate_uncached()
    }
}

impl Focus for Os {
    /// A window that does not come forward is shown by launching its folder again: Claude's
    /// single-instance lock hands over to the running instance, which shows it.
    fn focus(pid: Pid) -> FocusResult {
        if focus(pid) {
            FocusResult::Focused
        } else {
            FocusResult::NeedsHandOff
        }
    }

    /// Windows needs no permission to bring a window forward.
    fn accessibility_trusted(_prompt: bool) -> Option<bool> {
        None
    }
}

impl Launcher for Os {
    /// `shell:AppsFolder\<aumid>` for the Store build, else the exe. ShellExecute reports no pid.
    fn launch_main(l: &Located) -> Result<Option<Pid>, String> {
        launch_main(l).map(|()| None)
    }

    fn launch(l: &Located, data_dir: &str) -> Result<Pid, String> {
        launch(l, data_dir)
    }

    fn launch_url(l: &Located, data_dir: Option<&str>, url: &str) -> Result<(), String> {
        launch_url(&l.exe, data_dir, url).map(|_| ())
    }
}

impl Closer for Os {
    /// WM_CLOSE to each main window, wait up to 3 s, then terminate the whole tree.
    fn close(pids: &[Pid]) -> Result<(), String> {
        kill(pids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::windows::process::{parse_user_data_dir, split_cmdline};

    #[test]
    fn launch_cmdline_round_trips() {
        let exe = r"C:\Program Files\WindowsApps\Claude_1_x64__p\app\Claude.exe";
        let c = build_launch_cmdline(exe, r"C:\Users\a b\Claude-Q");
        let argv = split_cmdline(&c);
        assert_eq!(argv[0], exe);
        assert_eq!(parse_user_data_dir(&c).as_deref(), Some(r"C:\Users\a b\Claude-Q"));
    }

    #[test]
    fn url_cmdline_shapes() {
        let exe = r"C:\Program Files\WindowsApps\Claude_1_x64__p\app\Claude.exe";
        let url = "claude://auth/callback?code=a b&state=1";
        let c = build_url_cmdline(exe, None, url).unwrap();
        assert_eq!(c, format!("\"{exe}\" \"{url}\""));
        assert_eq!(split_cmdline(&c), vec![exe, url]);
        assert_eq!(parse_user_data_dir(&c), None);

        let c = build_url_cmdline(exe, Some("C:\\Users\\a b\\Claude-Q\\ "), url).unwrap();
        assert_eq!(c, format!("\"{exe}\" --user-data-dir=\"C:\\Users\\a b\\Claude-Q\" \"{url}\""));
        assert_eq!(parse_user_data_dir(&c).as_deref(), Some(r"C:\Users\a b\Claude-Q"));
        let argv = split_cmdline(&c);
        assert_eq!(argv.len(), 3);
        assert_eq!(argv[2], url);

        // Trailing backslashes of the url survive and do not escape the closing quote.
        let c = build_url_cmdline(exe, None, "claude://x\\").unwrap();
        assert_eq!(split_cmdline(&c), vec![exe, "claude://x\\"]);

        assert!(build_url_cmdline(exe, None, "claude://a\"b").is_err());
        assert!(build_url_cmdline(exe, None, "").is_err());
        assert!(build_url_cmdline(exe, Some("C:\\a\"b"), "claude://a").is_err());
        assert!(build_url_cmdline(exe, Some("  "), "claude://a").is_err());
        assert!(build_url_cmdline("", None, "claude://a").is_err());
    }

    #[test]
    fn url_launch_refuses_a_missing_exe() {
        // Nothing is started: the exe check comes first.
        assert!(launch_url(r"C:\definitely\missing\Claude.exe", None, "claude://x").is_err());
        assert!(<Os as Launcher>::launch_url(
            &Located { exe: r"C:\definitely\missing\Claude.exe".into(), ..Located::default() },
            Some(r"C:\D\Claude-Work"),
            "claude://x"
        )
        .is_err());
    }

    #[test]
    fn family_derivation() {
        let k = parse_package_key("Claude_3.6556.2.0_x64__pzs8sxrjxfjjc").unwrap();
        assert_eq!(k.family(), "Claude_pzs8sxrjxfjjc");
        assert_eq!(k.version, vec![3, 6556, 2, 0]);
        assert_eq!(k.arch, "x64");
        assert!(parse_package_key("Claude_3.1.0.0_neutral_split.scale-100_pzs8sxrjxfjjc").is_none());
        assert!(parse_package_key("ClaudeHelper_1.0.0.0_x64__abc").is_none());
        assert!(parse_package_key("Claude_x.y_x64__abc").is_none());
    }

    #[test]
    fn version_ordering() {
        let pkgs = vec![
            ("Claude_3.999.0.0_x64__p".to_string(), r"C:\W\Claude_3.999.0.0_x64__p".to_string()),
            ("Claude_3.6556.2.0_x64__p".to_string(), r"C:\W\Claude_3.6556.2.0_x64__p".to_string()),
            ("Claude_10.0.0.0_x64__p".to_string(), r"C:\W\Claude_10.0.0.0_x64__p".to_string()),
            ("Claude_3.6556.10.0_x64__p".to_string(), r"C:\W\Claude_3.6556.10.0_x64__p".to_string()),
        ];
        let (k, _) = pick_package(&pkgs, |_| true).unwrap();
        assert_eq!(k.version, vec![10, 0, 0, 0]);
        // highest whose exe is missing is skipped
        let (k, root) = pick_package(&pkgs, |p| !p.contains("10.0.0.0")).unwrap();
        assert_eq!(k.version, vec![3, 6556, 10, 0]);
        assert!(root.ends_with("Claude_3.6556.10.0_x64__p"));
        assert_eq!(cmp_version(&[1, 2], &[1, 2, 0]), std::cmp::Ordering::Equal);
    }
}
