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

//! Windows processes: a Toolhelp32 snapshot, image paths, start times and the command line read
//! from the PEB (NtQueryInformationProcess + ReadProcessMemory). No PowerShell, no WMI.
//!
//! - The command line of another process is readable for same-user, non-elevated processes.
//!   When it cannot be read, `cmdline` stays empty and `data_dir` is `None`; such a `claude.exe`
//!   is listed as a Main process only if its parent is NOT itself a Claude Desktop process of the
//!   same install (helpers are always children of the main process, so this keeps helpers out
//!   without the `--type=` flag). It can mis-attribute an unreadable extra-account process to
//!   Main; that is the documented trade-off (in practice the PEB of a same-user Claude is always
//!   readable).
//! - Path comparisons here follow Windows rules: case-insensitive, either separator.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;

use windows::core::PWSTR;
use windows::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED, FILETIME, HANDLE, STILL_ACTIVE};
use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, GetExitCodeProcess, GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PEB,
    PROCESS_ACCESS_RIGHTS, PROCESS_BASIC_INFORMATION, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_VM_READ, RTL_USER_PROCESS_PARAMETERS,
};

use super::claude::parse_package_key;
use super::win::from_wide;
use crate::core::claude_app::{Located, Proc};
use crate::platform::{Os, Pid, Processes};
use crate::util::log;

pub(super) const UDD: &str = "--user-data-dir=";

/// 100 ns intervals between 1601-01-01 (FILETIME) and 1970-01-01 (Unix).
const FILETIME_UNIX_OFFSET: u64 = 116_444_736_000_000_000;

// ───────────────────────────────────────────────────────────── command lines ──

/// Split a command line the way `CommandLineToArgvW` / the MSVC runtime does: the program name
/// is delimited by quotes without escapes; for the other arguments `2n` backslashes + `"` give
/// `n` backslashes and toggle quoting, `2n+1` backslashes + `"` give `n` backslashes and a
/// literal quote, `""` inside quotes is a literal quote.
pub(super) fn split_cmdline(cmd: &str) -> Vec<String> {
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
pub(super) fn parse_user_data_dir(cmdline: &str) -> Option<String> {
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

// ───────────────────────────────────────────────────────────── path rules ──

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

pub(super) fn is_claude_image(name_or_path: &str) -> bool {
    let n = name_or_path.rsplit(['\\', '/']).next().unwrap_or(name_or_path);
    n.eq_ignore_ascii_case("claude.exe")
}

/// True if `exe` lives inside a Claude data/install folder (kill safety for descendants).
pub(super) fn in_claude_folder(exe: &str, app_data: &str, local_app_data: &str) -> bool {
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

// ───────────────────────────────────────────────────────────── Win32 plumbing ──

/// Closes a kernel handle on drop.
pub(super) struct Owned(pub(super) HANDLE);

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

pub(super) fn open_process(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> Option<Owned> {
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
pub(super) fn image_path(h: HANDLE) -> Option<String> {
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

pub(super) fn creation_time(h: HANDLE) -> Option<u64> {
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
pub(super) struct Entry {
    pub(super) pid: u32,
    pub(super) ppid: u32,
    pub(super) name: String,
}

/// Toolhelp32 process snapshot.
pub(super) fn snapshot() -> Vec<Entry> {
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
        out.push(Entry { pid: pe.th32ProcessID, ppid: pe.th32ParentProcessID, name: from_wide(&pe.szExeFile) });
        // SAFETY: same handle and struct as above.
        ok = unsafe { Process32NextW(snap.0, &mut pe) }.is_ok();
    }
    out
}

// ───────────────────────────────────────────────────────────── listing ──

/// Running Claude Desktop main processes (helpers and the Claude Code CLI excluded). With the
/// installation unknown, a path heuristic decides what counts as Claude Desktop.
fn list_claude(l: Option<&Located>) -> Vec<Proc> {
    let install_dir = l.map(|l| l.install_dir.clone());
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
        let wins = super::window::main_windows(&pids);
        for p in &mut found {
            p.hwnd = wins.get(&p.pid).copied().unwrap_or(0);
        }
    }
    found
}

/// Any process (not only Claude) whose exe lives under `dir` or whose command line names it.
/// Returns (pid, image name). Read-only.
fn processes_under(dir: &str) -> Vec<(u32, String)> {
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

/// Is `pid` a live process? Access denied still means "exists" (like `process.kill(pid, 0)`'s EPERM).
fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: OpenProcess has no memory preconditions; a returned handle is closed below.
    match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(h) => {
            let mut code = 0u32;
            // SAFETY: `h` is a valid process handle we own and `code` is a valid out-pointer.
            let ok = unsafe { GetExitCodeProcess(h, &mut code) }.is_ok();
            // SAFETY: `h` was opened above and is closed exactly once.
            let _ = unsafe { CloseHandle(h) };
            !ok || code == STILL_ACTIVE.0 as u32
        }
        Err(e) => e.code() == E_ACCESSDENIED,
    }
}

/// FILETIME (100 ns since 1601) -> microseconds since the Unix epoch.
fn filetime_to_unix_micros(ft: u64) -> Option<u64> {
    ft.checked_sub(FILETIME_UNIX_OFFSET).map(|t| t / 10)
}

impl Processes for Os {
    fn list_claude(l: Option<&Located>) -> Vec<Proc> {
        list_claude(l)
    }

    fn alive(pid: Pid) -> bool {
        pid_alive(pid)
    }

    fn start_time(pid: Pid) -> Option<u64> {
        let h = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        creation_time(h.0).and_then(filetime_to_unix_micros)
    }

    fn parent(pid: Pid) -> Option<Pid> {
        snapshot().into_iter().find(|e| e.pid == pid).map(|e| e.ppid).filter(|&p| p != 0)
    }

    fn exe(pid: Pid) -> Option<String> {
        open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).and_then(|h| image_path(h.0))
    }

    fn argv(pid: Pid) -> Option<Vec<String>> {
        let h = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ)?;
        read_cmdline(h.0).map(|c| split_cmdline(&c))
    }

    fn processes_under(dir: &str) -> Vec<(Pid, String)> {
        processes_under(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn split_rules() {
        assert_eq!(split_cmdline(r#"a.exe "x y" z\\"w" q\"r"#), vec!["a.exe", "x y", r"z\w", r#"q"r"#]);
        assert_eq!(split_cmdline(r#""C:\a b\c.exe" --k="v w""#), vec![r"C:\a b\c.exe", "--k=v w"]);
        assert!(split_cmdline("   ").is_empty());
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

    #[test]
    fn this_process_reads_back() {
        let me = std::process::id();
        assert!(Os::alive(me));
        assert!(!Os::alive(0));
        let exe = Os::exe(me).expect("own image path");
        assert!(crate::util::paths::same_path(&exe, &std::env::current_exe().unwrap().to_string_lossy()));
        let argv = Os::argv(me).expect("own command line");
        assert!(!argv.is_empty());
        let t = Os::start_time(me).expect("own start time");
        let now = log::now_millis() * 1000;
        assert!(t <= now && now - t < 86_400 * 1_000_000 * 365);
        assert!(Os::parent(me).is_some());
        assert_eq!(filetime_to_unix_micros(FILETIME_UNIX_OFFSET + 10), Some(1));
        assert_eq!(filetime_to_unix_micros(0), None);
    }
}
