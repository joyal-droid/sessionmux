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

//! Claude Desktop discovery and process control. No PowerShell, no WMI.
//!
//! API:
//! ```ignore
//! pub struct Located { pub kind: &'static str /* "store"|"installer" */, pub exe: String, pub install_dir: String,
//!                      pub aumid: Option<String>, pub family: Option<String>, pub main_data_dir: String,
//!                      pub main_config_candidates: Vec<String> }
//! pub struct Proc { pub pid: u32, pub exe: String, pub cmdline: String, pub data_dir: Option<String>, pub hwnd: isize }
//! pub fn locate() -> Result<Located, String>
//! pub fn set_fixture(l:Option<Located>)   // smoke demo fixture: fixed locate, no processes
//! pub fn list_processes() -> Vec<Proc>
//! pub fn matches_data_dir(p:&Proc, data_dir:Option<&str>) -> bool
//! pub fn focus(pid:u32) -> bool
//! pub fn foreground_pid() -> Option<u32>
//! pub fn launch_main(l:&Located) -> Result<(),String>
//! pub fn launch(l:&Located, data_dir:&str) -> Result<u32,String>
//! pub fn build_url_cmdline(exe:&str, data_dir:Option<&str>, url:&str) -> Result<String,String>
//! pub fn launch_url(exe:&str, data_dir:Option<&str>, url:&str) -> Result<u32,String>
//! pub fn wait_and_focus(data_dir:Option<&str>, timeout_ms:u64) -> Option<u32>
//! pub fn kill(pids:&[u32]) -> Result<(),String>
//! pub fn processes_under(dir:&str) -> Vec<(u32,String)>
//! ```
//!
//! Implementation notes:
//! - `locate` reads the per-user AppX repository in the registry (no Get-AppxPackage) and
//!   caches a successful result for 60 s.
//! - The command line of another process is read from its PEB (NtQueryInformationProcess +
//!   ReadProcessMemory). That works for same-user, non-elevated processes. When it cannot be
//!   read, `cmdline` stays empty and `data_dir` is `None`; such a `claude.exe` is listed as a
//!   Main process only if its parent is NOT itself a Claude Desktop process of the same install
//!   (helpers are always children of the main process, so this keeps helpers out without the
//!   `--type=` flag). It can mis-attribute an unreadable extra-account process to Main; that is
//!   the documented trade-off (in practice the PEB of a same-user Claude is always readable).
//! - `kill` force-terminates descendants only when their image is `claude.exe` or their exe
//!   lives inside a Claude folder (%APPDATA%\Claude*, %LOCALAPPDATA%\Claude-Data,
//!   %LOCALAPPDATA%\Packages\Claude_*, %LOCALAPPDATA%\AnthropicClaude, WindowsApps\Claude_*).
//!   Other descendants (MCP servers, shells) are left to exit when their pipes close, and the
//!   walk stops at them: nothing below a non-Claude intermediate is killed, and Desktop main
//!   processes of other accounts are never killed whatever their parent is.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::{PCWSTR, PWSTR};
use windows::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
use windows::Win32::Foundation::{
    CloseHandle, BOOL, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, FILETIME, HANDLE, HWND, LPARAM, RECT, WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::{
    AttachThreadInput, CreateProcessW, GetCurrentProcessId, GetCurrentThreadId, GetProcessTimes, OpenProcess,
    QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject, PEB, PROCESS_ACCESS_RIGHTS,
    PROCESS_BASIC_INFORMATION, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, PROCESS_VM_READ,
    RTL_USER_PROCESS_PARAMETERS, STARTUPINFOW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{keybd_event, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, BringWindowToTop, EnumWindows, GetForegroundWindow, GetWindow, GetWindowLongPtrW,
    GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible, PostMessageW, SetForegroundWindow, ShowWindow,
    ASFW_ANY, GWL_EXSTYLE, GW_OWNER, SW_RESTORE, SW_SHOWNORMAL, WM_CLOSE, WS_EX_TOOLWINDOW,
};

use crate::util::{log, paths};

/// Where Claude Desktop is installed and where Main keeps its data.
#[derive(Debug, Clone, PartialEq)]
pub struct Located {
    /// "store" | "installer"
    pub kind: &'static str,
    pub exe: String,
    pub install_dir: String,
    pub aumid: Option<String>,
    pub family: Option<String>,
    pub main_data_dir: String,
    pub main_config_candidates: Vec<String>,
}

/// A running Claude Desktop main process.
#[derive(Debug, Clone, PartialEq)]
pub struct Proc {
    pub pid: u32,
    pub exe: String,
    pub cmdline: String,
    pub data_dir: Option<String>,
    pub hwnd: isize,
}

const LOCATE_TTL: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(500);
const GRACEFUL_WAIT: Duration = Duration::from_secs(3);
const PACKAGES_KEY: &str =
    r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages";
const UDD: &str = "--user-data-dir=";

static LOCATE_CACHE: Mutex<Option<(Instant, Located)>> = Mutex::new(None);

/// Demo installation for the smoke fixture: when set, `locate` returns it and `list_processes`
/// reports nothing running, so no real installation or process is ever read.
static FIXTURE: Mutex<Option<Located>> = Mutex::new(None);

/// Replace Claude Desktop discovery with a fixed result (`None` restores the real discovery).
pub fn set_fixture(l: Option<Located>) {
    if let Ok(mut g) = FIXTURE.lock() {
        *g = l;
    }
}

fn fixture() -> Option<Located> {
    FIXTURE.lock().ok().and_then(|g| g.clone())
}

// ───────────────────────────────────────────────────────────── pure helpers ──

/// A parsed AppX package key name `Claude_<ver>_<arch>__<publisherId>`.
#[derive(Debug, Clone, PartialEq)]
struct PackageKey {
    version: Vec<u64>,
    arch: String,
    publisher_id: String,
}

impl PackageKey {
    /// "Claude_<publisherId>"
    fn family(&self) -> String {
        format!("Claude_{}", self.publisher_id)
    }
}

/// Parse `Claude_3.6556.2.0_x64__pzs8sxrjxfjjc`. Resource/bundle keys (`..._neutral_split...`)
/// and other packages whose name merely starts with "Claude" are rejected.
fn parse_package_key(name: &str) -> Option<PackageKey> {
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

/// Split a command line the way `CommandLineToArgvW` / the MSVC runtime does: the program name
/// is delimited by quotes without escapes; for the other arguments `2n` backslashes + `"` give
/// `n` backslashes and toggle quoting, `2n+1` backslashes + `"` give `n` backslashes and a
/// literal quote, `""` inside quotes is a literal quote.
fn split_cmdline(cmd: &str) -> Vec<String> {
    let chars: Vec<char> = cmd.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let is_ws = |c: char| c == ' ' || c == '\t';

    // Program name.
    while i < chars.len() && is_ws(chars[i]) {
        i += 1;
    }
    if i >= chars.len() {
        return out;
    }
    let mut prog = String::new();
    if chars[i] == '"' {
        i += 1;
        while i < chars.len() && chars[i] != '"' {
            prog.push(chars[i]);
            i += 1;
        }
        i += 1; // closing quote (or past the end)
                // Anything glued to the closing quote still belongs to argv[0].
        while i < chars.len() && !is_ws(chars[i]) {
            prog.push(chars[i]);
            i += 1;
        }
    } else {
        while i < chars.len() && !is_ws(chars[i]) {
            prog.push(chars[i]);
            i += 1;
        }
    }
    out.push(prog);

    loop {
        while i < chars.len() && is_ws(chars[i]) {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let mut arg = String::new();
        let mut in_quotes = false;
        while i < chars.len() {
            let c = chars[i];
            if c == '\\' {
                let start = i;
                while i < chars.len() && chars[i] == '\\' {
                    i += 1;
                }
                let n = i - start;
                if i < chars.len() && chars[i] == '"' {
                    arg.extend(std::iter::repeat_n('\\', n / 2));
                    if n % 2 == 1 {
                        arg.push('"');
                        i += 1;
                    }
                    // even: the quote is processed by the next iteration as a delimiter
                } else {
                    arg.extend(std::iter::repeat_n('\\', n));
                }
            } else if c == '"' {
                if in_quotes && i + 1 < chars.len() && chars[i + 1] == '"' {
                    arg.push('"');
                    i += 2;
                } else {
                    in_quotes = !in_quotes;
                    i += 1;
                }
            } else if is_ws(c) && !in_quotes {
                break;
            } else {
                arg.push(c);
                i += 1;
            }
        }
        out.push(arg);
    }
    out
}

/// Value of `--user-data-dir` in a command line, or None. Handles
/// `--user-data-dir="C:\x y"`, `"--user-data-dir=C:\x y"` (Chromium relaunch) and
/// `--user-data-dir=C:\x`. Surrounding quotes, whitespace and trailing backslashes are trimmed.
pub(crate) fn parse_user_data_dir(cmdline: &str) -> Option<String> {
    let lower = cmdline.to_ascii_lowercase();
    let at = lower.find(UDD)?;

    let mut value: Option<String> = None;
    for a in split_cmdline(cmdline).iter().skip(1) {
        if a.len() >= UDD.len() && a.is_char_boundary(UDD.len()) && a[..UDD.len()].eq_ignore_ascii_case(UDD) {
            value = Some(a[UDD.len()..].to_string());
            break;
        }
    }
    if value.is_none() {
        // Fallback for anything argv splitting did not yield (e.g. the flag glued to argv[0]).
        // `to_ascii_lowercase` keeps byte offsets, so `at` indexes `cmdline` too.
        let after = &cmdline[at + UDD.len()..];
        value = Some(if let Some(q) = after.strip_prefix('"') {
            q.split('"').next().unwrap_or("").to_string()
        } else {
            after.split(|c: char| c.is_whitespace() || c == '"').next().unwrap_or("").to_string()
        });
    }
    let v = value?;
    let v = v.trim().trim_matches('"').trim().trim_end_matches(['\\', '/']);
    if v.is_empty() {
        None
    } else {
        Some(v.to_string())
    }
}

/// Lower-case, '/' → '\', no trailing separators, no surrounding quotes.
fn norm(p: &str) -> String {
    let mut s = p.trim().trim_matches('"').replace('/', "\\").to_lowercase();
    while s.len() > 1 && s.ends_with('\\') {
        s.pop();
    }
    s
}

/// True if `child` is `dir` or lies inside it (case-insensitive, no filesystem access).
fn under(child: &str, dir: &str) -> bool {
    let (c, d) = (norm(child), norm(dir));
    !c.is_empty() && !d.is_empty() && (c == d || (c.starts_with(&d) && c.as_bytes().get(d.len()) == Some(&b'\\')))
}

/// True if `exe` lives in any versioned folder of the same package as `install_dir`
/// (a Store update installs a new `Claude_<ver>_x64__<pub>` while the old one still runs),
/// or simply under `install_dir`.
fn exe_in_install(exe: &str, install_dir: &str) -> bool {
    if under(exe, install_dir) {
        return true;
    }
    let inst = norm(install_dir);
    let (parent, leaf) = match inst.rsplit_once('\\') {
        Some(x) => x,
        None => return false,
    };
    // Registry key names keep the original case; compare the parsed parts case-insensitively.
    let want = match parse_package_key(&restore_prefix(leaf)) {
        Some(k) => k,
        None => return false,
    };
    let e = norm(exe);
    let rest = match e.strip_prefix(parent).and_then(|r| r.strip_prefix('\\')) {
        Some(r) => r,
        None => return false,
    };
    let first = rest.split('\\').next().unwrap_or("");
    match parse_package_key(&restore_prefix(first)) {
        Some(k) => k.arch.eq_ignore_ascii_case(&want.arch) && k.publisher_id.eq_ignore_ascii_case(&want.publisher_id),
        None => false,
    }
}

/// `norm` lower-cases everything; restore the "Claude_" prefix parse_package_key expects.
fn restore_prefix(leaf: &str) -> String {
    match leaf.strip_prefix("claude_") {
        Some(r) => format!("Claude_{}", r),
        None => leaf.to_string(),
    }
}

/// Path heuristic used when the install could not be located.
fn looks_like_desktop_exe(exe: &str) -> bool {
    let e = norm(exe);
    e.contains("\\windowsapps\\claude_") || e.contains("\\anthropicclaude\\")
}

fn is_claude_image(name_or_path: &str) -> bool {
    let n = name_or_path.rsplit(['\\', '/']).next().unwrap_or(name_or_path);
    n.eq_ignore_ascii_case("claude.exe")
}

/// True if `exe` lives inside a Claude data/install folder (kill safety for descendants).
fn in_claude_folder(exe: &str, app_data: &str, local_app_data: &str) -> bool {
    let e = norm(exe);
    if e.is_empty() {
        return false;
    }
    let roaming = norm(app_data);
    let local = norm(local_app_data);
    let starts = |prefix: &str| e.starts_with(prefix);
    (!roaming.is_empty() && starts(&format!("{}\\claude", roaming)))
        || (!local.is_empty()
            && (starts(&format!("{}\\claude-data\\", local))
                || starts(&format!("{}\\packages\\claude_", local))
                || starts(&format!("{}\\anthropicclaude\\", local))))
        || e.contains("\\windowsapps\\claude_")
}

/// True if a command line names `dir` as a whole path (followed by '\', '"', whitespace or end).
fn cmdline_names(cmd: &str, dir: &str) -> bool {
    let d = norm(dir);
    if d.is_empty() || cmd.is_empty() {
        return false;
    }
    let c = cmd.replace('/', "\\").to_lowercase();
    let mut from = 0;
    while let Some(pos) = c[from..].find(&d) {
        let end = from + pos + d.len();
        match c[end..].chars().next() {
            None => return true,
            Some(ch) if ch == '\\' || ch == '"' || ch.is_whitespace() => return true,
            _ => {}
        }
        from = from + pos + 1;
        while from < c.len() && !c.is_char_boundary(from) {
            from += 1;
        }
    }
    false
}

/// True if a process (from `list_processes`) belongs to the account with `data_dir`
/// (None/empty = Main = no `--user-data-dir`). Exact folder match, case-insensitive.
pub fn matches_data_dir(p: &Proc, data_dir: Option<&str>) -> bool {
    match data_dir.map(str::trim).filter(|d| !d.is_empty()) {
        None => p.data_dir.as_deref().is_none_or(|d| d.trim().is_empty()),
        Some(want) => match p.data_dir.as_deref() {
            Some(have) if !have.trim().is_empty() => paths::same_path(have, want),
            _ => false,
        },
    }
}

// ───────────────────────────────────────────────────────────── Win32 plumbing ──

/// Closes a kernel handle on drop.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_invalid() && !self.0 .0.is_null() {
            // SAFETY: we own this handle (returned by OpenProcess/CreateToolhelp32Snapshot/...)
            // and close it exactly once here.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

fn open_process(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> Option<Owned> {
    if pid == 0 {
        return None;
    }
    // SAFETY: OpenProcess has no memory preconditions; failure is reported via Result.
    match unsafe { OpenProcess(access, false, pid) } {
        Ok(h) if !h.is_invalid() => Some(Owned(h)),
        _ => None,
    }
}

/// Full Win32 path of the process image.
fn image_path(h: HANDLE) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: buf is valid for `len` u16s and outlives the call; len is updated in place.
    let r = unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) };
    if r.is_err() {
        return None;
    }
    buf.truncate(len as usize);
    Some(String::from_utf16_lossy(&buf))
}

fn creation_time(h: HANDLE) -> Option<u64> {
    let (mut c, mut e, mut k, mut u) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: four valid out-pointers to FILETIME locals.
    unsafe { GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u) }.ok()?;
    Some(((c.dwHighDateTime as u64) << 32) | c.dwLowDateTime as u64)
}

/// Read a `T` (plain-old-data) from another process.
fn read_remote<T: Copy>(h: HANDLE, addr: usize) -> Option<T> {
    if addr == 0 {
        return None;
    }
    let mut v = std::mem::MaybeUninit::<T>::zeroed();
    let mut got = 0usize;
    // SAFETY: the destination is a properly sized, writable local; ReadProcessMemory copies at
    // most size_of::<T>() bytes and we only assume initialisation after a full read. T is only
    // instantiated with repr(C) Win32 structs for which any bit pattern is valid.
    unsafe {
        ReadProcessMemory(
            h,
            addr as *const c_void,
            v.as_mut_ptr() as *mut c_void,
            std::mem::size_of::<T>(),
            Some(&mut got),
        )
        .ok()?;
        if got != std::mem::size_of::<T>() {
            return None;
        }
        Some(v.assume_init())
    }
}

/// Command line of a (same-bitness or WOW64) process from its PEB. Needs
/// PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ.
fn read_cmdline(h: HANDLE) -> Option<String> {
    let mut pbi = PROCESS_BASIC_INFORMATION::default();
    let mut ret = 0u32;
    // SAFETY: pbi is a valid, correctly sized out-buffer for ProcessBasicInformation.
    let st = unsafe {
        NtQueryInformationProcess(
            h,
            ProcessBasicInformation,
            &mut pbi as *mut _ as *mut c_void,
            std::mem::size_of::<PROCESS_BASIC_INFORMATION>() as u32,
            &mut ret,
        )
    };
    if st.0 < 0 || pbi.PebBaseAddress.is_null() {
        return None;
    }
    let peb: PEB = read_remote(h, pbi.PebBaseAddress as usize)?;
    let params: RTL_USER_PROCESS_PARAMETERS = read_remote(h, peb.ProcessParameters as usize)?;
    let us = params.CommandLine;
    let n = (us.Length as usize / 2).min(32_768);
    if n == 0 || us.Buffer.is_null() {
        return Some(String::new());
    }
    let mut buf = vec![0u16; n];
    let mut got = 0usize;
    // SAFETY: buf holds n u16s; we ask for exactly n*2 bytes from the remote buffer.
    unsafe {
        ReadProcessMemory(h, us.Buffer.0 as *const c_void, buf.as_mut_ptr() as *mut c_void, n * 2, Some(&mut got))
            .ok()?;
    }
    buf.truncate(got / 2);
    Some(String::from_utf16_lossy(&buf))
}

#[derive(Debug, Clone)]
struct Entry {
    pid: u32,
    ppid: u32,
    name: String,
}

/// Toolhelp32 process snapshot.
fn snapshot() -> Vec<Entry> {
    let mut out = Vec::new();
    // SAFETY: plain API call; the returned handle is owned by `Owned` below.
    let snap = match unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) } {
        Ok(h) => Owned(h),
        Err(e) => {
            log::warn(&format!("claude_app: process snapshot failed: {}", e.message()));
            return out;
        }
    };
    let mut pe = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    // SAFETY: snap is a live snapshot handle; pe is a valid PROCESSENTRY32W with dwSize set.
    let mut ok = unsafe { Process32FirstW(snap.0, &mut pe) }.is_ok();
    while ok {
        out.push(Entry {
            pid: pe.th32ProcessID,
            ppid: pe.th32ParentProcessID,
            name: crate::util::win::from_wide(&pe.szExeFile),
        });
        // SAFETY: same handle and struct as above.
        ok = unsafe { Process32NextW(snap.0, &mut pe) }.is_ok();
    }
    out
}

/// pid → first visible, unowned top-level window (z-order), for the given pids (all if empty).
fn main_windows(pids: &HashSet<u32>) -> HashMap<u32, isize> {
    struct Ctx<'a> {
        pids: &'a HashSet<u32>,
        found: HashMap<u32, isize>,
    }
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        // SAFETY: lp is the &mut Ctx passed to EnumWindows below, alive for the whole enumeration
        // and only accessed from this (synchronous) callback.
        let ctx = &mut *(lp.0 as *mut Ctx);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != 0 && (ctx.pids.is_empty() || ctx.pids.contains(&pid)) && !ctx.found.contains_key(&pid) {
            let owned = matches!(GetWindow(hwnd, GW_OWNER), Ok(o) if !o.0.is_null());
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            let mut r = RECT::default();
            let sized = GetWindowRect(hwnd, &mut r).is_ok() && r.right > r.left && r.bottom > r.top;
            if !owned && IsWindowVisible(hwnd).as_bool() && ex & WS_EX_TOOLWINDOW.0 == 0 && sized {
                ctx.found.insert(pid, hwnd.0 as isize);
            }
        }
        BOOL(1)
    }
    let mut ctx = Ctx { pids, found: HashMap::new() };
    // SAFETY: the callback only dereferences the LPARAM as the Ctx that outlives this call.
    let _ = unsafe { EnumWindows(Some(cb), LPARAM(&mut ctx as *mut Ctx as isize)) };
    ctx.found
}

fn main_window(pid: u32) -> isize {
    let set: HashSet<u32> = [pid].into_iter().collect();
    main_windows(&set).get(&pid).copied().unwrap_or(0)
}

fn hwnd_of(v: isize) -> HWND {
    HWND(v as *mut c_void)
}

// ───────────────────────────────────────────────────────────── locate ──

fn reg_string(key: HKEY, subkey: &str, value: &str) -> Option<String> {
    let sk = crate::util::win::wide(subkey);
    let vn = crate::util::win::wide(value);
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
    let s = crate::util::win::from_wide(&buf);
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// (key name, PackageRootFolder) of every `Claude_*` package in the per-user repository.
fn registry_packages() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let path = crate::util::win::wide(PACKAGES_KEY);
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

/// Locate Claude Desktop (Store package via the registry, else the direct installer).
/// Successful results are cached for 60 s.
pub fn locate() -> Result<Located, String> {
    if let Some(l) = fixture() {
        return Ok(l);
    }
    if let Ok(g) = LOCATE_CACHE.lock() {
        if let Some((at, v)) = g.as_ref() {
            // A Store update can remove the old package folder while the cache is fresh.
            if at.elapsed() < LOCATE_TTL && Path::new(&v.exe).is_file() {
                return Ok(v.clone());
            }
        }
    }
    let v = locate_uncached()?;
    if let Ok(mut g) = LOCATE_CACHE.lock() {
        *g = Some((Instant::now(), v.clone()));
    }
    Ok(v)
}

// ───────────────────────────────────────────────────────────── processes ──

/// Running Claude Desktop main processes (helpers and the Claude Code CLI excluded).
pub fn list_processes() -> Vec<Proc> {
    if fixture().is_some() {
        return Vec::new();
    }
    let install_dir = locate().ok().map(|l| l.install_dir);
    let is_desktop_exe = |exe: &str| -> bool {
        if exe.is_empty() || norm(exe).contains("\\claude-code\\") {
            return false;
        }
        match install_dir.as_deref() {
            Some(d) => exe_in_install(exe, d),
            None => looks_like_desktop_exe(exe),
        }
    };

    let snap = snapshot();
    let by_pid: HashMap<u32, &Entry> = snap.iter().map(|e| (e.pid, e)).collect();
    let mut exe_cache: HashMap<u32, Option<String>> = HashMap::new();
    let mut exe_of = |pid: u32| -> Option<String> {
        exe_cache
            .entry(pid)
            .or_insert_with(|| open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).and_then(|h| image_path(h.0)))
            .clone()
    };

    let mut found: Vec<Proc> = Vec::new();
    for e in snap.iter().filter(|e| is_claude_image(&e.name)) {
        let h = match open_process(e.pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ) {
            Some(h) => Some(h),
            None => open_process(e.pid, PROCESS_QUERY_LIMITED_INFORMATION),
        };
        let h = match h {
            Some(h) => h,
            None => continue,
        };
        let cmd = read_cmdline(h.0);
        let exe = match image_path(h.0) {
            Some(p) => p,
            None => match cmd.as_deref().and_then(|c| split_cmdline(c).into_iter().next()) {
                Some(p) if !p.is_empty() => p,
                _ => continue,
            },
        };
        if !is_desktop_exe(&exe) {
            continue;
        }
        let cmdline = match cmd {
            Some(c) => {
                if c.contains("--type=") {
                    continue; // renderer/gpu/utility helpers
                }
                c
            }
            None => {
                // Unknown command line: a helper is always a child of a Desktop main process.
                let parent_is_desktop = by_pid
                    .get(&e.ppid)
                    .filter(|p| is_claude_image(&p.name))
                    .and_then(|p| exe_of(p.pid))
                    .is_some_and(|pe| is_desktop_exe(&pe));
                if parent_is_desktop {
                    continue;
                }
                String::new()
            }
        };
        let data_dir = parse_user_data_dir(&cmdline);
        found.push(Proc { pid: e.pid, exe, cmdline, data_dir, hwnd: 0 });
    }

    if !found.is_empty() {
        let pids: HashSet<u32> = found.iter().map(|p| p.pid).collect();
        let wins = main_windows(&pids);
        for p in &mut found {
            p.hwnd = wins.get(&p.pid).copied().unwrap_or(0);
        }
    }
    found
}

/// PID owning the current foreground window.
pub fn foreground_pid() -> Option<u32> {
    // SAFETY: GetForegroundWindow has no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() {
        return None;
    }
    let mut pid = 0u32;
    // SAFETY: fg may be stale; the API tolerates invalid handles (returns 0).
    unsafe { GetWindowThreadProcessId(fg, Some(&mut pid)) };
    if pid == 0 {
        None
    } else {
        Some(pid)
    }
}

/// Bring the process's main window to the foreground. True if it is the foreground window now.
pub fn focus(pid: u32) -> bool {
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

/// Launch the Main account: `shell:AppsFolder\<aumid>` for the Store build, else the exe.
pub fn launch_main(l: &Located) -> Result<(), String> {
    let target = match l.aumid.as_deref() {
        Some(a) if !a.is_empty() => format!("shell:AppsFolder\\{}", a),
        _ => l.exe.clone(),
    };
    let op = crate::util::win::wide("open");
    let file = crate::util::win::wide(&target);
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
pub fn launch(l: &Located, data_dir: &str) -> Result<u32, String> {
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
    let app = crate::util::win::wide(exe);
    let mut cmd = crate::util::win::wide(cmdline);
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
pub fn build_url_cmdline(exe: &str, data_dir: Option<&str>, url: &str) -> Result<String, String> {
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
pub fn launch_url(exe: &str, data_dir: Option<&str>, url: &str) -> Result<u32, String> {
    let cmd = build_url_cmdline(exe, data_dir, url)?;
    if !Path::new(exe).is_file() {
        return Err(format!("open url: exe not found: {}", exe));
    }
    spawn(exe, &cmd)
}

/// Poll every 500 ms until a process of the account has a window, then focus it.
pub fn wait_and_focus(data_dir: Option<&str>, timeout_ms: u64) -> Option<u32> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if let Some(p) = list_processes().into_iter().find(|p| matches_data_dir(p, data_dir) && p.hwnd != 0) {
            let focused = focus(p.pid);
            log::debug(&format!("claude_app: wait_and_focus pid {} focused={}", p.pid, focused));
            return Some(p.pid);
        }
        if Instant::now() + POLL > deadline {
            return None;
        }
        std::thread::sleep(POLL);
    }
}

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
pub fn kill(pids: &[u32]) -> Result<(), String> {
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
    let other_mains: HashSet<u32> = list_processes().into_iter().map(|p| p.pid).filter(|p| !set.contains(p)).collect();
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

/// Any process (not only Claude) whose exe lives under `dir` or whose command line names it.
/// Returns (pid, image name). Read-only.
pub fn processes_under(dir: &str) -> Vec<(u32, String)> {
    let d = dir.trim().trim_end_matches('\\');
    if d.is_empty() {
        return Vec::new();
    }
    // SAFETY: GetCurrentProcessId has no preconditions.
    let self_pid = unsafe { GetCurrentProcessId() };
    let mut out = Vec::new();
    for e in snapshot() {
        if e.pid == 0 || e.pid == 4 || e.pid == self_pid {
            continue;
        }
        let h = match open_process(e.pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ)
            .or_else(|| open_process(e.pid, PROCESS_QUERY_LIMITED_INFORMATION))
        {
            Some(h) => h,
            None => continue,
        };
        let hit = image_path(h.0).is_some_and(|exe| under(&exe, d))
            || read_cmdline(h.0).is_some_and(|c| cmdline_names(&c, d));
        if hit {
            out.push((e.pid, e.name));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc_with(dd: Option<&str>) -> Proc {
        Proc { pid: 1, exe: String::new(), cmdline: String::new(), data_dir: dd.map(String::from), hwnd: 0 }
    }

    #[test]
    fn udd_quoted_value() {
        let c = r#""C:\Program Files\WindowsApps\Claude_3.6556.2.0_x64__pzs8sxrjxfjjc\app\Claude.exe" --user-data-dir="C:\Users\example\AppData\Roaming\Claude-Design""#;
        assert_eq!(parse_user_data_dir(c).as_deref(), Some(r"C:\Users\example\AppData\Roaming\Claude-Design"));
    }

    #[test]
    fn udd_fully_quoted_with_spaces() {
        let c = r#""C:\x\Claude.exe" "--user-data-dir=C:\Users\a b\Claude-Work" --flag"#;
        assert_eq!(parse_user_data_dir(c).as_deref(), Some(r"C:\Users\a b\Claude-Work"));
        let c = r#""C:\x\Claude.exe" --user-data-dir="C:\Users\a b\Claude-Work" --other=1"#;
        assert_eq!(parse_user_data_dir(c).as_deref(), Some(r"C:\Users\a b\Claude-Work"));
    }

    #[test]
    fn udd_bare_and_trailing_backslash() {
        assert_eq!(
            parse_user_data_dir(r"C:\x\Claude.exe --user-data-dir=C:\D\Claude-X\ --a").as_deref(),
            Some(r"C:\D\Claude-X")
        );
        assert_eq!(parse_user_data_dir(r"C:\x\Claude.exe --USER-DATA-DIR=C:\D").as_deref(), Some(r"C:\D"));
        assert_eq!(parse_user_data_dir(r#""C:\x\Claude.exe""#), None);
        assert_eq!(parse_user_data_dir(r#""C:\x\Claude.exe" --user-data-dir="""#), None);
        assert_eq!(parse_user_data_dir(""), None);
    }

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
    fn split_rules() {
        assert_eq!(split_cmdline(r#"a.exe "x y" z\\"w" q\"r"#), vec!["a.exe", "x y", r"z\w", r#"q"r"#]);
        assert_eq!(split_cmdline(r#""C:\a b\c.exe" --k="v w""#), vec![r"C:\a b\c.exe", "--k=v w"]);
        assert!(split_cmdline("   ").is_empty());
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

    #[test]
    fn matches_data_dir_rules() {
        let main = proc_with(None);
        assert!(matches_data_dir(&main, None));
        assert!(matches_data_dir(&main, Some("")));
        assert!(!matches_data_dir(&main, Some(r"C:\Users\u\AppData\Roaming\Claude-X")));
        let x = proc_with(Some(r"C:\Users\U\AppData\Roaming\Claude-X"));
        assert!(matches_data_dir(&x, Some(r"c:\users\u\appdata\roaming\claude-x\")));
        assert!(!matches_data_dir(&x, Some(r"C:\Users\U\AppData\Roaming\Claude-XY")));
        assert!(!matches_data_dir(&x, None));
    }

    #[test]
    fn install_dir_siblings() {
        let inst = r"C:\Program Files\WindowsApps\Claude_3.6556.2.0_x64__pzs8sxrjxfjjc";
        assert!(exe_in_install(&format!(r"{}\app\Claude.exe", inst), inst));
        assert!(exe_in_install(
            r"C:\Program Files\WindowsApps\Claude_3.6000.0.0_x64__pzs8sxrjxfjjc\app\Claude.exe",
            inst
        ));
        assert!(!exe_in_install(r"C:\Program Files\WindowsApps\Claude_3.6000.0.0_x64__otherpub\app\Claude.exe", inst));
        assert!(!exe_in_install(r"C:\Users\u\AppData\Local\Claude-Data\claude_p\claude-code\2\claude.exe", inst));
        assert!(exe_in_install(
            r"C:\Users\u\AppData\Local\AnthropicClaude\app-1.0\claude.exe",
            r"C:\Users\u\AppData\Local\AnthropicClaude"
        ));
    }

    #[test]
    fn claude_folder_and_cmdline_names() {
        let (r, l) = (r"C:\Users\u\AppData\Roaming", r"C:\Users\u\AppData\Local");
        assert!(in_claude_folder(r"C:\Users\u\AppData\Roaming\Claude-Design\claude-code\2\claude.exe", r, l));
        assert!(in_claude_folder(r"C:\Users\u\AppData\Local\Claude-Data\claude_p\claude-code\x\claude.exe", r, l));
        assert!(!in_claude_folder(r"C:\Windows\System32\cmd.exe", r, l));
        assert!(!in_claude_folder(r"C:\Program Files\nodejs\node.exe", r, l));
        assert!(cmdline_names(r#"x.exe --user-data-dir="C:\D\Claude-A""#, r"c:\d\claude-a"));
        assert!(cmdline_names(r"x.exe C:\D\Claude-A\file.txt", r"C:\D\Claude-A\"));
        assert!(!cmdline_names(r"x.exe C:\D\Claude-AB", r"C:\D\Claude-A"));
        assert!(
            is_claude_image("Claude.exe") && is_claude_image(r"C:\a\claude.EXE") && !is_claude_image("claude.exe.bak")
        );
    }

    /// Read-only check against this machine: `cargo test -p sessionmux claude_app -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_locate_and_list() {
        let l = locate().expect("locate");
        println!("{:#?}", l);
        let procs = list_processes();
        for p in &procs {
            println!("pid={} hwnd={} data_dir={:?} exe={}", p.pid, p.hwnd, p.data_dir, p.exe);
        }
        println!("fg={:?}", foreground_pid());
        let me = processes_under(&l.main_data_dir);
        println!("under main_data_dir: {:?}", me);
    }
}
