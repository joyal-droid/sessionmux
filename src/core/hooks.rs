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

//! Claude Code hooks in the user's `%USERPROFILE%\.claude\settings.json`.
//!
//! API:
//! ```ignore
//! pub struct HookPlan { pub entries: Vec<(String, serde_json::Value)> }
//! pub fn plan(exe: &Path) -> HookPlan
//! pub fn preview_json(plan: &HookPlan) -> String
//! pub fn install(settings_path: &Path, plan: &HookPlan) -> Result<InstallReport, HooksError>
//! pub fn remove(settings_path: &Path, exe: &Path) -> Result<RemoveReport, HooksError>
//! pub fn status(settings_path: &Path, exe: &Path) -> HookStatus
//! pub fn user_settings_path() -> PathBuf
//! ```
//!
//! Sessionmux adds exactly three entries, all in exec form (`command` + `args`, no shell) and all
//! naming this exe: `PermissionRequest` → `--approve-hook`, `Notification` (the waiting, idle,
//! finished and quota types) → `--notify-hook`, `StopFailure` (`rate_limit`) → `--notify-hook`.
//! An entry is ours when its `command` is our exe path and its `args` are one of our hook
//! arguments; nothing else in the file is ever changed. Before every write the file is copied to
//! `settings.json.sessionmux-bak`; writes are atomic. Project settings files are never touched.
//!
//! The file is edited as an ordered tree that keeps every key in its place and every number and
//! string exactly as written, and it is written back with the indentation, line endings, final
//! newline and byte-order mark it had, so a file in Claude Code's own layout comes back
//! byte-for-byte apart from our entries. A file that is not a JSON object is never written.

#![allow(dead_code)]

use crate::util::json;
use serde_json::{json, Value};
use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The argument of the approval helper.
pub const ARG_APPROVE: &str = "--approve-hook";
/// The argument of the notification helper.
pub const ARG_NOTIFY: &str = "--notify-hook";
/// Suffix of the backup kept next to the settings file.
pub const BACKUP_SUFFIX: &str = ".sessionmux-bak";
/// The `Notification` types Sessionmux listens to.
pub const NOTIFY_MATCHER: &str = "permission_prompt|idle_prompt|agent_needs_input|agent_completed|quota_auto_resume_fired|quota_auto_resume_stale|quota_auto_resume_disabled";
/// Seconds Claude Code waits for the approval helper (the helper itself gives up after 85 s).
pub const APPROVE_TIMEOUT: u64 = 90;
/// Seconds Claude Code waits for the notification helper.
pub const NOTIFY_TIMEOUT: u64 = 5;

/// The entries to add, as (hook event, matcher entry) in file order.
#[derive(Clone, Debug, PartialEq)]
pub struct HookPlan {
    pub entries: Vec<(String, Value)>,
}

/// What `install` did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstallReport {
    /// Entries written (entries already present are not added twice).
    pub added: usize,
    /// A backup was written by this call.
    pub backup_written: bool,
}

/// What `remove` did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoveReport {
    /// Entries of ours taken out of the file.
    pub removed: usize,
    /// The file was rewritten (false when there was nothing of ours in it).
    pub written: bool,
}

/// The state of our hooks in one settings file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HookStatus {
    /// Every entry of the plan is present.
    pub installed: bool,
    /// `disableAllHooks` is set: no hook runs.
    pub disabled_all: bool,
    /// `allowManagedHooksOnly` is set: hooks from this file do not run.
    pub managed_only: bool,
    /// `settings.json.sessionmux-bak` exists.
    pub backup_exists: bool,
}

/// Why the settings file was left as it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HooksError {
    /// The file exists but is not a JSON object; it is never overwritten.
    Unparseable(String),
    /// Reading, backing up or writing failed.
    Io(String),
    /// Not available in this build.
    NotImplemented,
}

impl fmt::Display for HooksError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HooksError::Unparseable(m) => write!(f, "settings file is not valid JSON: {m}"),
            HooksError::Io(m) => write!(f, "{m}"),
            HooksError::NotImplemented => write!(f, "not implemented"),
        }
    }
}

impl std::error::Error for HooksError {}

/// One command hook running `exe` with `arg`.
fn command_hook(exe: &Path, arg: &str, timeout: u64) -> Value {
    json!({ "type": "command", "command": exe.to_string_lossy(), "args": [arg], "timeout": timeout })
}

/// The three entries for `exe`.
pub fn plan(exe: &Path) -> HookPlan {
    let entry = |matcher: &str, arg: &str, timeout: u64| json!({ "matcher": matcher, "hooks": [command_hook(exe, arg, timeout)] });
    HookPlan {
        entries: vec![
            ("PermissionRequest".to_string(), entry("", ARG_APPROVE, APPROVE_TIMEOUT)),
            ("Notification".to_string(), entry(NOTIFY_MATCHER, ARG_NOTIFY, NOTIFY_TIMEOUT)),
            ("StopFailure".to_string(), entry("rate_limit", ARG_NOTIFY, NOTIFY_TIMEOUT)),
        ],
    }
}

/// The entries as they appear under `hooks` in the file, pretty-printed for the confirm dialog.
pub fn preview_json(plan: &HookPlan) -> String {
    let mut events = J::Obj(Vec::new());
    for (event, entry) in &plan.entries {
        if let J::Arr(list) = events.entry(event, J::Arr(Vec::new())) {
            list.push(J::from_value(entry));
        }
    }
    let root = J::Obj(vec![(Key::new("hooks"), events)]);
    let mut out = String::new();
    root.write(&Style::default(), 0, &mut out);
    out
}

/// Add the plan's entries to the settings file (created when missing), keeping everything else.
pub fn install(settings_path: &Path, plan: &HookPlan) -> Result<InstallReport, HooksError> {
    let mut doc = Doc::load(settings_path)?;
    // The backup is the file without any entry of ours, so restoring it never brings a hook
    // back: a file that already holds one of ours (from an earlier install) is not backed up.
    let exe = plan.entries.iter().find_map(|(_, e)| Planned::of(e)).map(|p| p.exe).unwrap_or_default();
    let pristine = doc.original.is_some() && count_ours(&doc.root, &exe) == 0;
    let hooks = doc.root.entry("hooks", J::Obj(Vec::new()));
    if !matches!(hooks, J::Obj(_)) {
        return Err(HooksError::Unparseable("\"hooks\" is not an object".into()));
    }
    let mut added = 0;
    for (event, entry) in &plan.entries {
        let Some(want) = Planned::of(entry) else { continue };
        let list = match hooks.entry(event, J::Arr(Vec::new())) {
            J::Arr(list) => list,
            _ => return Err(HooksError::Unparseable(format!("\"hooks.{event}\" is not an array"))),
        };
        if !list.iter().any(|e| want.present_in(e)) {
            list.push(J::from_value(entry));
            added += 1;
        }
    }
    if added == 0 {
        return Ok(InstallReport { added: 0, backup_written: false });
    }
    doc.save(settings_path, pristine)?;
    Ok(InstallReport { added, backup_written: pristine })
}

/// Take every entry of ours (command == `exe`, our arguments) out of the settings file.
pub fn remove(settings_path: &Path, exe: &Path) -> Result<RemoveReport, HooksError> {
    let mut doc = Doc::load(settings_path)?;
    if doc.original.is_none() {
        return Ok(RemoveReport::default());
    }
    let exe = exe.to_string_lossy();
    let removed = match doc.root.get_mut("hooks") {
        Some(hooks) => remove_ours(hooks, &exe),
        None => 0,
    };
    if removed == 0 {
        return Ok(RemoveReport::default());
    }
    if matches!(doc.root.get("hooks"), Some(J::Obj(m)) if m.is_empty()) {
        doc.root.remove_key("hooks");
    }
    // The backup keeps the file as it was before the install; the file being changed here
    // holds our entries, so it is never backed up.
    doc.save(settings_path, false)?;
    Ok(RemoveReport { removed, written: true })
}

/// Remove our hooks under `hooks`, then the matcher entries and event arrays that this left
/// empty. Returns how many hooks were removed.
fn remove_ours(hooks: &mut J, exe: &str) -> usize {
    let J::Obj(events) = hooks else { return 0 };
    let mut removed = 0;
    events.retain_mut(|(_, list)| {
        let J::Arr(entries) = list else { return true };
        let mut here = 0;
        entries.retain_mut(|entry| {
            let Some(J::Arr(hs)) = entry.get_mut("hooks") else { return true };
            let before = hs.len();
            hs.retain(|h| ours(h, exe).is_none());
            let n = before - hs.len();
            here += n;
            n == 0 || !hs.is_empty()
        });
        removed += here;
        here == 0 || !entries.is_empty()
    });
    removed
}

/// How many hooks of ours (command == `exe`, our arguments) the file has.
fn count_ours(root: &J, exe: &str) -> usize {
    let Some(J::Obj(events)) = root.get("hooks") else { return 0 };
    events
        .iter()
        .filter_map(|(_, list)| match list {
            J::Arr(entries) => Some(entries),
            _ => None,
        })
        .flatten()
        .filter_map(|entry| match entry.get("hooks") {
            Some(J::Arr(hs)) => Some(hs.iter().filter(|h| ours(h, exe).is_some()).count()),
            _ => None,
        })
        .sum()
}

/// Read the settings file and report our entries and the switches that stop hooks from running.
/// A missing or unreadable file reads as nothing installed.
pub fn status(settings_path: &Path, exe: &Path) -> HookStatus {
    let backup_exists = backup_path(settings_path).exists();
    let Ok(doc) = Doc::load(settings_path) else { return HookStatus { backup_exists, ..HookStatus::default() } };
    let flag = |k: &str| matches!(doc.root.get(k), Some(J::Lit(v)) if v == "true");
    let hooks = doc.root.get("hooks");
    let installed = plan(exe).entries.iter().all(|(event, entry)| {
        let Some(want) = Planned::of(entry) else { return false };
        matches!(hooks.and_then(|h| h.get(event)), Some(J::Arr(list)) if list.iter().any(|e| want.present_in(e)))
    });
    HookStatus {
        installed,
        disabled_all: flag("disableAllHooks"),
        managed_only: flag("allowManagedHooksOnly"),
        backup_exists,
    }
}

/// `%USERPROFILE%\.claude\settings.json`, shared by every account.
pub fn user_settings_path() -> PathBuf {
    crate::util::paths::home().join(".claude").join("settings.json")
}

/// `settings.json` → `settings.json.sessionmux-bak`.
fn backup_path(settings_path: &Path) -> PathBuf {
    let mut p = settings_path.as_os_str().to_owned();
    p.push(BACKUP_SUFFIX);
    PathBuf::from(p)
}

/// Two spellings of the same exe path: case-insensitive, either separator, quotes ignored.
fn same_exe(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim().trim_matches('"').replace('/', "\\").to_lowercase();
    let (a, b) = (norm(a), norm(b));
    !a.is_empty() && a == b
}

/// The argument of one of our hooks: `command` names `exe` and `args` is exactly one of ours.
fn ours<'a>(hook: &'a J, exe: &str) -> Option<&'a str> {
    let command = hook.get("command")?.as_str()?;
    let J::Arr(args) = hook.get("args")? else { return None };
    let [arg] = args.as_slice() else { return None };
    let arg = arg.as_str()?;
    (same_exe(command, exe) && (arg == ARG_APPROVE || arg == ARG_NOTIFY)).then_some(arg)
}

/// A planned entry reduced to what identifies it in a file.
struct Planned {
    matcher: String,
    exe: String,
    arg: String,
}

impl Planned {
    fn of(entry: &Value) -> Option<Planned> {
        let hook = entry.get("hooks")?.get(0)?;
        Some(Planned {
            matcher: entry.get("matcher").and_then(Value::as_str).unwrap_or("").to_string(),
            exe: hook.get("command")?.as_str()?.to_string(),
            arg: hook.get("args")?.get(0)?.as_str()?.to_string(),
        })
    }

    /// True when `entry` (a matcher entry from the file) has the same matcher and runs our hook
    /// with the same argument.
    fn present_in(&self, entry: &J) -> bool {
        let matcher = entry.get("matcher").and_then(J::as_str).unwrap_or("");
        matcher == self.matcher
            && matches!(entry.get("hooks"), Some(J::Arr(hs)) if hs.iter().any(|h| ours(h, &self.exe) == Some(self.arg.as_str())))
    }
}

/// A settings file as read, with what is needed to write it back in its own layout.
struct Doc {
    root: J,
    style: Style,
    bom: bool,
    /// The file's bytes when it existed.
    original: Option<Vec<u8>>,
}

impl Doc {
    fn load(path: &Path) -> Result<Doc, HooksError> {
        match std::fs::read(path) {
            Ok(b) => Doc::parse(&b),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Ok(Doc { root: J::Obj(Vec::new()), style: Style::new_file(), bom: false, original: None })
            }
            Err(e) => Err(HooksError::Io(format!("could not read {}: {e}", file_name(path)))),
        }
    }

    /// The content of a file that exists.
    fn parse(bytes: &[u8]) -> Result<Doc, HooksError> {
        let bytes = bytes.to_vec();
        let bom = bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
        let body = if bom { &bytes[3..] } else { &bytes[..] };
        let text = std::str::from_utf8(body).map_err(|e| HooksError::Unparseable(e.to_string()))?;
        if text.trim().is_empty() {
            return Ok(Doc { root: J::Obj(Vec::new()), style: Style::new_file(), bom, original: Some(bytes) });
        }
        let check: Value = serde_json::from_str(text).map_err(|e| HooksError::Unparseable(e.to_string()))?;
        if !check.is_object() {
            return Err(HooksError::Unparseable("the top level is not an object".into()));
        }
        let root = Parser { s: text, b: text.as_bytes(), i: 0 }.document().map_err(HooksError::Unparseable)?;
        Ok(Doc { root, style: Style::detect(text), bom, original: Some(bytes) })
    }

    /// With `backup`, keep the file as it was (when it existed) next to it first; then write the
    /// new content atomically.
    fn save(&self, path: &Path, backup: bool) -> Result<(), HooksError> {
        let io_err = |what: &str, e: io::Error| HooksError::Io(format!("could not {what} {}: {e}", file_name(path)));
        if let (true, Some(bytes)) = (backup, &self.original) {
            write_bytes_atomic(&backup_path(path), bytes).map_err(|e| io_err("back up", e))?;
        }
        let mut text = String::new();
        self.root.write(&self.style, 0, &mut text);
        if self.style.final_newline {
            text.push_str(self.style.newline);
        }
        let mut out = Vec::with_capacity(text.len() + 3);
        if self.bom {
            out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        }
        out.extend_from_slice(text.as_bytes());
        write_bytes_atomic(path, &out).map_err(|e| io_err("write", e))
    }
}

/// The file name only, for messages (no folder names end up in the log).
fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Write bytes atomically: temp file next to the target, fsync, rename over it.
fn write_bytes_atomic(p: &Path, data: &[u8]) -> io::Result<()> {
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut tmp = p.as_os_str().to_owned();
    let rnd = uuid::Uuid::new_v4().simple().to_string();
    tmp.push(format!(".tmp-{}-{}", std::process::id(), &rnd[..8]));
    let tmp = PathBuf::from(tmp);
    let result = (|| {
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        json::rename_with_retry(&tmp, p)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// How a file is laid out.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Style {
    indent: String,
    newline: &'static str,
    final_newline: bool,
}

impl Default for Style {
    /// `JSON.stringify(value, null, 2)`.
    fn default() -> Style {
        Style { indent: "  ".into(), newline: "\n", final_newline: false }
    }
}

impl Style {
    fn new_file() -> Style {
        Style { final_newline: true, ..Style::default() }
    }

    /// The indentation of the first indented line, the line ending in use and whether the file
    /// ends with a newline. A file on one line is written with two-space indentation.
    fn detect(text: &str) -> Style {
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let indent = text
            .split('\n')
            .skip(1)
            .map(|l| l.trim_end_matches('\r'))
            .find(|l| !l.trim().is_empty())
            .map(|l| l[..l.len() - l.trim_start_matches([' ', '\t']).len()].to_string())
            .filter(|i| !i.is_empty())
            .unwrap_or_else(|| "  ".into());
        Style { indent, newline, final_newline: text.ends_with('\n') }
    }
}

/// An object key as written and as read.
#[derive(Clone, Debug, PartialEq)]
struct Key {
    raw: String,
    val: String,
}

impl Key {
    fn new(val: &str) -> Key {
        Key { raw: quote(val), val: val.to_string() }
    }
}

/// An ordered JSON value. Literals (null, booleans, numbers) and strings keep their source text.
#[derive(Clone, Debug, PartialEq)]
enum J {
    Lit(String),
    Str { raw: String, val: String },
    Arr(Vec<J>),
    Obj(Vec<(Key, J)>),
}

fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// Keys of our entries in the order they are written; other keys follow.
const KEY_ORDER: &[&str] = &["matcher", "type", "command", "args", "timeout", "hooks"];

impl J {
    fn from_value(v: &Value) -> J {
        match v {
            Value::Null => J::Lit("null".into()),
            Value::Bool(b) => J::Lit(b.to_string()),
            Value::Number(n) => J::Lit(n.to_string()),
            Value::String(s) => J::Str { raw: quote(s), val: s.clone() },
            Value::Array(a) => J::Arr(a.iter().map(J::from_value).collect()),
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort_by_key(|k| KEY_ORDER.iter().position(|o| o == k).unwrap_or(KEY_ORDER.len()));
                J::Obj(keys.into_iter().map(|k| (Key::new(k), J::from_value(&m[k]))).collect())
            }
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            J::Str { val, .. } => Some(val),
            _ => None,
        }
    }

    fn get(&self, key: &str) -> Option<&J> {
        match self {
            J::Obj(m) => m.iter().find(|(k, _)| k.val == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn get_mut(&mut self, key: &str) -> Option<&mut J> {
        match self {
            J::Obj(m) => m.iter_mut().find(|(k, _)| k.val == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The member `key` of an object, appended with `default` when missing. On a non-object this
    /// returns the value itself untouched, which callers reject by its type.
    fn entry(&mut self, key: &str, default: J) -> &mut J {
        let J::Obj(m) = self else { return self };
        let i = match m.iter().position(|(k, _)| k.val == key) {
            Some(i) => i,
            None => {
                m.push((Key::new(key), default));
                m.len() - 1
            }
        };
        &mut m[i].1
    }

    fn remove_key(&mut self, key: &str) {
        if let J::Obj(m) = self {
            m.retain(|(k, _)| k.val != key);
        }
    }

    /// Pretty-print at nesting `level` (`JSON.stringify` layout with the given style).
    fn write(&self, style: &Style, level: usize, out: &mut String) {
        let pad = |out: &mut String, level: usize| {
            out.push_str(style.newline);
            for _ in 0..level {
                out.push_str(&style.indent);
            }
        };
        match self {
            J::Lit(raw) | J::Str { raw, .. } => out.push_str(raw),
            J::Arr(items) if items.is_empty() => out.push_str("[]"),
            J::Obj(members) if members.is_empty() => out.push_str("{}"),
            J::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    pad(out, level + 1);
                    item.write(style, level + 1, out);
                }
                pad(out, level);
                out.push(']');
            }
            J::Obj(members) => {
                out.push('{');
                for (i, (k, v)) in members.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    pad(out, level + 1);
                    out.push_str(&k.raw);
                    out.push_str(": ");
                    v.write(style, level + 1, out);
                }
                pad(out, level);
                out.push('}');
            }
        }
    }
}

/// Reads already-validated JSON text into a [`J`] tree.
struct Parser<'a> {
    s: &'a str,
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn document(mut self) -> Result<J, String> {
        let v = self.value()?;
        self.ws();
        if self.i != self.b.len() {
            return Err("trailing characters".into());
        }
        Ok(v)
    }

    fn ws(&mut self) {
        while self.b.get(self.i).is_some_and(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn peek(&self) -> Result<u8, String> {
        self.b.get(self.i).copied().ok_or_else(|| "unexpected end".to_string())
    }

    fn expect(&mut self, c: u8) -> Result<(), String> {
        self.ws();
        if self.peek()? != c {
            return Err(format!("expected '{}'", c as char));
        }
        self.i += 1;
        Ok(())
    }

    fn value(&mut self) -> Result<J, String> {
        self.ws();
        match self.peek()? {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => {
                let (raw, val) = self.string()?;
                Ok(J::Str { raw, val })
            }
            _ => {
                let start = self.i;
                while self
                    .b
                    .get(self.i)
                    .is_some_and(|c| !matches!(c, b',' | b']' | b'}' | b' ' | b'\t' | b'\n' | b'\r'))
                {
                    self.i += 1;
                }
                if start == self.i {
                    return Err("expected a value".into());
                }
                Ok(J::Lit(self.s[start..self.i].to_string()))
            }
        }
    }

    fn string(&mut self) -> Result<(String, String), String> {
        let start = self.i;
        self.i += 1;
        loop {
            match self.peek()? {
                b'\\' => self.i += 2,
                b'"' => {
                    self.i += 1;
                    break;
                }
                _ => self.i += 1,
            }
        }
        let raw = self.s.get(start..self.i).ok_or("bad string")?.to_string();
        let val: String = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
        Ok((raw, val))
    }

    fn object(&mut self) -> Result<J, String> {
        self.i += 1;
        let mut members = Vec::new();
        self.ws();
        if self.peek()? == b'}' {
            self.i += 1;
            return Ok(J::Obj(members));
        }
        loop {
            self.ws();
            if self.peek()? != b'"' {
                return Err("expected a key".into());
            }
            let (raw, val) = self.string()?;
            self.expect(b':')?;
            members.push((Key { raw, val }, self.value()?));
            self.ws();
            match self.peek()? {
                b',' => self.i += 1,
                b'}' => {
                    self.i += 1;
                    return Ok(J::Obj(members));
                }
                _ => return Err("expected ',' or '}'".into()),
            }
        }
    }

    fn array(&mut self) -> Result<J, String> {
        self.i += 1;
        let mut items = Vec::new();
        self.ws();
        if self.peek()? == b']' {
            self.i += 1;
            return Ok(J::Arr(items));
        }
        loop {
            items.push(self.value()?);
            self.ws();
            match self.peek()? {
                b',' => self.i += 1,
                b']' => {
                    self.i += 1;
                    return Ok(J::Arr(items));
                }
                _ => return Err("expected ',' or ']'".into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = r"C:\Program Files\Sessionmux\Sessionmux.exe";

    fn exe() -> &'static Path {
        Path::new(EXE)
    }

    /// A fixture folder under the system temp folder.
    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        assert!(d.path().starts_with(std::env::temp_dir()));
        let p = d.path().join(".claude").join("settings.json");
        (d, p)
    }

    fn read(p: &Path) -> Value {
        json::read(p).unwrap()
    }

    fn ours_count(v: &Value) -> usize {
        let Some(hooks) = v.get("hooks").and_then(Value::as_object) else { return 0 };
        hooks
            .values()
            .flat_map(|l| l.as_array().into_iter().flatten())
            .flat_map(|e| e.get("hooks").and_then(Value::as_array).into_iter().flatten())
            .filter(|h| h.get("command").and_then(Value::as_str).is_some_and(|c| same_exe(c, EXE)))
            .count()
    }

    const UNRELATED: &str = r#"{
  "model": "opus",
  "permissions": {
    "allow": [
      "Bash(npm run test:*)"
    ],
    "deny": []
  },
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "C:\\tools\\guard.exe",
            "timeout": 1.50
          }
        ]
      }
    ],
    "Notification": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "notify-send \"done \u00e9\""
          }
        ]
      }
    ]
  },
  "env": {
    "A": "1"
  }
}
"#;

    #[test]
    fn plan_and_preview() {
        let p = plan(exe());
        assert_eq!(p.entries.len(), 3);
        let preview = preview_json(&p);
        let v: Value = serde_json::from_str(&preview).unwrap();
        for (event, entry) in &p.entries {
            assert_eq!(&v["hooks"][event][0], entry);
        }
        assert_eq!(v["hooks"]["PermissionRequest"][0]["hooks"][0]["args"][0], ARG_APPROVE);
        assert_eq!(v["hooks"]["PermissionRequest"][0]["hooks"][0]["timeout"], APPROVE_TIMEOUT);
        assert_eq!(v["hooks"]["StopFailure"][0]["matcher"], "rate_limit");
        // Written in the documented order, not alphabetically.
        let order = ["\"PermissionRequest\"", "\"Notification\"", "\"StopFailure\""];
        let at: Vec<usize> = order.iter().map(|k| preview.find(k).unwrap()).collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{preview}");
        assert!(preview.find("\"matcher\"").unwrap() < preview.find("\"type\"").unwrap());
        assert!(preview.find("\"command\"").unwrap() < preview.find("\"args\"").unwrap());
        assert!(preview.starts_with("{\n  \"hooks\": {\n    \"PermissionRequest\": [\n"), "{preview}");
    }

    #[test]
    fn missing_file_round_trip() {
        let (_d, p) = fixture();
        assert_eq!(status(&p, exe()), HookStatus::default());
        assert_eq!(remove(&p, exe()).unwrap(), RemoveReport::default());
        assert!(!p.exists());

        let r = install(&p, &plan(exe())).unwrap();
        assert_eq!(r, InstallReport { added: 3, backup_written: false });
        assert!(!backup_path(&p).exists());
        assert_eq!(ours_count(&read(&p)), 3);
        assert!(status(&p, exe()).installed);

        let r = remove(&p, exe()).unwrap();
        assert_eq!(r, RemoveReport { removed: 3, written: true });
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{}\n");
        assert!(!status(&p, exe()).installed);
    }

    #[test]
    fn empty_file_round_trip() {
        let (_d, p) = fixture();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "").unwrap();
        let r = install(&p, &plan(exe())).unwrap();
        assert_eq!(r, InstallReport { added: 3, backup_written: true });
        assert_eq!(std::fs::read(backup_path(&p)).unwrap(), b"");
        assert!(status(&p, exe()).installed && status(&p, exe()).backup_exists);
        assert_eq!(remove(&p, exe()).unwrap().removed, 3);
        assert_eq!(read(&p), json!({}));
    }

    #[test]
    fn unrelated_hooks_survive_byte_for_byte() {
        let (_d, p) = fixture();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, UNRELATED).unwrap();

        let r = install(&p, &plan(exe())).unwrap();
        assert_eq!(r, InstallReport { added: 3, backup_written: true });
        assert_eq!(std::fs::read_to_string(backup_path(&p)).unwrap(), UNRELATED);
        let installed = std::fs::read_to_string(&p).unwrap();
        // Everything before our first addition is untouched, numbers and escapes included.
        let cut = installed.find("      },\n      {\n        \"matcher\": \"permission_prompt").unwrap();
        assert_eq!(&installed[..cut], &UNRELATED[..cut]);
        let v = read(&p);
        assert_eq!(v["hooks"]["Notification"].as_array().unwrap().len(), 2);
        assert_eq!(v["hooks"]["PreToolUse"][0]["hooks"][0]["command"], r"C:\tools\guard.exe");
        assert_eq!(v["env"]["A"], "1");
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys.len(), 4);
        assert!(installed.find("\"model\"").unwrap() < installed.find("\"env\"").unwrap());

        let r = remove(&p, exe()).unwrap();
        assert_eq!(r, RemoveReport { removed: 3, written: true });
        assert_eq!(std::fs::read_to_string(&p).unwrap(), UNRELATED);
        // Turning it off again leaves the backup as the file was before the install.
        assert_eq!(std::fs::read_to_string(backup_path(&p)).unwrap(), UNRELATED);
    }

    #[test]
    fn backup_never_holds_our_entries() {
        let (_d, p) = fixture();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, UNRELATED).unwrap();
        for _ in 0..2 {
            install(&p, &plan(exe())).unwrap();
            assert_eq!(std::fs::read(backup_path(&p)).unwrap(), UNRELATED.as_bytes());
            remove(&p, exe()).unwrap();
            assert_eq!(std::fs::read(backup_path(&p)).unwrap(), UNRELATED.as_bytes());
            assert_eq!(std::fs::read(&p).unwrap(), UNRELATED.as_bytes());
        }

        // A file that already has one entry of ours is completed, but not backed up.
        let (_d2, q) = fixture();
        std::fs::create_dir_all(q.parent().unwrap()).unwrap();
        let partial = json!({"hooks": {"StopFailure": [{"matcher": "rate_limit", "hooks": [
            {"type": "command", "command": EXE, "args": [ARG_NOTIFY], "timeout": 5}
        ]}]}});
        std::fs::write(&q, serde_json::to_string_pretty(&partial).unwrap()).unwrap();
        let r = install(&q, &plan(exe())).unwrap();
        assert_eq!(r, InstallReport { added: 2, backup_written: false });
        assert!(!backup_path(&q).exists());
        remove(&q, exe()).unwrap();
        assert!(!backup_path(&q).exists());
    }

    #[test]
    fn other_layouts_survive_byte_for_byte() {
        let (_d, p) = fixture();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let crlf = UNRELATED.replace("  ", "    ").replace('\n', "\r\n");
        let mut with_bom = vec![0xEF, 0xBB, 0xBF];
        with_bom.extend_from_slice(crlf.trim_end().as_bytes());
        std::fs::write(&p, &with_bom).unwrap();
        install(&p, &plan(exe())).unwrap();
        let text = std::fs::read(&p).unwrap();
        assert!(text.starts_with(&[0xEF, 0xBB, 0xBF]));
        assert!(!String::from_utf8_lossy(&text).replace("\r\n", "").contains('\n'));
        assert!(status(&p, exe()).installed);
        remove(&p, exe()).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), with_bom);
    }

    #[test]
    fn existing_entries_are_not_added_twice() {
        let (_d, p) = fixture();
        install(&p, &plan(exe())).unwrap();
        let before = std::fs::read(&p).unwrap();
        let r = install(&p, &plan(exe())).unwrap();
        assert_eq!(r, InstallReport { added: 0, backup_written: false });
        assert_eq!(std::fs::read(&p).unwrap(), before);
        assert!(!backup_path(&p).exists());
        assert_eq!(ours_count(&read(&p)), 3);
    }

    #[test]
    fn our_entries_are_found_in_any_spelling() {
        let (_d, p) = fixture();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let other_spelling = EXE.to_uppercase().replace('\\', "/");
        let file = json!({
            "hooks": {
                "PermissionRequest": [{"matcher": "", "hooks": [
                    {"type": "command", "command": other_spelling, "args": [ARG_APPROVE], "timeout": 90},
                    {"type": "command", "command": "C:\\tools\\audit.exe", "args": [ARG_APPROVE]}
                ]}],
                "Stop": [{"hooks": [{"type": "command", "command": EXE, "args": ["--relay"]}]}],
                "SessionStart": []
            }
        });
        std::fs::write(&p, serde_json::to_string_pretty(&file).unwrap()).unwrap();
        // The approval entry already exists; the other two are added.
        assert_eq!(install(&p, &plan(exe())).unwrap().added, 2);
        let r = remove(&p, exe()).unwrap();
        assert_eq!(r.removed, 3);
        let v = read(&p);
        // A foreign hook in our matcher entry, a hook of ours with other arguments and an
        // empty array that was already there all stay.
        assert_eq!(v["hooks"]["PermissionRequest"][0]["hooks"].as_array().unwrap().len(), 1);
        assert_eq!(v["hooks"]["PermissionRequest"][0]["hooks"][0]["command"], "C:\\tools\\audit.exe");
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["args"][0], "--relay");
        assert_eq!(v["hooks"]["SessionStart"], json!([]));
        assert!(v["hooks"].get("Notification").is_none() && v["hooks"].get("StopFailure").is_none());
        assert_eq!(remove(&p, exe()).unwrap(), RemoveReport::default());
    }

    #[test]
    fn switches_that_stop_hooks_are_reported() {
        let (_d, p) = fixture();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "{\n  \"disableAllHooks\": true\n}\n").unwrap();
        let s = status(&p, exe());
        assert!(s.disabled_all && !s.managed_only && !s.installed);
        install(&p, &plan(exe())).unwrap();
        let s = status(&p, exe());
        assert!(s.disabled_all && s.installed);
        assert_eq!(read(&p)["disableAllHooks"], true);
        remove(&p, exe()).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\n  \"disableAllHooks\": true\n}\n");

        std::fs::write(&p, r#"{"allowManagedHooksOnly": true, "disableAllHooks": false}"#).unwrap();
        let s = status(&p, exe());
        assert!(s.managed_only && !s.disabled_all);
    }

    #[test]
    fn unusable_files_are_never_written() {
        let (_d, p) = fixture();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        for body in ["{\"hooks\": ", "[1, 2]", "{\"hooks\": []}", "{\"hooks\": {\"Notification\": {}}}"] {
            std::fs::write(&p, body).unwrap();
            assert!(matches!(install(&p, &plan(exe())), Err(HooksError::Unparseable(_))), "{body}");
            assert_eq!(std::fs::read_to_string(&p).unwrap(), body);
            assert!(!backup_path(&p).exists());
            assert!(!status(&p, exe()).installed);
        }
        std::fs::write(&p, "[1, 2]").unwrap();
        assert!(matches!(remove(&p, exe()), Err(HooksError::Unparseable(_))));
    }

    #[test]
    fn layout_detection() {
        assert_eq!(
            Style::detect("{\r\n\t\"a\": 1\r\n}"),
            Style { indent: "\t".into(), newline: "\r\n", final_newline: false }
        );
        assert_eq!(Style::detect("{\"a\": 1}\n"), Style { indent: "  ".into(), newline: "\n", final_newline: true });
        assert!(user_settings_path().ends_with(Path::new(".claude").join("settings.json")));
    }
}
