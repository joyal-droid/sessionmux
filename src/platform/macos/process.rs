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

//! Processes, read through libproc and `sysctl`: the process table, executable paths, parents,
//! start times and other processes' arguments (`KERN_PROCARGS2`). No permission is needed for
//! processes of the same user.
//!
//! A Claude Desktop main process is one whose executable is the located Claude's executable and
//! whose arguments carry no `--type=` (renderers and other helpers run from helper apps inside
//! the bundle and always carry it). Its data folder is the value of `--user-data-dir=`; none means
//! the Main account. When a process's arguments cannot be read, it counts as a main process only
//! if its parent is not itself a Claude main process.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::path::Path;
use std::time::Duration;

use crate::core::claude_app::{Located, Proc};
use crate::platform::{Os, Paths, Pid, Processes};

/// The argument that names an instance's data folder.
pub(crate) const UDD: &str = "--user-data-dir=";
/// The argument every Claude helper process carries.
const HELPER_FLAG: &str = "--type=";
/// The executable inside a Claude bundle, for when no installation was located.
const BUNDLE_EXE_SUFFIX: &str = "/Claude.app/Contents/MacOS/Claude";
/// Extra room when reading the process table, for processes started between the two calls.
const PID_HEADROOM: usize = 64;
/// The data folder given to a Claude main process whose arguments cannot be read (one that is
/// starting or ending, or another user's). It names no folder, so the process belongs to no
/// account, Main included, and is never closed or sent a link as one.
pub(crate) const UNKNOWN_DATA_DIR: &str = "?";

// ───────────────────────────────────────────────────────────── arguments ──

/// Arguments from a `KERN_PROCARGS2` buffer: the argument count (native-endian `i32`), the
/// executable path, NUL padding, then the arguments, each NUL-terminated (the environment follows
/// and is ignored). Returns the arguments found (fewer than the count when the buffer ends early;
/// the last one may lack its NUL); None when the buffer is too short or the executable path is not
/// terminated. Bytes that are not UTF-8 are replaced.
pub(crate) fn parse_procargs2(buf: &[u8]) -> Option<Vec<String>> {
    let head = buf.get(..4)?;
    let argc = i32::from_ne_bytes([head[0], head[1], head[2], head[3]]);
    let argc = usize::try_from(argc).ok()?;
    let rest = &buf[4..];
    let exec_end = rest.iter().position(|&b| b == 0)?;
    let mut i = exec_end;
    while i < rest.len() && rest[i] == 0 {
        i += 1;
    }
    let mut out = Vec::with_capacity(argc.min(256));
    while out.len() < argc && i < rest.len() {
        let end = rest[i..].iter().position(|&b| b == 0).map_or(rest.len(), |n| i + n);
        out.push(String::from_utf8_lossy(&rest[i..end]).into_owned());
        i = end + 1;
    }
    Some(out)
}

/// True for the arguments of a Claude helper process (renderer, GPU, utility).
pub(crate) fn is_helper(argv: &[String]) -> bool {
    argv.iter().skip(1).any(|a| a.starts_with(HELPER_FLAG))
}

/// The data folder named by `--user-data-dir=` (letter case of the flag ignored), with quotes,
/// surrounding blanks and trailing `/` removed; None when absent or empty.
pub(crate) fn user_data_dir(argv: &[String]) -> Option<String> {
    let v = argv.iter().skip(1).find_map(|a| {
        let head = a.get(..UDD.len())?;
        head.eq_ignore_ascii_case(UDD).then(|| &a[UDD.len()..])
    })?;
    let v = v.trim().trim_matches('"').trim();
    let v = if v.len() > 1 { v.trim_end_matches('/') } else { v };
    (!v.is_empty()).then(|| v.to_string())
}

/// True when an argument names `dir` or a path inside it, alone or as the value of `--x=`.
fn arg_names(arg: &str, dir: &str) -> bool {
    let value = match arg.split_once('=') {
        Some((k, v)) if k.starts_with('-') => v,
        _ => arg,
    };
    let value = value.trim_matches('"');
    value.starts_with('/') && <Os as Paths>::is_under(value, dir)
}

// ───────────────────────────────────────────────────────────── system calls ──

/// Every process id on the system (0 excluded).
pub(crate) fn all_pids() -> Vec<Pid> {
    // SAFETY: a null buffer of size 0 only asks for the number of processes.
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let Ok(n) = usize::try_from(n) else { return Vec::new() };
    let mut buf: Vec<libc::c_int> = vec![0; n + PID_HEADROOM];
    let Ok(bytes) = libc::c_int::try_from(buf.len() * std::mem::size_of::<libc::c_int>()) else { return Vec::new() };
    // SAFETY: `buf` is writable for `bytes` bytes and outlives the call.
    let got = unsafe { libc::proc_listallpids(buf.as_mut_ptr().cast::<c_void>(), bytes) };
    let Ok(got) = usize::try_from(got) else { return Vec::new() };
    buf.truncate(got.min(buf.len()));
    buf.into_iter().filter_map(|p| Pid::try_from(p).ok()).filter(|&p| p != 0).collect()
}

fn c_pid(pid: Pid) -> Option<libc::c_int> {
    libc::c_int::try_from(pid).ok().filter(|&p| p > 0)
}

/// Full path of the process's executable.
pub(crate) fn pid_path(pid: Pid) -> Option<String> {
    let p = c_pid(pid)?;
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `buf` is writable for its full length, which is the size passed.
    let n = unsafe { libc::proc_pidpath(p, buf.as_mut_ptr().cast::<c_void>(), buf.len() as u32) };
    let n = usize::try_from(n).ok().filter(|&n| n > 0)?;
    buf.truncate(n.min(buf.len()));
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// The kernel's basic record of a process (parent, start time); None once it has exited.
fn bsd_info(pid: Pid) -> Option<libc::proc_bsdinfo> {
    let p = c_pid(pid)?;
    // SAFETY: `proc_bsdinfo` is plain old data; an all-zero value is valid and overwritten.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: `info` is a live, writable record of exactly `size` bytes.
    let n = unsafe {
        libc::proc_pidinfo(p, libc::PROC_PIDTBSDINFO, 0, (&mut info as *mut libc::proc_bsdinfo).cast(), size)
    };
    (n == size).then_some(info)
}

/// The largest argument area a process can have.
fn arg_max() -> usize {
    let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    let mut v: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>();
    // SAFETY: `mib` names a read-only integer value; `v` and `len` describe a writable integer.
    let r = unsafe {
        libc::sysctl(mib.as_mut_ptr(), 2, (&mut v as *mut libc::c_int).cast(), &mut len, std::ptr::null_mut(), 0)
    };
    match usize::try_from(v) {
        Ok(v) if r == 0 && v > 0 => v,
        _ => 1 << 20,
    }
}

/// The raw `KERN_PROCARGS2` area of `pid`. The kernel can refuse while a process is starting or
/// exiting, so a refusal is retried once after 10 ms.
fn procargs(pid: Pid) -> Option<Vec<u8>> {
    let p = c_pid(pid)?;
    let size = arg_max();
    for attempt in 0..2 {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, p];
        let mut buf = vec![0u8; size];
        let mut len = buf.len();
        // SAFETY: `buf` is writable for `len` bytes; the kernel writes at most that many and
        // stores the real length in `len`.
        let r =
            unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) };
        if r == 0 {
            buf.truncate(len.min(buf.len()));
            return Some(buf);
        }
        let err = std::io::Error::last_os_error().raw_os_error();
        if attempt == 0 && matches!(err, Some(libc::EINVAL) | Some(libc::EPERM)) {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        return None;
    }
    None
}

// ───────────────────────────────────────────────────────────── Claude ──

/// `path` resolved through symbolic links when it exists, else as given.
pub(crate) fn real_path(path: &str) -> String {
    std::fs::canonicalize(path).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| path.to_string())
}

/// Decides whether an executable is Claude Desktop's main executable: the located one (compared
/// after resolving links), or any `Claude.app/Contents/MacOS/Claude` when none was located.
pub(crate) struct ClaudeExe(Option<String>);

impl ClaudeExe {
    pub(crate) fn new(l: Option<&Located>) -> ClaudeExe {
        ClaudeExe(l.map(|l| l.exe.trim()).filter(|e| !e.is_empty()).map(real_path))
    }

    pub(crate) fn matches(&self, exe: &str) -> bool {
        if exe.is_empty() {
            return false;
        }
        match &self.0 {
            Some(want) => exe == want || real_path(exe) == *want,
            None => exe.ends_with(BUNDLE_EXE_SUFFIX),
        }
    }

    /// Like [`ClaudeExe::matches`], but any `Claude.app/Contents/MacOS/Claude` also counts.
    pub(crate) fn matches_any(&self, exe: &str) -> bool {
        exe.ends_with(BUNDLE_EXE_SUFFIX) || self.matches(exe)
    }
}

/// One Claude process seen in the table: its pid, executable and arguments (None = unreadable).
struct Seen {
    pid: Pid,
    exe: String,
    argv: Option<Vec<String>>,
}

/// Running Claude Desktop main processes of the located installation, without their windows.
pub(crate) fn claude_mains(l: Option<&Located>) -> Vec<Proc> {
    mains(l, false)
}

/// Like [`claude_mains`], but processes of any `Claude.app` copy count, not only the located one.
pub(crate) fn any_claude_mains(l: Option<&Located>) -> Vec<Proc> {
    mains(l, true)
}

fn mains(l: Option<&Located>, any_copy: bool) -> Vec<Proc> {
    let claude = ClaudeExe::new(l);
    let me = std::process::id();
    let seen: Vec<Seen> = all_pids()
        .into_iter()
        .filter(|&pid| pid != me)
        .filter_map(|pid| {
            let exe = pid_path(pid)?;
            let ours = if any_copy { claude.matches_any(&exe) } else { claude.matches(&exe) };
            ours.then(|| Seen { pid, exe, argv: <Os as Processes>::argv(pid) })
        })
        .collect();
    let claude_pids: HashSet<Pid> = seen.iter().map(|s| s.pid).collect();
    let mut out = Vec::new();
    for s in seen {
        let (cmdline, data_dir) = match &s.argv {
            Some(argv) if is_helper(argv) => continue,
            Some(argv) => (argv.join(" "), user_data_dir(argv)),
            None => {
                // Unreadable arguments: a helper is always started by a main process.
                if <Os as Processes>::parent(s.pid).is_some_and(|pp| claude_pids.contains(&pp)) {
                    continue;
                }
                (String::new(), Some(UNKNOWN_DATA_DIR.to_string()))
            }
        };
        out.push(Proc { pid: s.pid, exe: s.exe, cmdline, data_dir, hwnd: 0 });
    }
    out
}

/// The running processes as (pid, parent, start time), for walking process trees.
pub(crate) fn table() -> Vec<(Pid, Pid, u64)> {
    all_pids()
        .into_iter()
        .filter_map(|pid| {
            let info = bsd_info(pid)?;
            Some((pid, info.pbi_ppid, start_micros(&info)))
        })
        .collect()
}

fn start_micros(info: &libc::proc_bsdinfo) -> u64 {
    info.pbi_start_tvsec.saturating_mul(1_000_000).saturating_add(info.pbi_start_tvusec)
}

impl Processes for Os {
    fn list_claude(l: Option<&Located>) -> Vec<Proc> {
        let mut found = claude_mains(l);
        if !found.is_empty() {
            let pids: HashSet<Pid> = found.iter().map(|p| p.pid).collect();
            let wins = super::window::main_windows(&pids);
            for p in &mut found {
                p.hwnd = wins.get(&p.pid).map_or(0, |w| w.0 as isize);
            }
        }
        found
    }

    /// `kill(pid, 0)`: it exists when the call works or is refused for lack of permission.
    fn alive(pid: Pid) -> bool {
        let Some(p) = c_pid(pid) else { return false };
        // SAFETY: signal 0 only checks whether the process exists; nothing is delivered.
        if unsafe { libc::kill(p, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    fn start_time(pid: Pid) -> Option<u64> {
        bsd_info(pid).map(|i| start_micros(&i))
    }

    fn parent(pid: Pid) -> Option<Pid> {
        bsd_info(pid).map(|i| i.pbi_ppid).filter(|&p| p != 0)
    }

    fn exe(pid: Pid) -> Option<String> {
        pid_path(pid)
    }

    fn argv(pid: Pid) -> Option<Vec<String>> {
        procargs(pid).as_deref().and_then(parse_procargs2)
    }

    /// Any process whose executable lies under `dir` or whose arguments name a path inside it.
    fn processes_under(dir: &str) -> Vec<(Pid, String)> {
        let d = dir.trim();
        if d.is_empty() || !d.starts_with('/') || <Os as Paths>::norm_path(d) == "/" {
            return Vec::new();
        }
        let me = std::process::id();
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let mut names: HashMap<Pid, String> = HashMap::new();
        for pid in all_pids().into_iter().filter(|&p| p != me) {
            let exe = pid_path(pid).unwrap_or_default();
            // Other users' arguments are never readable; asking would only wait for the refusal.
            let ours = bsd_info(pid).is_some_and(|i| i.pbi_uid == uid);
            let hit = (!exe.is_empty() && <Os as Paths>::is_under(&exe, d))
                || (ours && Self::argv(pid).is_some_and(|argv| argv.iter().any(|a| arg_names(a, d))));
            if hit {
                let name = Path::new(&exe).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                names.insert(pid, name);
            }
        }
        let mut out: Vec<(Pid, String)> = names.into_iter().collect();
        out.sort_by_key(|(p, _)| *p);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `KERN_PROCARGS2` buffer: count, executable, `pad` NUL bytes after its terminator, then
    /// the arguments and one environment entry.
    fn buf(argc: i32, exec: &str, pad: usize, args: &[&[u8]]) -> Vec<u8> {
        let mut b = argc.to_ne_bytes().to_vec();
        b.extend_from_slice(exec.as_bytes());
        b.push(0);
        b.extend(std::iter::repeat_n(0u8, pad));
        for a in args {
            b.extend_from_slice(a);
            b.push(0);
        }
        b.extend_from_slice(b"HOME=/Users/u\0");
        b
    }

    #[test]
    fn procargs2_normal() {
        let exe = "/Applications/Claude.app/Contents/MacOS/Claude";
        let b = buf(2, exe, 3, &[exe.as_bytes(), b"--user-data-dir=/Users/u/Library/Application Support/Claude-W"]);
        let argv = parse_procargs2(&b).unwrap();
        assert_eq!(argv, vec![exe, "--user-data-dir=/Users/u/Library/Application Support/Claude-W"]);
        assert_eq!(user_data_dir(&argv).as_deref(), Some("/Users/u/Library/Application Support/Claude-W"));
        assert!(!is_helper(&argv));
    }

    #[test]
    fn procargs2_padding() {
        for pad in [0usize, 1, 7] {
            let b = buf(2, "/x/Claude", pad, &[b"/x/Claude", b"--type=renderer"]);
            let argv = parse_procargs2(&b).unwrap();
            assert_eq!(argv, vec!["/x/Claude", "--type=renderer"], "pad {pad}");
            assert!(is_helper(&argv));
        }
    }

    #[test]
    fn procargs2_count_larger_than_strings() {
        let mut b = 5i32.to_ne_bytes().to_vec();
        b.extend_from_slice(b"/x/Claude\0\0/x/Claude\0a\0b");
        assert_eq!(parse_procargs2(&b).unwrap(), vec!["/x/Claude", "a", "b"]);
        let mut b = 3i32.to_ne_bytes().to_vec();
        b.extend_from_slice(b"/x/Claude\0");
        assert_eq!(parse_procargs2(&b).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn procargs2_short_or_broken() {
        assert_eq!(parse_procargs2(&[]), None);
        assert_eq!(parse_procargs2(&[1, 0]), None);
        assert_eq!(parse_procargs2(&(-1i32).to_ne_bytes()), None);
        let mut b = 1i32.to_ne_bytes().to_vec();
        b.extend_from_slice(b"/x/Claude-without-terminator");
        assert_eq!(parse_procargs2(&b), None);
        assert_eq!(parse_procargs2(&buf(0, "/x/Claude", 2, &[])).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn procargs2_non_utf8_is_lossy() {
        let b = buf(2, "/x/Claude", 1, &[b"/x/Claude", b"--user-data-dir=/tmp/\xffbad"]);
        let argv = parse_procargs2(&b).unwrap();
        assert_eq!(argv[1], "--user-data-dir=/tmp/\u{fffd}bad");
    }

    #[test]
    fn data_folder_rules() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(user_data_dir(&a(&["/x/Claude"])), None);
        assert_eq!(user_data_dir(&a(&["/x/Claude", "--user-data-dir="])), None);
        assert_eq!(user_data_dir(&a(&["/x/Claude", "--USER-DATA-DIR=/d/Claude-A/"])).as_deref(), Some("/d/Claude-A"));
        assert_eq!(user_data_dir(&a(&["/x/Claude", "--user-data-dir=\"/d/a b\""])).as_deref(), Some("/d/a b"));
        assert_eq!(user_data_dir(&a(&["/x/Claude", "--user-data-dir=/"])).as_deref(), Some("/"));
        // The executable path itself is never read as an argument.
        assert_eq!(user_data_dir(&a(&["--user-data-dir=/d"])), None);
        assert!(!is_helper(&a(&["--type=renderer"])));
    }

    #[test]
    fn argument_paths_name_folders() {
        assert!(arg_names("/d/Claude-A/x.sock", "/d/Claude-A"));
        assert!(arg_names("--log=/d/Claude-A/log", "/d/Claude-A"));
        assert!(arg_names("/d/Claude-A", "/d/Claude-A/"));
        assert!(!arg_names("/d/Claude-AB", "/d/Claude-A"));
        assert!(!arg_names("Claude-A", "/d/Claude-A"));
        assert!(!arg_names("key=/elsewhere", "/d/Claude-A"));
    }

    #[test]
    fn claude_exe_rules() {
        let none = ClaudeExe::new(None);
        assert!(none.matches("/Applications/Claude.app/Contents/MacOS/Claude"));
        assert!(!none
            .matches("/Applications/Claude.app/Contents/Frameworks/Claude Helper.app/Contents/MacOS/Claude Helper"));
        assert!(!none.matches(""));
        let l = Located { exe: "/nowhere/Claude.app/Contents/MacOS/Claude".into(), ..Located::default() };
        let one = ClaudeExe::new(Some(&l));
        assert!(one.matches("/nowhere/Claude.app/Contents/MacOS/Claude"));
        assert!(!one.matches("/Applications/Claude.app/Contents/MacOS/Claude"));
        assert!(one.matches_any("/Applications/Claude.app/Contents/MacOS/Claude"));
        assert!(!one.matches_any("/bin/sh"));
    }

    #[test]
    fn an_unknown_data_folder_belongs_to_no_account() {
        let p = Proc {
            pid: 1,
            exe: String::new(),
            cmdline: String::new(),
            data_dir: Some(UNKNOWN_DATA_DIR.into()),
            hwnd: 0,
        };
        assert!(!crate::core::claude_app::matches_data_dir(&p, None));
        assert!(!crate::core::claude_app::matches_data_dir(&p, Some("/Users/u/Library/Application Support/Claude-X")));
        assert!(!crate::core::claude_app::matches_data_dir(&p, Some("/")));
    }

    #[test]
    fn alive_rules() {
        assert!(Os::alive(std::process::id()));
        assert!(Os::alive(1)); // launchd: refused, but it exists
        assert!(!Os::alive(0));
        assert!(!Os::alive(u32::MAX));
    }

    #[test]
    fn own_process_details() {
        let me = std::process::id();
        let exe = std::env::current_exe().unwrap().canonicalize().unwrap();
        assert_eq!(Os::exe(me).map(std::path::PathBuf::from), Some(exe.clone()));
        let argv = Os::argv(me).expect("own arguments");
        assert_eq!(argv.len(), std::env::args().count());
        let want: Vec<String> = std::env::args().skip(1).collect();
        assert_eq!(argv[1..], want[..]);
        // SAFETY: getppid has no preconditions.
        let ppid = unsafe { libc::getppid() } as Pid;
        assert_eq!(Os::parent(me), Some(ppid));
        let t = Os::start_time(me).expect("own start time");
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros() as u64;
        assert!(t <= now && now - t < 24 * 3600 * 1_000_000);
        assert_eq!(Os::start_time(me), Some(t), "the start time never changes");
        assert!(all_pids().contains(&me));
        assert!(table().iter().any(|&(p, pp, st)| p == me && pp == ppid && st == t));
    }

    #[test]
    fn launchd_arguments_are_refused_or_read() {
        // Another user's process: reading may be refused, but it must not panic.
        let _ = Os::argv(1);
        assert_eq!(Os::exe(0), None);
        assert_eq!(Os::start_time(u32::MAX), None);
    }

    #[test]
    fn processes_under_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().canonicalize().unwrap();
        let named = d.join("work");
        // `$0` of the shell names the folder; the trailing `:` keeps the shell from replacing itself.
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 30; :"])
            .arg(&named)
            .spawn()
            .expect("start a shell");
        let pid = child.id();
        let mut hit = false;
        for _ in 0..50 {
            if Os::processes_under(&d.to_string_lossy()).iter().any(|(p, _)| *p == pid) {
                hit = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
        assert!(hit, "a process naming the folder in its arguments is listed");
        assert!(Os::processes_under("").is_empty());
        assert!(Os::processes_under("/").is_empty());
        assert!(Os::processes_under("relative").is_empty());
    }

    #[test]
    fn nothing_is_listed_for_a_missing_install() {
        let l = Located { exe: "/nowhere/Claude.app/Contents/MacOS/Claude".into(), ..Located::default() };
        assert!(Os::list_claude(Some(&l)).is_empty());
    }
}
