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

//! Live end-to-end check against the real machine (ignored by default).
//!
//! Run with:
//!   PowerShell: $env:SESSIONMUX_LIVE = '1'; cargo test livetest -- --ignored --nocapture
//!   cmd:        set "SESSIONMUX_LIVE=1" && cargo test livetest -- --ignored --nocapture
//!
//! It locates Claude Desktop, lists its processes, lists the accounts from a temp copy of the
//! real store, focuses each running account's window (or relaunches a tray-hidden one, which is
//! what the app does), then copies one quiet session from Main into the first extra account and
//! removes the copy again. Set SESSIONMUX_LIVE_KEEP=1 to keep the copied session.
#![cfg(test)]

use crate::core::accounts::AccountsStore;
use crate::core::claude_app::{self, Proc};
use crate::core::sessions::{Session, SessionAccount, Sessions};
use crate::util::paths;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn account_uuid(dir: &str) -> Option<String> {
    let v: Option<serde_json::Value> = crate::util::json::read(&Path::new(dir).join("config.json"));
    v.and_then(|c| c.get("lastKnownAccountUuid").and_then(|u| u.as_str()).map(str::to_string))
}

fn running<'a>(procs: &'a [Proc], dir: Option<&str>) -> Vec<&'a Proc> {
    procs.iter().filter(|p| claude_app::matches_data_dir(p, dir)).collect()
}

fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    let rd = std::fs::read_dir(root).ok()?;
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find_file(&p, name) {
                return Some(f);
            }
        } else if p.file_name().and_then(|s| s.to_str()) == Some(name) {
            return Some(p);
        }
    }
    None
}

fn switch_to(loc: &claude_app::Located, procs: &[Proc], name: &str, dir: Option<&str>) {
    let mine = running(procs, dir);
    println!("\n-- switch to {name} ({} process(es))", mine.len());
    if let Some(p) = mine.iter().find(|p| p.hwnd != 0) {
        let ok = claude_app::focus(p.pid);
        std::thread::sleep(Duration::from_millis(500));
        let fg = claude_app::foreground_pid();
        println!("   focus pid {} -> {ok}; foreground pid now {:?} (match={})", p.pid, fg, fg == Some(p.pid));
        return;
    }
    // Tray-hidden (hwnd 0) or not running: launching again hands off to Claude's single instance.
    println!("   no window: launching ({})", if mine.is_empty() { "not running" } else { "tray-hidden" });
    let r = match dir {
        None => claude_app::launch_main(loc).map(|_| 0),
        Some(d) => claude_app::launch(loc, d),
    };
    println!("   launch -> {r:?}");
    let pid = claude_app::wait_and_focus(dir, 15_000);
    std::thread::sleep(Duration::from_millis(500));
    println!("   wait_and_focus -> {pid:?}; foreground pid now {:?}", claude_app::foreground_pid());
}

#[test]
#[ignore]
fn live() {
    if std::env::var("SESSIONMUX_LIVE").ok().as_deref().map(str::trim) != Some("1") {
        println!("SESSIONMUX_LIVE != 1, skipping");
        return;
    }
    let t0 = std::time::Instant::now();

    // 1. locate
    let loc = claude_app::locate().expect("Claude Desktop not found");
    println!("located: kind={} exe={} main_data_dir={}", loc.kind, loc.exe, loc.main_data_dir);
    println!("   aumid={:?} family={:?} ({} ms)", loc.aumid, loc.family, t0.elapsed().as_millis());

    // 2. processes
    let t = std::time::Instant::now();
    let procs = claude_app::list_processes();
    println!("processes: {} ({} ms)", procs.len(), t.elapsed().as_millis());
    for p in &procs {
        println!("   pid {:>6} hwnd {:>8} data_dir {:?}", p.pid, p.hwnd, p.data_dir);
    }

    // 3. accounts from a temp copy of the real store (never writes the real one)
    let tmp = tempfile::tempdir().unwrap();
    std::fs::copy(paths::user_data().join("sessionmux.json"), tmp.path().join("sessionmux.json")).expect("copy store");
    let store = AccountsStore::open(tmp.path(), &paths::app_data());
    let accounts = store.list_accounts(&procs, Some(&loc.main_data_dir));
    println!("accounts: {}", accounts.len());
    for a in &accounts {
        println!(
            "   {:<8} main={} running={} pid={:?} signedIn={} sessions={} codeReady={} sameSignInAs={:?} dir={:?}",
            a.name,
            a.is_main,
            a.running,
            a.pid,
            a.signed_in,
            a.session_count,
            a.code_ready,
            a.same_sign_in_as,
            a.data_dir
        );
    }
    let main = accounts.iter().find(|a| a.is_main).expect("no Main account");
    let other = accounts.iter().find(|a| !a.is_main).expect("no second account");

    // 4. switching: other, then Main (so the user ends on Main)
    switch_to(&loc, &procs, &other.name, other.data_dir.as_deref());
    std::thread::sleep(Duration::from_millis(800));
    switch_to(&loc, &procs, &main.name, None);

    // 5. session copy Main -> other
    let sa = |a: &crate::core::accounts::Account, dir: String| SessionAccount {
        id: a.id.clone(),
        name: a.name.clone(),
        account_uuid: a.account_uuid.clone().or_else(|| account_uuid(&dir)),
        data_dir: dir,
        is_main: a.is_main,
        running: a.running,
    };
    let from = sa(main, loc.main_data_dir.clone());
    let to = sa(other, other.data_dir.clone().expect("other has a data dir"));
    let sessions = Sessions::new();
    let t = std::time::Instant::now();
    let list: Vec<Session> = sessions.list(&from);
    println!("\nsessions of {}: {} ({} ms)", from.name, list.len(), t.elapsed().as_millis());
    for s in &list {
        println!(
            "   {} running={} transcript={} scratch={} turns={} {:?}",
            s.session_id, s.running, s.has_transcript, s.is_scratch, s.completed_turns, s.title
        );
    }
    let pick = list
        .iter()
        .find(|s| !s.running && s.has_transcript && !s.is_scratch)
        .or_else(|| list.iter().find(|s| !s.running && s.has_transcript))
        .expect("no quiet session with a transcript to copy");
    println!("copying {:?} ({}) -> {}", pick.title, pick.session_id, to.name);
    let before = sessions.list(&to).len();
    let t = std::time::Instant::now();
    let r = sessions.transfer(&from, &to, &pick.session_id, "copy");
    println!("transfer ({} ms): {}", t.elapsed().as_millis(), serde_json::to_string(&r).unwrap());
    assert!(r.ok, "transfer failed: {r:?}");
    let new_id = r.new_session_id.clone().unwrap();

    // verify the copy
    let after: Vec<Session> = sessions.list(&to);
    let copied = after.iter().find(|s| s.session_id == new_id).expect("copy not listed in target");
    println!(
        "target now lists {} sessions (was {before}); copy: cli={} cwd={}",
        after.len(),
        copied.cli_session_id,
        copied.cwd
    );
    let rec = find_file(&Path::new(&to.data_dir).join("claude-code-sessions"), &format!("{new_id}.json"))
        .expect("record file missing");
    println!("   record    {}", rec.display());
    let transcript = paths::home()
        .join(".claude")
        .join("projects")
        .join(paths::slug(&copied.cwd))
        .join(format!("{}.jsonl", copied.cli_session_id));
    println!("   transcript {} exists={}", transcript.display(), transcript.exists());
    assert!(transcript.exists(), "copied transcript missing");
    assert_ne!(copied.cli_session_id, pick.cli_session_id, "cli session id was not renewed");
    let src_rec =
        find_file(&Path::new(&from.data_dir).join("claude-code-sessions"), &format!("{}.json", pick.session_id));
    println!("   source record still present: {}", src_rec.is_some());
    assert!(src_rec.is_some(), "copy removed the source record");

    // 6. clean up (record, transcript, sibling dir, scratch folder) unless asked to keep
    if std::env::var("SESSIONMUX_LIVE_KEEP").ok().as_deref().map(str::trim) == Some("1") {
        println!("SESSIONMUX_LIVE_KEEP=1: keeping the copy");
    } else {
        sessions.delete(&to, &new_id).expect("delete copy");
        let _ = std::fs::remove_file(&transcript);
        let sibling = transcript.with_extension("");
        if sibling.is_dir() {
            let _ = std::fs::remove_dir_all(&sibling);
        }
        if copied.is_scratch && copied.cwd != pick.cwd && Path::new(&copied.cwd).is_dir() {
            let _ = std::fs::remove_dir_all(&copied.cwd);
        }
        let left = sessions.list(&to).len();
        println!("cleaned up: target lists {left} sessions (was {before}); transcript exists={}", transcript.exists());
        assert_eq!(left, before);
    }
    println!("\nLIVE OK in {} ms", t0.elapsed().as_millis());
}
