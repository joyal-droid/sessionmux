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

//! Code-tab session listing / transfer / deletion for Claude Desktop accounts. Cowork sessions
//! are stored apart from Code sessions; see [`cowork`].
//!
//! Layout recap:
//! ```text
//!   <dataDir>/claude-code-sessions/<acctUuid>/<orgUuid>/local_<uuid>.json   session record
//!   <dataDir>/scratch-workspaces/<acctUuid>/<orgUuid>/scratch-<date>-<hex>  scratch cwd
//!   ~/.claude/projects/<slug(cwd)>/<cliSessionId>.jsonl                     transcript
//!   ~/.claude/projects/<slug(cwd)>/<cliSessionId>/                          sibling dir
//!   ~/.claude/projects/<slug(cwd)>/memory/                                  per-project memory
//!   ~/.claude/sessions/<pid>.json                                           live CLI registry
//! ```
//! Session records are kept as an order-preserving JSON tree ([`OVal`]): every unknown key
//! survives a transfer in its original position; only the account-specific keys are rewritten.
//!
//! API:
//! ```ignore
//! impl Sessions {
//!   pub fn list(&self, a:&SessionAccount) -> Vec<Session>          // Code sessions only
//!   pub fn list_all(&self, a:&SessionAccount) -> Vec<Session>      // Code and Cowork sessions
//!   pub fn transfer(&self, from:&SessionAccount, to:&SessionAccount, session_id:&str, mode:&str) -> TransferResult
//!   pub fn delete(&self, a:&SessionAccount, session_id:&str) -> Result<(), ApiError>
//! }
//! pub fn pick_handover_target(accounts:&[AccountView], from:&AccountId, limited:&HashSet<AccountId>) -> Option<AccountId>
//! ```
//! Type notes: `Session.cli_session_id` / `model` / `effort` are `""` when unknown; failed
//! transfers carry `warnings: []`, `targetRunning: false` and `sourceRunning: false`.
//!
//! A session that is open in Claude (running) can be copied and moved: the copy is a snapshot
//! of the files as they are at that moment. A move of a running session leaves the source
//! record, scratch folder and transcript in place (the live process still owns them) and
//! reports `source-left-open`. Deleting a running session is still refused.

use crate::core::accounts::ApiError;
use crate::platform::{Misc, Os, Processes};
use crate::util::{json, log, paths};
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};
use std::collections::HashSet;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

pub mod cowork;

/// `Session.kind` of a Code-tab session.
pub const KIND_CODE: &str = "code";
/// `Session.kind` of a Cowork session.
pub const KIND_COWORK: &str = "cowork";

/// Extra reads of a session record that does not parse (it may be mid-write).
const RECORD_READ_RETRIES: u32 = 5;
/// Reads of a transcript in all while its size or modification time keeps changing.
const SNAPSHOT_ATTEMPTS: u32 = 3;
/// Extra copy attempts for a file another process holds open without sharing.
const COPY_RETRIES: u32 = 3;
/// Pause between retries of the above.
const RETRY_PAUSE: Duration = Duration::from_millis(50);

/// Account id as used by the accounts list.
pub type AccountId = String;

/// What handover target selection needs to know about one account.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountView {
    pub id: AccountId,
    pub signed_in: bool,
    pub code_ready: bool,
}

#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct Session {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "cliSessionId")]
    pub cli_session_id: String,
    pub title: String,
    pub cwd: String,
    #[serde(rename = "isScratch")]
    pub is_scratch: bool,
    #[serde(rename = "lastActivityAt")]
    pub last_activity_at: u64,
    #[serde(rename = "createdAt")]
    pub created_at: u64,
    pub model: String,
    pub effort: String,
    #[serde(rename = "isArchived")]
    pub is_archived: bool,
    #[serde(rename = "completedTurns")]
    pub completed_turns: u32,
    pub running: bool,
    #[serde(rename = "hasTranscript")]
    pub has_transcript: bool,
    /// [`KIND_CODE`] or [`KIND_COWORK`].
    pub kind: &'static str,
    /// The folder "Show folder" opens: the cwd of a Code session, the selected folder or the
    /// outputs folder of a Cowork session.
    #[serde(skip)]
    pub folder: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionAccount {
    pub id: String,
    pub name: String,
    /// Resolved, never empty.
    pub data_dir: String,
    pub is_main: bool,
    pub account_uuid: Option<String>,
    pub running: bool,
}

#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct TransferResult {
    pub ok: bool,
    #[serde(rename = "newSessionId", skip_serializing_if = "Option::is_none")]
    pub new_session_id: Option<String>,
    pub warnings: Vec<String>,
    #[serde(rename = "targetRunning")]
    pub target_running: bool,
    /// The source session was open in Claude when it was transferred.
    #[serde(rename = "sourceRunning")]
    pub source_running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl TransferResult {
    fn fail(reason: Option<&str>, error: Option<String>) -> TransferResult {
        TransferResult {
            ok: false,
            new_session_id: None,
            warnings: Vec::new(),
            target_running: false,
            source_running: false,
            reason: reason.map(String::from),
            error,
        }
    }
}

/// Session operations rooted at a home directory (`~/.claude` lives under it).
#[derive(Clone, Debug)]
pub struct Sessions {
    pub home: PathBuf,
}

impl Default for Sessions {
    fn default() -> Self {
        Sessions::new()
    }
}

// ---------------------------------------------------------------------------------------------
// Order-preserving JSON (serde_json here is built without `preserve_order`)
// ---------------------------------------------------------------------------------------------

/// JSON value whose objects keep their key order, like a JS object from `JSON.parse`.
#[derive(Clone, Debug, PartialEq)]
pub enum OVal {
    Null,
    Bool(bool),
    Num(serde_json::Number),
    Str(String),
    Arr(Vec<OVal>),
    Obj(Vec<(String, OVal)>),
}

impl OVal {
    fn get(&self, key: &str) -> Option<&OVal> {
        match self {
            OVal::Obj(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn get_str(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(OVal::Str(s)) => Some(s),
            _ => None,
        }
    }
    /// JS property assignment: existing keys keep their position, new keys go last.
    fn set(&mut self, key: &str, v: OVal) {
        if let OVal::Obj(kv) = self {
            match kv.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = v,
                None => kv.push((key.to_string(), v)),
            }
        }
    }
    /// JS truthiness.
    fn truthy(&self) -> bool {
        match self {
            OVal::Null => false,
            OVal::Bool(b) => *b,
            OVal::Num(n) => n.as_f64().map(|f| f != 0.0 && !f.is_nan()).unwrap_or(true),
            OVal::Str(s) => !s.is_empty(),
            _ => true,
        }
    }
    /// JS `Number(v) || 0`, clamped into u64.
    fn js_number_or_zero(v: Option<&OVal>) -> u64 {
        let f = match v {
            Some(OVal::Num(n)) => n.as_f64().unwrap_or(0.0),
            Some(OVal::Bool(b)) => f64::from(u8::from(*b)),
            Some(OVal::Str(s)) => {
                let t = s.trim();
                if t.is_empty() {
                    0.0
                } else {
                    t.parse::<f64>().unwrap_or(0.0)
                }
            }
            _ => 0.0,
        };
        if f.is_finite() && f > 0.0 {
            f as u64
        } else {
            0
        }
    }
    /// String form of a non-null value (`rec.model == null ? null : rec.model`).
    fn display_or_empty(v: Option<&OVal>) -> String {
        match v {
            None | Some(OVal::Null) => String::new(),
            Some(OVal::Str(s)) => s.clone(),
            Some(other) => serde_json::to_string(other).unwrap_or_default(),
        }
    }
}

impl Serialize for OVal {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            OVal::Null => s.serialize_unit(),
            OVal::Bool(b) => s.serialize_bool(*b),
            OVal::Num(n) => n.serialize(s),
            OVal::Str(v) => s.serialize_str(v),
            OVal::Arr(a) => {
                let mut seq = s.serialize_seq(Some(a.len()))?;
                for x in a {
                    seq.serialize_element(x)?;
                }
                seq.end()
            }
            OVal::Obj(kv) => {
                let mut map = s.serialize_map(Some(kv.len()))?;
                for (k, v) in kv {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for OVal {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<OVal, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = OVal;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_bool<E: de::Error>(self, b: bool) -> Result<OVal, E> {
                Ok(OVal::Bool(b))
            }
            fn visit_i64<E: de::Error>(self, n: i64) -> Result<OVal, E> {
                Ok(OVal::Num(n.into()))
            }
            fn visit_u64<E: de::Error>(self, n: u64) -> Result<OVal, E> {
                Ok(OVal::Num(n.into()))
            }
            fn visit_f64<E: de::Error>(self, f: f64) -> Result<OVal, E> {
                // JS prints integral doubles without ".0".
                if f.fract() == 0.0 && f.abs() < 9_007_199_254_740_992.0 {
                    return Ok(OVal::Num((f as i64).into()));
                }
                Ok(serde_json::Number::from_f64(f).map(OVal::Num).unwrap_or(OVal::Null))
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<OVal, E> {
                Ok(OVal::Str(s.to_string()))
            }
            fn visit_string<E: de::Error>(self, s: String) -> Result<OVal, E> {
                Ok(OVal::Str(s))
            }
            fn visit_unit<E: de::Error>(self) -> Result<OVal, E> {
                Ok(OVal::Null)
            }
            fn visit_none<E: de::Error>(self) -> Result<OVal, E> {
                Ok(OVal::Null)
            }
            fn visit_some<D2: Deserializer<'de>>(self, d: D2) -> Result<OVal, D2::Error> {
                OVal::deserialize(d)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<OVal, A::Error> {
                let mut v = Vec::new();
                while let Some(x) = a.next_element()? {
                    v.push(x);
                }
                Ok(OVal::Arr(v))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<OVal, A::Error> {
                let mut obj = OVal::Obj(Vec::new());
                while let Some((k, v)) = m.next_entry::<String, OVal>()? {
                    obj.set(&k, v); // duplicate keys: last value wins, first position kept (JSON.parse)
                }
                Ok(obj)
            }
        }
        d.deserialize_any(V)
    }
}

// ---------------------------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------------------------

/// `^local_[A-Za-z0-9-]{1,64}$`
pub fn valid_session_id(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("local_") else { return false };
    (1..=64).contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn read_oval(p: &Path) -> Option<OVal> {
    let bytes = std::fs::read(p).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// A session record (object or array), read again a few times when it does not parse yet:
/// Claude may be rewriting it while the session is open.
fn read_record_settled(p: &Path) -> Option<OVal> {
    for attempt in 0..=RECORD_READ_RETRIES {
        if let Some(r @ (OVal::Obj(_) | OVal::Arr(_))) = read_oval(p) {
            return Some(r);
        }
        if attempt < RECORD_READ_RETRIES {
            std::thread::sleep(RETRY_PAUSE);
        }
    }
    None
}

/// Size and modification time: enough to tell that a file changed between two looks.
fn stamp(p: &Path) -> Option<(u64, Option<SystemTime>)> {
    let m = std::fs::metadata(p).ok()?;
    Some((m.len(), m.modified().ok()))
}

#[cfg(test)]
type SnapshotHook = Box<dyn FnMut(&Path, u32)>;

#[cfg(test)]
thread_local! {
    /// Test hook: runs right after each whole-file read of a snapshot (path, attempt from 1).
    static AFTER_SNAPSHOT_READ: std::cell::RefCell<Option<SnapshotHook>> = const { std::cell::RefCell::new(None) };
}

/// Whole-file read of a file another process may still be appending to. When its size or
/// modification time moved during the read, it is read again, up to [`SNAPSHOT_ATTEMPTS`]
/// reads in all; the last read is kept either way. None when the file cannot be read.
fn read_snapshot(p: &Path) -> Option<Vec<u8>> {
    let mut last = None;
    for attempt in 1..=SNAPSHOT_ATTEMPTS {
        let before = stamp(p);
        let Ok(bytes) = std::fs::read(p) else { return last };
        #[cfg(test)]
        AFTER_SNAPSHOT_READ.with(|h| {
            if let Some(f) = h.borrow_mut().as_mut() {
                f(p, attempt)
            }
        });
        let steady = before.is_some() && before == stamp(p);
        last = Some(bytes);
        if steady {
            break;
        }
        log::debug(&format!("sessions: {} changed while it was read (read {attempt})", p.display()));
    }
    last
}

fn exists(p: &Path) -> bool {
    p.exists()
}

fn list_dirs(p: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(p) else { return Vec::new() };
    rd.flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

/// The most recently modified folder in `parent` (the first one when no time can be read).
fn newest_dir(parent: &Path) -> Option<String> {
    let dirs = list_dirs(parent);
    let mut pick = dirs.first()?.clone();
    if dirs.len() > 1 {
        let mut best: Option<SystemTime> = None;
        for d in &dirs {
            if let Some(m) = std::fs::metadata(parent.join(d)).ok().and_then(|m| m.modified().ok()) {
                if best.map(|b| m > b).unwrap_or(true) {
                    best = Some(m);
                    pick = d.clone();
                }
            }
        }
    }
    Some(pick)
}

/// `path.resolve(p)`.
fn resolve(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

fn comp_key(c: &Component) -> String {
    c.as_os_str().to_string_lossy().to_lowercase()
}

/// If `child` is `parent` or lives below it, the remaining path components (empty for equal),
/// otherwise None. Case-insensitive, keeps the child's casing.
fn rel_inside(parent: &Path, child: &str) -> Option<Vec<String>> {
    if child.is_empty() {
        return None;
    }
    let p = resolve(parent);
    let c = resolve(Path::new(child));
    let pc: Vec<Component> = p.components().collect();
    let cc: Vec<Component> = c.components().collect();
    if cc.len() < pc.len() {
        return None;
    }
    for (a, b) in pc.iter().zip(cc.iter()) {
        if comp_key(a) != comp_key(b) {
            return None;
        }
    }
    Some(cc[pc.len()..].iter().map(|x| x.as_os_str().to_string_lossy().into_owned()).collect())
}

fn join_parts(base: &Path, parts: &[String]) -> PathBuf {
    let mut p = base.to_path_buf();
    for x in parts {
        p.push(x);
    }
    p
}

fn lossy(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[cfg(test)]
thread_local! {
    /// Test hook: make every copy_dir fail after copying.
    static FAIL_COPY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Recursive copy (overwrites unless `merge`, keeps timestamps, recreates links).
/// `merge = true` never overwrites files that already exist at the target. Symlinks and
/// junctions are recreated as links (never followed); see [`copy_link`]. Returns how many
/// files were skipped because another process kept them locked; see [`copy_file_tolerant`].
fn copy_dir(src: &Path, dst: &Path, merge: bool) -> io::Result<usize> {
    let mut skipped = 0;
    copy_tree(src, dst, merge, &mut skipped)?;
    #[cfg(test)]
    if FAIL_COPY.with(|f| f.get()) {
        return Err(io::Error::other("disk full"));
    }
    Ok(skipped)
}

fn copy_tree(src: &Path, dst: &Path, merge: bool, skipped: &mut usize) -> io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for ent in std::fs::read_dir(src)? {
        let ent = ent?;
        let ft = ent.file_type()?;
        let s = ent.path();
        let d = dst.join(ent.file_name());
        if ft.is_dir() {
            copy_tree(&s, &d, merge, skipped)?;
        } else if ft.is_file() {
            if merge && d.exists() {
                continue;
            }
            copy_file_tolerant(&s, &d, skipped)?;
        } else if ft.is_symlink() {
            copy_link(&s, &d, merge);
        } else {
            log::debug(&format!("sessions: skipped non-regular entry {}", s.display()));
        }
    }
    Ok(())
}

/// Another process holds the file open (never on systems without mandatory file locks).
fn is_sharing_violation(e: &io::Error) -> bool {
    <Os as Misc>::is_sharing_violation(e)
}

/// Copy one file (CopyFileExW keeps its timestamps). A file another process keeps locked is
/// tried [`COPY_RETRIES`] more times and then skipped and counted in `skipped`, so one busy
/// file never fails the whole copy; any other error does.
fn copy_file_tolerant(s: &Path, d: &Path, skipped: &mut usize) -> io::Result<()> {
    let mut retries = 0;
    loop {
        match std::fs::copy(s, d) {
            Ok(_) => return Ok(()),
            Err(e) if is_sharing_violation(&e) && retries < COPY_RETRIES => {
                retries += 1;
                std::thread::sleep(RETRY_PAUSE);
            }
            Err(e) if is_sharing_violation(&e) => {
                log::warn(&format!("sessions: skipped busy file {}: {e}", s.display()));
                *skipped += 1;
                return Ok(());
            }
            Err(e) => return Err(e),
        }
    }
}

/// Recreate a symlink/junction at `d` (relative targets resolved against the
/// source's folder, never followed). Creating symlinks needs Developer Mode or the privilege;
/// when that fails the entry is skipped and logged (the copy itself still succeeds).
fn copy_link(s: &Path, d: &Path, merge: bool) {
    let target = match std::fs::read_link(s) {
        Ok(t) => t,
        Err(e) => {
            log::debug(&format!("sessions: unreadable link {}: {e}", s.display()));
            return;
        }
    };
    let target = if target.is_absolute() { target } else { s.parent().map(|p| p.join(&target)).unwrap_or(target) };
    if std::fs::symlink_metadata(d).is_ok() {
        if merge {
            return;
        }
        let _ = rm_rf(d);
    }
    let is_dir = std::fs::metadata(s).map(|m| m.is_dir()).unwrap_or(false);
    if let Err(e) = <Os as Misc>::symlink(&target, d, is_dir) {
        log::debug(&format!("sessions: could not recreate link {} -> {}: {e}", d.display(), target.display()));
    }
}

/// Remove a file or a directory tree; a missing path is fine (`fs.rm(p, {recursive, force})`).
fn rm_rf(p: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(p) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p),
        Ok(_) => std::fs::remove_file(p),
    }
}

/// Pick a free path: base, base-2, base-3, ...
fn unique_dir(parent: &Path, leaf: &str) -> PathBuf {
    let mut candidate = parent.join(leaf);
    let mut n = 2;
    while exists(&candidate) {
        candidate = parent.join(format!("{leaf}-{n}"));
        n += 1;
    }
    candidate
}

/// Path as it appears inside a JSON string (backslashes doubled).
fn json_escaped(p: &str) -> String {
    let s = serde_json::to_string(p).unwrap_or_default();
    s.get(1..s.len().saturating_sub(1)).unwrap_or("").to_string()
}

/// `Map.set` on an insertion-ordered pair list.
fn set_pair(pairs: &mut Vec<(String, String)>, k: String, v: String) {
    match pairs.iter_mut().find(|(pk, _)| *pk == k) {
        Some(slot) => slot.1 = v,
        None => pairs.push((k, v)),
    }
}

/// Replace every key of `pairs` in `text` in a single pass, longest key first (so the
/// JSON-escaped form wins over the raw form).
fn replace_all(text: &str, pairs: &[(String, String)]) -> String {
    let mut keys: Vec<&(String, String)> = pairs.iter().filter(|(k, _)| !k.is_empty()).collect();
    if keys.is_empty() {
        return text.to_string();
    }
    keys.sort_by_key(|k| std::cmp::Reverse(k.0.encode_utf16().count()));
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        if let Some((k, v)) = keys.iter().map(|kv| (&kv.0, &kv.1)).find(|(k, _)| rest.starts_with(k.as_str())) {
            out.push_str(v);
            i += k.len();
            continue;
        }
        match rest.chars().next() {
            Some(ch) => {
                out.push(ch);
                i += ch.len_utf8();
            }
            None => break,
        }
    }
    out
}

/// Is `pid` a live process? Access denied still means "exists" (like `process.kill(pid, 0)`'s EPERM).
fn pid_alive(pid: i64) -> bool {
    match u32::try_from(pid) {
        Ok(p) if p > 0 => <Os as Processes>::alive(p),
        _ => false,
    }
}

/// JS `parseInt(name, 10)` (leading digits, optional sign); None for NaN.
fn parse_int_prefix(s: &str) -> Option<i64> {
    let t = s.trim_start();
    let (neg, body) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let digits: String = body.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let n: i64 = digits.parse().unwrap_or(i64::MAX);
    Some(if neg { -n } else { n })
}

/// One live entry of the Claude Code session registry. Both ids are None for an entry whose
/// file could not be read (part-written or locked) or names no session: its owner is unknown.
struct LiveEntry {
    /// hostSessionId: the Desktop record's sessionId (`local_…`).
    host: Option<String>,
    /// sessionId: the record's cliSessionId.
    cli: Option<String>,
}

/// One session record file of an account.
struct RecordFile {
    file: PathBuf,
    session_id: String,
}

// ---------------------------------------------------------------------------------------------

impl Sessions {
    /// Uses %USERPROFILE%. Tests construct `Sessions { home }` directly.
    pub fn new() -> Sessions {
        Sessions { home: crate::util::paths::home() }
    }

    fn projects_dir(&self) -> PathBuf {
        self.home.join(".claude").join("projects")
    }

    fn registry_dir(&self) -> PathBuf {
        self.home.join(".claude").join("sessions")
    }

    /// hostSessionIds (`local_…`) whose registry pid is alive.
    fn running_host_ids(&self) -> Vec<String> {
        self.live_registry().into_iter().filter_map(|e| e.host).collect()
    }

    /// Registry entries (`~/.claude/sessions/<pid>.json`) whose pid is alive. A `<pid>.json`
    /// of a live pid that cannot be read, even after a second try, or names no session is
    /// kept as an entry of unknown owner.
    fn live_registry(&self) -> Vec<LiveEntry> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(self.registry_dir()) else { return out };
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().into_owned();
            if !name.to_lowercase().ends_with(".json") {
                continue;
            }
            let ids = |rec: &serde_json::Value| {
                let field = |k: &str| rec.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from);
                (field("hostSessionId"), field("sessionId"))
            };
            let read = || json::read::<serde_json::Value>(&ent.path()).filter(|r| ids(r) != (None, None));
            let rec = match read() {
                Some(rec) => rec,
                None => {
                    // The file name alone says which pid wrote it.
                    let stem = &name[..name.len() - ".json".len()];
                    let pid = Some(stem).filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()));
                    if !pid.and_then(|p| p.parse::<i64>().ok()).map(pid_alive).unwrap_or(false) {
                        continue;
                    }
                    // A file being written reads fine a moment later.
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    match read() {
                        Some(rec) => rec,
                        None => {
                            out.push(LiveEntry { host: None, cli: None });
                            continue;
                        }
                    }
                }
            };
            let (host, cli) = ids(&rec);
            // Prefer the pid field; fall back to the file name (<pid>.json).
            let pid = match rec.get("pid") {
                Some(v) if v.is_i64() || v.is_u64() => v.as_i64(),
                Some(v) if v.as_f64().map(|f| f.fract() == 0.0).unwrap_or(false) => v.as_f64().map(|f| f as i64),
                _ => parse_int_prefix(&name),
            };
            if pid.map(pid_alive).unwrap_or(false) {
                out.push(LiveEntry { host, cli });
            }
        }
        out
    }

    /// How many live Claude Code sessions belong to account `a` right now. Each live registry
    /// entry is matched against the account's record files, read now, by its hostSessionId
    /// (the record's sessionId) or its session id (the record's cliSessionId). A live entry
    /// whose owner is unknown counts for every account, so a window is never restarted under
    /// a session that could not be read.
    pub fn live_sessions_in(&self, a: &SessionAccount) -> usize {
        if a.data_dir.is_empty() {
            return 0;
        }
        let live = self.live_registry();
        if live.is_empty() {
            return 0;
        }
        let mut count = live.iter().filter(|e| e.host.is_none() && e.cli.is_none()).count();
        for f in Self::record_files(a) {
            let rec = read_oval(&f.file);
            let session_id = rec.as_ref().and_then(|r| r.get_str("sessionId")).unwrap_or(&f.session_id);
            let cli = rec.as_ref().and_then(|r| r.get_str("cliSessionId")).filter(|c| !c.is_empty());
            let hit = live
                .iter()
                .any(|e| e.host.as_deref() == Some(session_id) || (cli.is_some() && e.cli.as_deref() == cli));
            if hit {
                count += 1;
            }
        }
        count
    }

    fn is_running(&self, session_id: &str) -> bool {
        self.running_host_ids().iter().any(|h| h == session_id)
    }

    /// accountUuid from the object, else from `<dataDir>/config.json`.
    fn account_uuid_of(a: &SessionAccount) -> Option<String> {
        if let Some(u) = a.account_uuid.as_deref().filter(|u| !u.is_empty()) {
            return Some(u.to_string());
        }
        if a.data_dir.is_empty() {
            return None;
        }
        let cfg: serde_json::Value = json::read(&Path::new(&a.data_dir).join("config.json"))?;
        cfg.get("lastKnownAccountUuid").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from)
    }

    /// Every session record file of an account; the account uuid folder when known,
    /// otherwise every uuid folder.
    fn record_files(a: &SessionAccount) -> Vec<RecordFile> {
        let base = Path::new(&a.data_dir).join("claude-code-sessions");
        let accts = match Self::account_uuid_of(a) {
            Some(u) => vec![u],
            None => list_dirs(&base),
        };
        let mut out = Vec::new();
        for acct in accts {
            for org in list_dirs(&base.join(&acct)) {
                let dir = base.join(&acct).join(&org);
                let Ok(rd) = std::fs::read_dir(&dir) else { continue };
                for ent in rd.flatten() {
                    let name = ent.file_name().to_string_lossy().into_owned();
                    let l = name.to_lowercase();
                    if !(l.starts_with("local_") && l.ends_with(".json") && l.len() > "local_.json".len()) {
                        continue;
                    }
                    out.push(RecordFile {
                        file: dir.join(&name),
                        session_id: name[..name.len() - ".json".len()].to_string(),
                    });
                }
            }
        }
        out
    }

    fn find_record(a: &SessionAccount, session_id: &str) -> Option<RecordFile> {
        Self::record_files(a).into_iter().find(|f| f.session_id == session_id)
    }

    fn transcript_path(&self, cwd: &str, cli: &str) -> PathBuf {
        self.projects_dir().join(paths::slug(cwd)).join(format!("{cli}.jsonl"))
    }

    /// Sessions of an account, sorted by lastActivityAt desc. `running` = a live
    /// `~/.claude/sessions/<pid>.json` names it as hostSessionId.
    pub fn list(&self, a: &SessionAccount) -> Vec<Session> {
        if a.data_dir.is_empty() {
            log::warn("sessions: Source account has no data folder");
            return Vec::new();
        }
        let scratch_root = Path::new(&a.data_dir).join("scratch-workspaces");
        let running = self.running_host_ids();
        let mut out = Vec::new();
        for f in Self::record_files(a) {
            let rec = match read_oval(&f.file) {
                Some(r @ (OVal::Obj(_) | OVal::Arr(_))) => r,
                _ => {
                    log::warn(&format!("sessions: unreadable record {}", f.file.display()));
                    continue;
                }
            };
            let session_id = rec.get_str("sessionId").map(String::from).unwrap_or(f.session_id);
            let cwd = rec.get_str("cwd").unwrap_or("").to_string();
            let cli = rec.get_str("cliSessionId").map(String::from);
            let has_transcript = match (&cli, cwd.is_empty()) {
                (Some(c), false) if !c.is_empty() => exists(&self.transcript_path(&cwd, c)),
                _ => false,
            };
            out.push(Session {
                running: running.contains(&session_id),
                session_id,
                cli_session_id: cli.unwrap_or_default(),
                title: rec.get_str("title").unwrap_or("").to_string(),
                is_scratch: !cwd.is_empty() && rel_inside(&scratch_root, &cwd).is_some(),
                last_activity_at: OVal::js_number_or_zero(rec.get("lastActivityAt")),
                created_at: OVal::js_number_or_zero(rec.get("createdAt")),
                model: OVal::display_or_empty(rec.get("model")),
                effort: OVal::display_or_empty(rec.get("effort")),
                is_archived: rec.get("isArchived").map(OVal::truthy).unwrap_or(false),
                completed_turns: OVal::js_number_or_zero(rec.get("completedTurns")).min(u64::from(u32::MAX)) as u32,
                has_transcript,
                kind: KIND_CODE,
                folder: cwd.clone(),
                cwd,
            });
        }
        out.sort_by_key(|x| std::cmp::Reverse(x.last_activity_at));
        out
    }

    /// Code and Cowork sessions of an account, sorted by lastActivityAt desc.
    pub fn list_all(&self, a: &SessionAccount) -> Vec<Session> {
        let mut out = self.list(a);
        if !a.data_dir.is_empty() {
            out.extend(cowork::list(a));
        }
        out.sort_by_key(|x| std::cmp::Reverse(x.last_activity_at));
        out
    }

    /// Delete a session record only (transcript, scratch folder and memory stay).
    pub fn delete(&self, a: &SessionAccount, session_id: &str) -> Result<(), ApiError> {
        if a.data_dir.is_empty() {
            return Err(ApiError::new("io", "Source account has no data folder"));
        }
        if !valid_session_id(session_id) {
            return Err(ApiError::new("invalid", "Invalid session id"));
        }
        if self.is_running(session_id) {
            return Err(ApiError::new("running", "The session is running — stop it first"));
        }
        let Some(found) = Self::find_record(a, session_id) else {
            return Err(ApiError::new("not-found", "Session not found"));
        };
        match rm_rf(&found.file) {
            Ok(()) => {
                log::info(&format!("sessions: deleted record {session_id}"));
                Ok(())
            }
            Err(e) => {
                log::error(&format!("sessions: delete failed: {e}"));
                Err(ApiError::new("io", e.to_string()))
            }
        }
    }

    /// Copy or move a session to another account. Anything created is removed
    /// again if a later step fails. A running session is copied as it is at that moment;
    /// on a move its source files are left in place (warning `source-left-open`).
    pub fn transfer(&self, from: &SessionAccount, to: &SessionAccount, session_id: &str, mode: &str) -> TransferResult {
        let mut created: Vec<PathBuf> = Vec::new();
        match self.transfer_inner(from, to, session_id, mode, &mut created) {
            Ok(r) => r,
            Err(e) => {
                log::error(&format!("sessions: transfer failed: {e}"));
                for p in created.iter().rev() {
                    let _ = rm_rf(p);
                }
                TransferResult::fail(Some("error"), Some(e))
            }
        }
    }

    fn transfer_inner(
        &self,
        from: &SessionAccount,
        to: &SessionAccount,
        session_id: &str,
        mode: &str,
        created: &mut Vec<PathBuf>,
    ) -> Result<TransferResult, String> {
        if mode != "copy" && mode != "move" {
            return Ok(TransferResult::fail(None, Some("Invalid mode".into())));
        }
        if !valid_session_id(session_id) {
            return Ok(TransferResult::fail(None, Some("Invalid session id".into())));
        }
        if from.data_dir.is_empty() {
            return Err("Source account has no data folder".into());
        }
        if to.data_dir.is_empty() {
            return Err("Target account has no data folder".into());
        }
        let from_dir = Path::new(&from.data_dir);
        let to_dir = Path::new(&to.data_dir);
        if mode == "move" && paths::same_path(&from.data_dir, &to.data_dir) {
            return Ok(TransferResult::fail(
                Some("same-account"),
                Some("Source and target are the same account".into()),
            ));
        }

        // 1. Is the session open in Claude right now? Then everything below is a snapshot.
        let mut source_running = self.is_running(session_id);

        // 2. Locate source and target identity.
        let Some(src) = Self::find_record(from, session_id) else {
            return Ok(TransferResult::fail(Some("not-found"), Some("Session not found".into())));
        };
        let Some(record) = read_record_settled(&src.file) else {
            return Ok(TransferResult::fail(None, Some("Session record is unreadable".into())));
        };
        let Some(to_acct) = Self::account_uuid_of(to) else {
            return Ok(TransferResult::fail(Some("target-never-opened-code"), None));
        };
        let to_acct_dir = to_dir.join("claude-code-sessions").join(&to_acct);
        let Some(to_org) = newest_dir(&to_acct_dir) else {
            return Ok(TransferResult::fail(Some("target-never-opened-code"), None));
        };

        // 3. New identity.
        let old_cli = record.get_str("cliSessionId").map(String::from);
        let new_cli = uuid::Uuid::new_v4().to_string();
        let new_session_id = format!("local_{}", uuid::Uuid::new_v4());
        let mut next = record.clone();
        next.set("sessionId", OVal::Str(new_session_id.clone()));
        next.set("cliSessionId", OVal::Str(new_cli.clone()));

        // 4. Scratch workspace -> copy into the target account.
        let old_cwd = record.get_str("cwd").unwrap_or("").to_string();
        let mut new_cwd = old_cwd.clone();
        let mut src_scratch: Option<PathBuf> = None;
        let mut dst_scratch_dir: Option<PathBuf> = None;
        let mut skipped = 0usize;
        let from_scratch_root = from_dir.join("scratch-workspaces");
        let rel = if old_cwd.is_empty() { None } else { rel_inside(&from_scratch_root, &old_cwd) };
        if let Some(parts) = rel.filter(|p| !p.is_empty()) {
            // parts = <acct>\<org>\<leaf>[\sub...]; the scratch folder is the 3rd level.
            if parts.len() >= 3 {
                let ss = from_scratch_root.join(&parts[0]).join(&parts[1]).join(&parts[2]);
                let dst_parent = to_dir.join("scratch-workspaces").join(&to_acct).join(&to_org);
                std::fs::create_dir_all(&dst_parent).map_err(|e| e.to_string())?;
                let dst_scratch = unique_dir(&dst_parent, &parts[2]);
                created.push(dst_scratch.clone()); // registered first so a failed copy is rolled back
                if exists(&ss) {
                    skipped += copy_dir(&ss, &dst_scratch, false).map_err(|e| e.to_string())?;
                } else {
                    std::fs::create_dir_all(&dst_scratch).map_err(|e| e.to_string())?;
                }
                new_cwd = lossy(&join_parts(&dst_scratch, &parts[3..]));
                // Map every path field that pointed inside the old scratch folder.
                for key in ["cwd", "originCwd", "gitAnchorsFolderRealpath"] {
                    if let Some(v) = record.get_str(key) {
                        if let Some(r) = rel_inside(&ss, v) {
                            next.set(key, OVal::Str(lossy(&join_parts(&dst_scratch, &r))));
                        }
                    }
                }
                next.set("cwd", OVal::Str(new_cwd.clone()));
                src_scratch = Some(ss);
                dst_scratch_dir = Some(dst_scratch);
            }
        }
        let cwd_changed = new_cwd != old_cwd;

        // 5. Transcript, sibling dir and memory.
        let mut warnings: Vec<String> = Vec::new();
        let projects = self.projects_dir();
        let old_slug_dir = projects.join(paths::slug(&old_cwd));
        let new_slug_dir = projects.join(paths::slug(&new_cwd));
        let old_cli = old_cli.filter(|c| !c.is_empty());
        let text = match (&old_cli, old_cwd.is_empty()) {
            (Some(c), false) => read_snapshot(&old_slug_dir.join(format!("{c}.jsonl")))
                .map(|b| String::from_utf8_lossy(&b).into_owned()),
            _ => None,
        };
        match text {
            None => warnings.push("no-transcript".into()),
            Some(text) => {
                let mut pairs: Vec<(String, String)> = Vec::new();
                if let Some(c) = &old_cli {
                    set_pair(&mut pairs, c.clone(), new_cli.clone());
                }
                if cwd_changed {
                    set_pair(&mut pairs, json_escaped(&old_cwd), json_escaped(&new_cwd));
                    set_pair(&mut pairs, old_cwd.clone(), new_cwd.clone());
                    if let (Some(ss), Some(ds)) = (&src_scratch, &dst_scratch_dir) {
                        set_pair(&mut pairs, json_escaped(&lossy(ss)), json_escaped(&lossy(ds)));
                        set_pair(&mut pairs, lossy(ss), lossy(ds));
                    }
                }
                std::fs::create_dir_all(&new_slug_dir).map_err(|e| e.to_string())?;
                let dst = new_slug_dir.join(format!("{new_cli}.jsonl"));
                std::fs::write(&dst, replace_all(&text, &pairs)).map_err(|e| e.to_string())?;
                created.push(dst);
            }
        }
        if let (Some(c), false) = (&old_cli, old_cwd.is_empty()) {
            let sib = old_slug_dir.join(c);
            if exists(&sib) {
                let dst_sib = new_slug_dir.join(&new_cli);
                created.push(dst_sib.clone()); // registered first so a failed copy is rolled back
                skipped += copy_dir(&sib, &dst_sib, false).map_err(|e| e.to_string())?;
            }
            if cwd_changed {
                let mem = old_slug_dir.join("memory");
                // Merge only; never overwrite and never roll back (may hold pre-existing files).
                if exists(&mem) {
                    skipped += copy_dir(&mem, &new_slug_dir.join("memory"), true).map_err(|e| e.to_string())?;
                }
            }
        }
        if skipped > 0 {
            warnings.push(format!("files-skipped:{skipped}"));
        }

        // 6. Write the new record atomically.
        let dst_record = to_acct_dir.join(&to_org).join(format!("{new_session_id}.json"));
        write_record_atomic(&dst_record, &next).map_err(|e| e.to_string())?;
        created.push(dst_record);

        // 7. Move: remove the source only after everything succeeded, and only when no live
        // process owns it (looked at again, the session may have been opened meanwhile).
        if mode == "move" {
            created.clear(); // past the point of no return; keep the copy.
            source_running = source_running || self.is_running(session_id);
        }
        if mode == "move" && source_running {
            warnings.push("source-left-open".into());
            log::info(&format!("sessions: {session_id} is open in Claude; its source files stay in place"));
        } else if mode == "move" {
            if let Err(e) = rm_rf(&src.file) {
                warnings.push("source-not-removed".into());
                log::warn(&format!("sessions: copied but could not remove source {}: {e}", src.file.display()));
                src_scratch = None; // keep the scratch folder the source record still points to
            }
            // Only a scratch folder under <from.dataDir>/scratch-workspaces is ever deleted.
            if let Some(ss) = src_scratch {
                if rel_inside(&from_scratch_root, &lossy(&ss)).map(|r| !r.is_empty()).unwrap_or(false) {
                    if let Err(e) = rm_rf(&ss) {
                        warnings.push("scratch-not-removed".into());
                        log::warn(&format!("sessions: could not remove {}: {e}", ss.display()));
                    }
                }
            }
        }

        log::info(&format!("sessions: {mode} {session_id} -> {new_session_id}"));
        Ok(TransferResult {
            ok: true,
            new_session_id: Some(new_session_id),
            warnings,
            target_running: to.running,
            source_running,
            reason: None,
            error: None,
        })
    }
}

/// Account a session is handed over to: the first account in panel order that is signed in,
/// has used the Code tab, is not `from` and is not limited. None when there is no such account.
#[allow(dead_code)]
pub fn pick_handover_target(
    accounts: &[AccountView],
    from: &AccountId,
    limited: &HashSet<AccountId>,
) -> Option<AccountId> {
    accounts
        .iter()
        .find(|a| a.signed_in && a.code_ready && a.id != *from && !limited.contains(&a.id))
        .map(|a| a.id.clone())
}

/// `JSON.stringify(obj, null, 2)` via tmp file + rename so a crash never leaves a half-written record.
fn write_record_atomic(file: &Path, obj: &OVal) -> io::Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(obj).map_err(io::Error::other)?;
    let rnd = uuid::Uuid::new_v4().simple().to_string();
    let mut tmp = file.as_os_str().to_owned();
    tmp.push(format!(".{}.{}.{}.tmp", std::process::id(), log::now_millis(), &rnd[..8]));
    let tmp = PathBuf::from(tmp);
    let res = std::fs::write(&tmp, data).and_then(|_| json::rename_with_retry(&tmp, file));
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::fs;

    const ACCT_A: &str = "aaaaaaaa-0000-0000-0000-000000000001";
    const ACCT_B: &str = "bbbbbbbb-0000-0000-0000-000000000002";
    const ORG_A: &str = "org-a";
    const ORG_B: &str = "org-b";
    /// A dead pid: numbers this large are never assigned to a process.
    const DEAD_PID: u32 = 2147483000;
    const SCRATCH_LEAF: &str = "scratch-2026-10-26-abc123";

    fn slug(p: &Path) -> String {
        paths::slug(&p.to_string_lossy())
    }
    fn write_json(f: &Path, v: &Value) {
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(f, serde_json::to_string(v).unwrap()).unwrap();
    }
    fn read_json(f: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(f).unwrap()).unwrap()
    }
    fn esc(p: &Path) -> String {
        json_escaped(&p.to_string_lossy())
    }
    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    struct E {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        dir_b: PathBuf,
        projects: PathBuf,
        rec_dir_a: PathBuf,
        rec_dir_b: PathBuf,
        scratch: PathBuf,
        cli1: String,
        project: PathBuf,
        api: Sessions,
        a: SessionAccount,
        b: SessionAccount,
    }

    fn setup() -> E {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let home = root.join("home");
        let dir_a = root.join("Claude-A");
        let dir_b = root.join("Claude-B");
        let projects = home.join(".claude").join("projects");
        let registry = home.join(".claude").join("sessions");
        fs::create_dir_all(&projects).unwrap();
        fs::create_dir_all(&registry).unwrap();

        write_json(&dir_a.join("config.json"), &json!({"lastKnownAccountUuid": ACCT_A}));
        write_json(&dir_b.join("config.json"), &json!({"lastKnownAccountUuid": ACCT_B}));
        let rec_dir_a = dir_a.join("claude-code-sessions").join(ACCT_A).join(ORG_A);
        let rec_dir_b = dir_b.join("claude-code-sessions").join(ACCT_B).join(ORG_B);
        fs::create_dir_all(&rec_dir_a).unwrap();
        fs::create_dir_all(&rec_dir_b).unwrap();

        // Session 1: scratch session with transcript, sibling dir and memory.
        let scratch = dir_a.join("scratch-workspaces").join(ACCT_A).join(ORG_A).join(SCRATCH_LEAF);
        fs::create_dir_all(scratch.join("sub")).unwrap();
        fs::write(scratch.join("notes.txt"), "hello").unwrap();
        let cli1 = "11111111-1111-1111-1111-111111111111".to_string();
        // Written as text so the key order (with unknownKey in the middle) is under our control.
        let s1 = format!(
            "{{\"sessionId\":\"local_s1\",\"cliSessionId\":\"{cli1}\",\"cwd\":\"{sc}\",\"originCwd\":\"{sc}\",\"unknownKey\":{{\"keep\":[1,2,3]}},\"gitAnchorsFolderRealpath\":\"{sc}\",\"title\":\"Scratch one\",\"lastActivityAt\":3000,\"createdAt\":100,\"model\":\"opus\",\"effort\":\"high\",\"isArchived\":false,\"completedTurns\":4,\"zLast\":null}}",
            sc = esc(&scratch)
        );
        fs::write(rec_dir_a.join("local_s1.json"), s1).unwrap();
        let slug_dir1 = projects.join(slug(&scratch));
        fs::create_dir_all(slug_dir1.join(&cli1)).unwrap();
        fs::write(slug_dir1.join(&cli1).join("custom-title.json"), r#"{"t":1}"#).unwrap();
        fs::create_dir_all(slug_dir1.join("memory")).unwrap();
        fs::write(slug_dir1.join("memory").join("a.md"), "from-source").unwrap();
        fs::write(slug_dir1.join("memory").join("shared.md"), "source-version").unwrap();
        let lines = [
            json!({"type": "user", "sessionId": cli1, "cwd": s(&scratch), "text": format!("look in {}\\sub", s(&scratch))}),
            json!({"type": "assistant", "sessionId": cli1, "cwd": s(&scratch.join("sub"))}),
        ]
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n")
            + "\n";
        fs::write(slug_dir1.join(format!("{cli1}.jsonl")), lines).unwrap();

        // Session 2: real project folder, no transcript.
        let project = root.join("My Project");
        fs::create_dir_all(&project).unwrap();
        write_json(
            &rec_dir_a.join("local_s2.json"),
            &json!({"sessionId": "local_s2", "cliSessionId": "22222222-2222-2222-2222-222222222222", "cwd": s(&project),
                    "originCwd": s(&project), "gitAnchorsFolderRealpath": s(&project), "title": "Project", "lastActivityAt": 5000,
                    "createdAt": 200, "model": "sonnet", "isArchived": true}),
        );

        // Session 3: oldest, running (registry entry with our own live pid).
        write_json(
            &rec_dir_a.join("local_s3.json"),
            &json!({"sessionId": "local_s3", "cliSessionId": "33333333-3333-3333-3333-333333333333", "cwd": s(&project),
                    "title": "Live", "lastActivityAt": 1000, "createdAt": 50}),
        );
        let me = std::process::id();
        write_json(
            &registry.join(format!("{me}.json")),
            &json!({"pid": me, "sessionId": "x", "cwd": s(&project), "hostSessionId": "local_s3", "status": "busy"}),
        );
        // Stale registry entry for s1 with a dead pid -> must NOT count as running.
        write_json(&registry.join(format!("{DEAD_PID}.json")), &json!({"pid": DEAD_PID, "hostSessionId": "local_s1"}));

        let a = SessionAccount {
            id: "a".into(),
            name: "A".into(),
            data_dir: s(&dir_a),
            is_main: true,
            account_uuid: Some(ACCT_A.into()),
            running: false,
        };
        let b = SessionAccount {
            id: "b".into(),
            name: "B".into(),
            data_dir: s(&dir_b),
            is_main: false,
            account_uuid: Some(ACCT_B.into()),
            running: true,
        };
        E {
            api: Sessions { home },
            root,
            dir_b,
            projects,
            rec_dir_a,
            rec_dir_b,
            scratch,
            cli1,
            project,
            a,
            b,
            _tmp: tmp,
        }
    }

    fn expected_scratch(e: &E) -> PathBuf {
        e.dir_b.join("scratch-workspaces").join(ACCT_B).join(ORG_B).join(SCRATCH_LEAF)
    }

    fn is_local_uuid(id: &str) -> bool {
        id.strip_prefix("local_")
            .and_then(|u| uuid::Uuid::parse_str(u).ok())
            .map(|u| u.get_version_num() == 4)
            .unwrap_or(false)
    }

    #[test]
    fn list_ordering_flags_transcript_detection() {
        let e = setup();
        let list = e.api.list(&e.a);
        assert_eq!(
            list.iter().map(|s| s.session_id.as_str()).collect::<Vec<_>>(),
            ["local_s2", "local_s1", "local_s3"]
        );
        let (s2, s1, s3) = (&list[0], &list[1], &list[2]);
        assert!(s1.is_scratch);
        assert!(!s2.is_scratch);
        assert!(s1.has_transcript);
        assert!(!s2.has_transcript);
        assert!(!s1.running, "dead pid in registry is not running");
        assert!(s3.running);
        assert!(s2.is_archived);
        assert_eq!(s1.completed_turns, 4);
        assert_eq!(s1.model, "opus");
        assert_eq!(s1.effort, "high");
        assert_eq!(s2.effort, ""); // unknown
        assert_eq!(s1.title, "Scratch one");
        assert_eq!(s1.cli_session_id, e.cli1);
        assert_eq!(s1.last_activity_at, 3000);
        assert_eq!(s1.created_at, 100);
        // Account uuid falls back to config.json when not provided.
        let via_config = e.api.list(&SessionAccount { account_uuid: None, ..e.a.clone() });
        assert_eq!(via_config.len(), 3);
        assert!(e.api.list(&e.b).is_empty());
        // Serialised shape is what panel.js expects from listSessions.
        let v = serde_json::to_value(s1).unwrap();
        let mut k: Vec<&str> = v.as_object().unwrap().keys().map(|s| s.as_str()).collect();
        k.sort();
        let mut want = [
            "sessionId",
            "cliSessionId",
            "title",
            "cwd",
            "isScratch",
            "lastActivityAt",
            "createdAt",
            "model",
            "effort",
            "isArchived",
            "completedTurns",
            "running",
            "hasTranscript",
            "kind",
        ];
        want.sort();
        assert_eq!(k, want);
    }

    #[test]
    fn copy_source_intact_new_ids_cwd_and_transcript_rewritten_memory_merged() {
        let e = setup();
        let expected = expected_scratch(&e);
        let new_slug_dir = e.projects.join(slug(&expected));
        fs::create_dir_all(new_slug_dir.join("memory")).unwrap();
        fs::write(new_slug_dir.join("memory").join("shared.md"), "target-version").unwrap();

        let r = e.api.transfer(&e.a, &e.b, "local_s1", "copy");
        assert!(r.ok, "{r:?}");
        assert!(r.warnings.is_empty());
        assert!(r.target_running);
        let nid = r.new_session_id.clone().unwrap();
        assert!(is_local_uuid(&nid));
        assert_ne!(nid, "local_s1");

        // Source untouched.
        assert!(e.rec_dir_a.join("local_s1.json").exists());
        assert!(e.scratch.join("notes.txt").exists());
        assert!(e.projects.join(slug(&e.scratch)).join(format!("{}.jsonl", e.cli1)).exists());

        // New record.
        let rec_path = e.rec_dir_b.join(format!("{nid}.json"));
        let rec = read_json(&rec_path);
        assert_eq!(rec["sessionId"], nid.as_str());
        assert_ne!(rec["cliSessionId"], e.cli1.as_str());
        assert!(uuid::Uuid::parse_str(rec["cliSessionId"].as_str().unwrap()).is_ok());
        assert_eq!(rec["cwd"], s(&expected).as_str());
        assert_eq!(rec["originCwd"], s(&expected).as_str());
        assert_eq!(rec["gitAnchorsFolderRealpath"], s(&expected).as_str());
        assert_eq!(rec["unknownKey"], json!({"keep": [1, 2, 3]}));
        assert_eq!(rec["zLast"], Value::Null);
        assert_eq!(rec["title"], "Scratch one");
        // Key order preserved, pretty-printed like JSON.stringify(next, null, 2).
        let raw = fs::read_to_string(&rec_path).unwrap();
        let order: Vec<usize> = [
            "\"sessionId\"",
            "\"cliSessionId\"",
            "\"cwd\"",
            "\"originCwd\"",
            "\"unknownKey\"",
            "\"gitAnchorsFolderRealpath\"",
            "\"title\"",
            "\"zLast\"",
        ]
        .iter()
        .map(|k| raw.find(k).unwrap())
        .collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "key order kept: {raw}");
        assert!(raw.starts_with("{\n  \"sessionId\": "));
        assert!(raw.contains("\"keep\": [\n      1,"));
        assert_eq!(fs::read_to_string(expected.join("notes.txt")).unwrap(), "hello");
        assert!(expected.join("sub").exists());

        // Transcript rewritten.
        let new_cli = rec["cliSessionId"].as_str().unwrap().to_string();
        let text = fs::read_to_string(new_slug_dir.join(format!("{new_cli}.jsonl"))).unwrap();
        assert!(!text.contains(&e.cli1), "old cli id gone");
        assert!(!text.contains(&esc(&e.scratch)), "old escaped cwd gone");
        assert!(text.contains(&esc(&expected)));
        let parsed: Vec<Value> = text.trim().split('\n').map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(parsed[0]["cwd"], s(&expected).as_str());
        assert_eq!(parsed[0]["sessionId"], new_cli.as_str());
        assert_eq!(parsed[1]["cwd"], s(&expected.join("sub")).as_str());
        assert!(parsed[0]["text"].as_str().unwrap().starts_with(&format!("look in {}", s(&expected))));

        // Sibling dir + memory merge.
        assert!(new_slug_dir.join(&new_cli).join("custom-title.json").exists());
        assert_eq!(fs::read_to_string(new_slug_dir.join("memory").join("a.md")).unwrap(), "from-source");
        assert_eq!(fs::read_to_string(new_slug_dir.join("memory").join("shared.md")).unwrap(), "target-version");

        // Listing the target shows it as scratch with a transcript.
        let list_b = e.api.list(&e.b);
        assert_eq!(list_b.len(), 1);
        assert!(list_b[0].is_scratch);
        assert!(list_b[0].has_transcript);

        // Serialised result shape is what panel.js expects:
        // {ok,newSessionId,warnings,targetRunning,sourceRunning}.
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(
            v,
            json!({"ok": true, "newSessionId": nid, "warnings": [], "targetRunning": true, "sourceRunning": false})
        );
    }

    #[test]
    fn move_removes_source_record_and_scratch_keeps_source_transcript() {
        let e = setup();
        let r = e.api.transfer(&e.a, &e.b, "local_s1", "move");
        assert!(r.ok, "{r:?}");
        assert!(!e.rec_dir_a.join("local_s1.json").exists());
        assert!(!e.scratch.exists(), "source scratch removed");
        assert!(e.projects.join(slug(&e.scratch)).join(format!("{}.jsonl", e.cli1)).exists(), "source transcript kept");
        assert!(e.rec_dir_b.join(format!("{}.json", r.new_session_id.unwrap())).exists());
    }

    #[test]
    fn move_of_real_project_never_deletes_project_folder() {
        let e = setup();
        let r = e.api.transfer(&e.a, &e.b, "local_s2", "move");
        assert!(r.ok, "{r:?}");
        assert_eq!(r.warnings, ["no-transcript"]);
        assert!(e.project.exists());
        assert!(!e.rec_dir_a.join("local_s2.json").exists());
        let rec = read_json(&e.rec_dir_b.join(format!("{}.json", r.new_session_id.unwrap())));
        assert_eq!(rec["cwd"], s(&e.project).as_str());
        assert_eq!(rec["isArchived"], true);
    }

    /// Mark session 1 (scratch folder, transcript, sibling dir) as open in Claude.
    fn mark_s1_running(e: &E) {
        let me = std::process::id();
        write_json(
            &e.api.registry_dir().join("live-s1.json"),
            &json!({"pid": me, "sessionId": e.cli1, "cwd": s(&e.scratch), "hostSessionId": "local_s1", "status": "busy"}),
        );
    }

    #[test]
    fn running_session_copies_and_moves_but_is_not_deleted() {
        let e = setup();
        // Copy of a running session succeeds and says so.
        let r = e.api.transfer(&e.a, &e.b, "local_s3", "copy");
        assert!(r.ok, "{r:?}");
        assert!(r.source_running);
        assert_eq!(r.warnings, ["no-transcript"]);
        assert!(e.rec_dir_b.join(format!("{}.json", r.new_session_id.clone().unwrap())).exists());
        assert_eq!(serde_json::to_value(&r).unwrap()["sourceRunning"], true);

        // Move of a running session copies it and leaves every source file where it was.
        mark_s1_running(&e);
        let transcript = e.projects.join(slug(&e.scratch)).join(format!("{}.jsonl", e.cli1));
        let before = fs::read(&transcript).unwrap();
        let rec_before = fs::read(e.rec_dir_a.join("local_s1.json")).unwrap();
        let r = e.api.transfer(&e.a, &e.b, "local_s1", "move");
        assert!(r.ok, "{r:?}");
        assert!(r.source_running);
        assert_eq!(r.warnings, ["source-left-open"]);
        assert_eq!(fs::read(e.rec_dir_a.join("local_s1.json")).unwrap(), rec_before, "source record untouched");
        assert!(e.scratch.join("notes.txt").exists(), "source scratch kept");
        assert_eq!(fs::read(&transcript).unwrap(), before, "source transcript untouched");
        let nid = r.new_session_id.clone().unwrap();
        let rec = read_json(&e.rec_dir_b.join(format!("{nid}.json")));
        assert_eq!(rec["cwd"], s(&expected_scratch(&e)).as_str());
        assert!(expected_scratch(&e).join("notes.txt").exists());
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["sourceRunning"], true);
        assert_eq!(v["warnings"], json!(["source-left-open"]));

        // A session that is not running is still moved for real.
        let r = e.api.transfer(&e.a, &e.b, "local_s2", "move");
        assert!(r.ok && !r.source_running, "{r:?}");
        assert!(!e.rec_dir_a.join("local_s2.json").exists());

        // Delete keeps its refusal.
        assert_eq!(e.api.delete(&e.a, "local_s3").unwrap_err().reason, "running");
        assert_eq!(e.api.delete(&e.a, "local_s1").unwrap_err().reason, "running");
        assert!(e.rec_dir_a.join("local_s3.json").exists());
        assert!(e.rec_dir_a.join("local_s1.json").exists());
    }

    fn set_snapshot_hook(f: Option<SnapshotHook>) {
        AFTER_SNAPSHOT_READ.with(|h| *h.borrow_mut() = f);
    }

    fn append(p: &Path, line: &str) {
        use std::io::Write;
        let mut f = fs::OpenOptions::new().append(true).open(p).unwrap();
        f.write_all(line.as_bytes()).unwrap();
    }

    #[test]
    fn snapshot_reads_again_while_the_transcript_grows() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("t.jsonl");
        fs::write(&p, "one\n").unwrap();

        // Grows once during the first read: the second read is steady and has the new line.
        let calls = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let c = calls.clone();
        set_snapshot_hook(Some(Box::new(move |p, attempt| {
            c.set(c.get() + 1);
            if attempt == 1 {
                append(p, "two\n");
            }
        })));
        let got = read_snapshot(&p);
        set_snapshot_hook(None);
        assert_eq!(got.as_deref(), Some(&b"one\ntwo\n"[..]));
        assert_eq!(calls.get(), 2);

        // Keeps growing: three reads in all, then the last read is used.
        let calls = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let c = calls.clone();
        set_snapshot_hook(Some(Box::new(move |p, attempt| {
            c.set(c.get() + 1);
            append(p, &format!("more{attempt}\n"));
        })));
        let got = read_snapshot(&p);
        set_snapshot_hook(None);
        assert_eq!(calls.get(), SNAPSHOT_ATTEMPTS);
        assert_eq!(got.as_deref(), Some(&b"one\ntwo\nmore1\nmore2\n"[..]));

        // Steady file: one read.
        let calls = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let c = calls.clone();
        set_snapshot_hook(Some(Box::new(move |_, _| c.set(c.get() + 1))));
        assert!(read_snapshot(&p).is_some());
        set_snapshot_hook(None);
        assert_eq!(calls.get(), 1);
        assert_eq!(read_snapshot(&tmp.path().join("missing.jsonl")), None);
    }

    #[test]
    fn copy_of_running_session_takes_the_grown_transcript() {
        let e = setup();
        mark_s1_running(&e);
        let late = json!({"type": "assistant", "sessionId": e.cli1, "text": "late line"}).to_string() + "\n";
        set_snapshot_hook(Some(Box::new(move |p, attempt| {
            if attempt == 1 {
                append(p, &late);
            }
        })));
        let r = e.api.transfer(&e.a, &e.b, "local_s1", "copy");
        set_snapshot_hook(None);
        assert!(r.ok && r.source_running, "{r:?}");
        let rec = read_json(&e.rec_dir_b.join(format!("{}.json", r.new_session_id.unwrap())));
        let new_cli = rec["cliSessionId"].as_str().unwrap();
        let text = fs::read_to_string(
            e.projects.join(paths::slug(rec["cwd"].as_str().unwrap())).join(format!("{new_cli}.jsonl")),
        )
        .unwrap();
        let last: Value = serde_json::from_str(text.trim().lines().last().unwrap()).unwrap();
        assert_eq!(last["text"], "late line");
        assert_eq!(last["sessionId"], new_cli, "the late line is rewritten like the rest");
    }

    #[cfg(windows)]
    #[test]
    fn locked_file_is_skipped_and_counted() {
        use std::os::windows::fs::OpenOptionsExt;
        let e = setup();
        // Held open with no sharing, like a file another process is writing.
        let lock = fs::OpenOptions::new().read(true).share_mode(0).open(e.scratch.join("notes.txt")).unwrap();
        let r = e.api.transfer(&e.a, &e.b, "local_s1", "copy");
        drop(lock);
        assert!(r.ok, "{r:?}");
        assert_eq!(r.warnings, ["files-skipped:1"]);
        assert!(!expected_scratch(&e).join("notes.txt").exists());
        assert!(expected_scratch(&e).join("sub").exists());
        assert!(e.rec_dir_b.join(format!("{}.json", r.new_session_id.unwrap())).exists());
    }

    #[test]
    fn half_written_record_is_read_again() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("local_x.json");
        fs::write(&p, "{\"sessionId\":\"local_").unwrap();
        let p2 = p.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(70));
            write_record_atomic(&p2, &OVal::Obj(vec![("sessionId".into(), OVal::Str("local_x".into()))])).unwrap();
        });
        let got = read_record_settled(&p);
        writer.join().unwrap();
        assert_eq!(got.and_then(|r| r.get_str("sessionId").map(String::from)).as_deref(), Some("local_x"));
        fs::write(&p, "[1, 2").unwrap();
        assert_eq!(read_record_settled(&p), None);
        assert_eq!(read_record_settled(&tmp.path().join("missing.json")), None);
    }

    #[test]
    fn handover_target_follows_panel_order_and_skips_unfit_accounts() {
        let v = |id: &str, signed_in: bool, code_ready: bool| AccountView { id: id.into(), signed_in, code_ready };
        let accounts = [
            v("main", true, true),
            v("signed-out", false, true),
            v("no-code", true, false),
            v("limited", true, true),
            v("first-fit", true, true),
            v("second-fit", true, true),
        ];
        let limited: HashSet<AccountId> = ["limited".to_string()].into_iter().collect();
        assert_eq!(pick_handover_target(&accounts, &"main".into(), &limited).as_deref(), Some("first-fit"));
        assert_eq!(pick_handover_target(&accounts, &"first-fit".into(), &limited).as_deref(), Some("main"));
        assert_eq!(pick_handover_target(&accounts, &"x".into(), &HashSet::new()).as_deref(), Some("main"));
        let all: HashSet<AccountId> = ["first-fit", "second-fit", "limited"].iter().map(|s| s.to_string()).collect();
        assert_eq!(pick_handover_target(&accounts, &"main".into(), &all), None);
        assert_eq!(pick_handover_target(&[], &"main".into(), &HashSet::new()), None);
        assert_eq!(pick_handover_target(&accounts[..1], &"main".into(), &HashSet::new()), None);
    }

    #[test]
    fn missing_transcript_is_a_warning_not_an_error() {
        let e = setup();
        fs::remove_file(e.projects.join(slug(&e.scratch)).join(format!("{}.jsonl", e.cli1))).unwrap();
        let r = e.api.transfer(&e.a, &e.b, "local_s1", "copy");
        assert!(r.ok);
        assert_eq!(r.warnings, ["no-transcript"]);
        assert!(e.rec_dir_b.join(format!("{}.json", r.new_session_id.unwrap())).exists());
    }

    #[test]
    fn target_that_never_opened_the_code_tab() {
        let e = setup();
        fs::remove_dir_all(e.dir_b.join("claude-code-sessions")).unwrap();
        let r = e.api.transfer(&e.a, &e.b, "local_s1", "copy");
        assert_eq!(r, TransferResult::fail(Some("target-never-opened-code"), None));
        let no_uuid = SessionAccount {
            id: "c".into(),
            name: "C".into(),
            data_dir: s(&e.root.join("Claude-C")),
            is_main: false,
            account_uuid: None,
            running: false,
        };
        let r2 = e.api.transfer(&e.a, &no_uuid, "local_s1", "copy");
        assert_eq!(r2, TransferResult::fail(Some("target-never-opened-code"), None));
    }

    #[test]
    fn scratch_leaf_collision_gets_dash_2_suffix() {
        let e = setup();
        let taken = expected_scratch(&e);
        fs::create_dir_all(&taken).unwrap();
        let r = e.api.transfer(&e.a, &e.b, "local_s1", "copy");
        assert!(r.ok);
        let rec = read_json(&e.rec_dir_b.join(format!("{}.json", r.new_session_id.unwrap())));
        let want = format!("{}-2", s(&taken));
        assert_eq!(rec["cwd"], want.as_str());
        assert!(e
            .projects
            .join(paths::slug(&want))
            .join(format!("{}.jsonl", rec["cliSessionId"].as_str().unwrap()))
            .exists());
        // And -3 on the next one.
        let r3 = e.api.transfer(&e.a, &e.b, "local_s1", "copy");
        let rec3 = read_json(&e.rec_dir_b.join(format!("{}.json", r3.new_session_id.unwrap())));
        assert_eq!(rec3["cwd"], format!("{}-3", s(&taken)).as_str());
    }

    #[test]
    fn delete_removes_only_the_record() {
        let e = setup();
        assert_eq!(e.api.delete(&e.a, "local_s1"), Ok(()));
        assert!(!e.rec_dir_a.join("local_s1.json").exists());
        assert!(e.projects.join(slug(&e.scratch)).join(format!("{}.jsonl", e.cli1)).exists());
        assert!(e.scratch.exists());
        assert_eq!(e.api.delete(&e.a, "local_s1").unwrap_err().reason, "not-found");
        assert!(e.api.delete(&e.a, "../evil").is_err());
    }

    #[test]
    fn move_from_scratch_subfolder_rewrites_paths_to_whole_scratch_folder() {
        let e = setup();
        let sub = e.scratch.join("sub");
        let cli = "44444444-4444-4444-4444-444444444444";
        write_json(
            &e.rec_dir_a.join("local_s4.json"),
            &json!({"sessionId": "local_s4", "cliSessionId": cli, "cwd": s(&sub), "originCwd": s(&e.scratch),
                    "gitAnchorsFolderRealpath": s(&e.scratch), "title": "Sub", "lastActivityAt": 10}),
        );
        let slug_dir = e.projects.join(slug(&sub));
        fs::create_dir_all(&slug_dir).unwrap();
        fs::write(
            slug_dir.join(format!("{cli}.jsonl")),
            json!({"sessionId": cli, "cwd": s(&sub), "file": s(&e.scratch.join("notes.txt"))}).to_string() + "\n",
        )
        .unwrap();

        let r = e.api.transfer(&e.a, &e.b, "local_s4", "move");
        assert!(r.ok, "{r:?}");
        let dst = expected_scratch(&e);
        let rec = read_json(&e.rec_dir_b.join(format!("{}.json", r.new_session_id.unwrap())));
        assert_eq!(rec["cwd"], s(&dst.join("sub")).as_str());
        assert_eq!(rec["originCwd"], s(&dst).as_str());
        let new_cli = rec["cliSessionId"].as_str().unwrap();
        let line: Value = serde_json::from_str(
            fs::read_to_string(
                e.projects.join(paths::slug(rec["cwd"].as_str().unwrap())).join(format!("{new_cli}.jsonl")),
            )
            .unwrap()
            .trim(),
        )
        .unwrap();
        assert_eq!(line["cwd"], s(&dst.join("sub")).as_str());
        assert_eq!(line["file"], s(&dst.join("notes.txt")).as_str(), "path to a sibling of cwd is rewritten too");
        assert!(!e.scratch.exists(), "source scratch removed");
    }

    #[test]
    fn failed_scratch_copy_leaves_nothing_behind() {
        let e = setup();
        FAIL_COPY.with(|f| f.set(true));
        let r = e.api.transfer(&e.a, &e.b, "local_s1", "copy");
        FAIL_COPY.with(|f| f.set(false));
        assert!(!r.ok);
        assert_eq!(r.reason.as_deref(), Some("error"));
        assert_eq!(r.error.as_deref(), Some("disk full"));
        let dst_parent = e.dir_b.join("scratch-workspaces").join(ACCT_B).join(ORG_B);
        let left: Vec<_> = if dst_parent.exists() { fs::read_dir(&dst_parent).unwrap().collect() } else { Vec::new() };
        assert!(left.is_empty());
        assert_eq!(fs::read_dir(&e.rec_dir_b).unwrap().count(), 0);
    }

    #[test]
    fn validation_reasons() {
        let e = setup();
        let r = e.api.transfer(&e.a, &e.b, "local_s1", "teleport");
        assert_eq!(r, TransferResult::fail(None, Some("Invalid mode".into())));
        let r = e.api.transfer(&e.a, &e.b, "local_../x", "copy");
        assert_eq!(r.error.as_deref(), Some("Invalid session id"));
        let r = e.api.transfer(&e.a, &e.a, "local_s1", "move");
        assert_eq!(r.reason.as_deref(), Some("same-account"));
        let r = e.api.transfer(&e.a, &e.b, "local_nope", "copy");
        assert_eq!(r.reason.as_deref(), Some("not-found"));
        fs::write(e.rec_dir_a.join("local_bad.json"), "{broken").unwrap();
        let r = e.api.transfer(&e.a, &e.b, "local_bad", "copy");
        assert_eq!(r, TransferResult::fail(None, Some("Session record is unreadable".into())));
        assert_eq!(e.api.list(&e.a).len(), 3, "unreadable record skipped in list");
        let empty = SessionAccount { data_dir: String::new(), ..e.b.clone() };
        assert_eq!(e.api.transfer(&e.a, &empty, "local_s1", "copy").reason.as_deref(), Some("error"));
        // Copy within the same account is allowed (a duplicate).
        let r = e.api.transfer(&e.a, &e.a, "local_s2", "copy");
        assert!(r.ok, "{r:?}");
    }

    #[test]
    fn live_sessions_are_counted_per_account_from_the_records() {
        let e = setup();
        // s3 is live through its hostSessionId; s1's registry pid is dead.
        assert_eq!(e.api.live_sessions_in(&e.a), 1);
        assert_eq!(e.api.live_sessions_in(&e.b), 0);
        // A live entry for B that names only the session id (the record's cliSessionId).
        let cli = "44444444-4444-4444-4444-444444444444";
        write_json(
            &e.rec_dir_b.join("local_b1.json"),
            &json!({"sessionId": "local_b1", "cliSessionId": cli, "cwd": s(&e.project), "title": "B live"}),
        );
        assert_eq!(e.api.live_sessions_in(&e.b), 0, "no registry entry yet");
        let registry = e.api.home.join(".claude").join("sessions");
        write_json(&registry.join("live-b.json"), &json!({"pid": std::process::id(), "sessionId": cli}));
        assert_eq!(e.api.live_sessions_in(&e.b), 1);
        assert_eq!(e.api.live_sessions_in(&e.a), 1);
        // Read at call time: the entry going away is seen at once.
        fs::remove_file(registry.join("live-b.json")).unwrap();
        assert_eq!(e.api.live_sessions_in(&e.b), 0);
        let empty = SessionAccount { data_dir: String::new(), ..e.b.clone() };
        assert_eq!(e.api.live_sessions_in(&empty), 0);
    }

    #[test]
    fn an_unreadable_live_registry_entry_counts_as_live() {
        let e = setup();
        let registry = e.api.home.join(".claude").join("sessions");
        let own = registry.join(format!("{}.json", std::process::id()));
        // Part-written (it held A's live s3): the owner cannot be told, so every account counts it.
        fs::write(&own, "{\"pid\": ").unwrap();
        assert_eq!(e.api.live_sessions_in(&e.b), 1);
        assert_eq!(e.api.live_sessions_in(&e.a), 1);
        // Readable but naming no session.
        write_json(&own, &json!({"pid": std::process::id()}));
        assert_eq!(e.api.live_sessions_in(&e.b), 1);
        // Once it names a session of A, B is free again.
        write_json(&own, &json!({"pid": std::process::id(), "hostSessionId": "local_s1"}));
        assert_eq!(e.api.live_sessions_in(&e.b), 0);
        // A broken file of a dead pid, or one whose name is not a pid, is not counted.
        fs::remove_file(&own).unwrap();
        fs::write(registry.join("4294967294.json"), "{\"pid\": ").unwrap();
        fs::write(registry.join("notes.json"), "{broken").unwrap();
        assert_eq!(e.api.live_sessions_in(&e.b), 0);
    }

    #[test]
    fn helpers_match_js() {
        assert!(valid_session_id("local_abc-DEF-123"));
        assert!(!valid_session_id("local_"));
        assert!(!valid_session_id("local_a/b"));
        assert!(!valid_session_id(&format!("local_{}", "a".repeat(65))));
        assert_eq!(parse_int_prefix("1234.json"), Some(1234));
        assert_eq!(parse_int_prefix("x.json"), None);
        assert!(pid_alive(i64::from(std::process::id())));
        assert!(!pid_alive(i64::from(DEAD_PID)));
        assert!(!pid_alive(0));
        assert_eq!(json_escaped(r"C:\a b"), r"C:\\a b");
        let pairs = vec![("ab".to_string(), "X".to_string()), ("abc".to_string(), "Y".to_string())];
        assert_eq!(replace_all("abcab", &pairs), "YX");
        let parts = |v: &[&str]| Some(v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        if cfg!(windows) {
            assert_eq!(rel_inside(Path::new(r"C:\A\b"), r"c:\a\B\c\D"), parts(&["c", "D"]));
            assert_eq!(rel_inside(Path::new(r"C:\A\b"), r"C:\A\bc"), None);
            assert_eq!(rel_inside(Path::new(r"C:\A\b"), r"C:\A\b\"), parts(&[]));
        } else {
            assert_eq!(rel_inside(Path::new("/A/b"), "/a/B/c/D"), parts(&["c", "D"]));
            assert_eq!(rel_inside(Path::new("/A/b"), "/A//b/./c/"), parts(&["c"]));
            assert_eq!(rel_inside(Path::new("/A/b"), "/A/bc"), None);
            assert_eq!(rel_inside(Path::new("/A/b"), "/A"), None);
            assert_eq!(rel_inside(Path::new("/A/b"), "/A/b/"), parts(&[]));
        }
        assert_eq!(rel_inside(Path::new("/A/b"), ""), None);
        let v: OVal = serde_json::from_str(r#"{"b":1,"a":2.0,"b":3}"#).unwrap();
        assert_eq!(serde_json::to_string(&v).unwrap(), r#"{"b":3,"a":2}"#);
    }
}
