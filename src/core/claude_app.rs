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

//! Claude Desktop discovery and process control. The system work (registry, processes, windows,
//! launching) lives in the platform layer; this module keeps the shared types, the 60 s locate
//! cache, the smoke fixture and the account matching.
//!
//! API:
//! ```ignore
//! pub struct Located { pub kind: &'static str /* "store"|"installer"|"bundle" */, pub exe: String, pub install_dir: String,
//!                      pub aumid: Option<String>, pub family: Option<String>, pub main_data_dir: String,
//!                      pub main_config_candidates: Vec<String> }
//! pub struct Proc { pub pid: u32, pub exe: String, pub cmdline: String, pub data_dir: Option<String>, pub hwnd: isize }
//! pub fn locate() -> Result<Located, String>
//! pub fn set_fixture(l:Option<Located>)   // smoke demo fixture: fixed locate, no processes
//! pub fn list_processes() -> Vec<Proc>
//! pub fn matches_data_dir(p:&Proc, data_dir:Option<&str>) -> bool
//! pub fn focus(pid:u32) -> bool
//! pub fn wait_and_focus(data_dir:Option<&str>, timeout_ms:u64) -> Option<u32>
//! ```

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::platform::{ClaudeInstall, Focus, FocusResult, Os, Processes};
use crate::util::{log, paths};

/// Where Claude Desktop is installed and where Main keeps its data.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Located {
    /// "store" | "installer" | "bundle"
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
    /// Native id of its main window; 0 = none.
    pub hwnd: isize,
}

const LOCATE_TTL: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(500);

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

/// True if a process (from `list_processes`) belongs to the account with `data_dir`
/// (None/empty = Main = no `--user-data-dir`). Exact folder match by the system's path rules.
pub fn matches_data_dir(p: &Proc, data_dir: Option<&str>) -> bool {
    match data_dir.map(str::trim).filter(|d| !d.is_empty()) {
        None => p.data_dir.as_deref().is_none_or(|d| d.trim().is_empty()),
        Some(want) => match p.data_dir.as_deref() {
            Some(have) if !have.trim().is_empty() => paths::same_path(have, want),
            _ => false,
        },
    }
}

/// Locate Claude Desktop. Successful results are cached for 60 s.
pub fn locate() -> Result<Located, String> {
    if let Some(l) = fixture() {
        return Ok(l);
    }
    if let Ok(g) = LOCATE_CACHE.lock() {
        if let Some((at, v)) = g.as_ref() {
            // An update can remove the old install folder while the cache is fresh.
            if at.elapsed() < LOCATE_TTL && Path::new(&v.exe).is_file() {
                return Ok(v.clone());
            }
        }
    }
    let v = <Os as ClaudeInstall>::locate()?;
    if let Ok(mut g) = LOCATE_CACHE.lock() {
        *g = Some((Instant::now(), v.clone()));
    }
    Ok(v)
}

/// Running Claude Desktop main processes (helpers and the Claude Code CLI excluded).
pub fn list_processes() -> Vec<Proc> {
    if fixture().is_some() {
        return Vec::new();
    }
    let l = locate().ok();
    <Os as Processes>::list_claude(l.as_ref())
}

/// Bring the process's main window to the foreground. True if it is in front now.
pub fn focus(pid: u32) -> bool {
    <Os as Focus>::focus(pid) == FocusResult::Focused
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

#[cfg(test)]
mod tests {
    use super::*;

    fn proc_with(dd: Option<&str>) -> Proc {
        Proc { pid: 1, exe: String::new(), cmdline: String::new(), data_dir: dd.map(String::from), hwnd: 0 }
    }

    #[test]
    fn main_matches_only_without_a_data_dir() {
        let main = proc_with(None);
        assert!(matches_data_dir(&main, None));
        assert!(matches_data_dir(&main, Some("")));
        assert!(matches_data_dir(&proc_with(Some("  ")), None));
        assert!(!matches_data_dir(&proc_with(Some("/x/Claude-X")), None));
    }

    #[cfg(windows)]
    #[test]
    fn matches_data_dir_rules() {
        let main = proc_with(None);
        assert!(!matches_data_dir(&main, Some(r"C:\Users\u\AppData\Roaming\Claude-X")));
        let x = proc_with(Some(r"C:\Users\U\AppData\Roaming\Claude-X"));
        assert!(matches_data_dir(&x, Some(r"c:\users\u\appdata\roaming\claude-x\")));
        assert!(!matches_data_dir(&x, Some(r"C:\Users\U\AppData\Roaming\Claude-XY")));
        assert!(!matches_data_dir(&x, None));
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
        println!("fg={:?}", <Os as crate::platform::Windows>::foreground_pid());
        let me = <Os as Processes>::processes_under(&l.main_data_dir);
        println!("under main_data_dir: {:?}", me);
    }
}
