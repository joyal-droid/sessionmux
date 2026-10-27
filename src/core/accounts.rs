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

//! Account store.
//!
//! Persists `{ version: 2, accounts: [...], settings: {...}, ignoredDataDirs: [...] }` to
//! `<userData>\sessionmux.json` (2-space pretty JSON, stable key order), starts with only the
//! Main account when no store exists yet, lists existing `%APPDATA%\Claude-*` profile folders as *found* (never
//! auto-added) and merges live process state into the [`Account`] objects handed to the panel.
//!
//! Division of work with app.rs:
//! - No event emitter: [`AccountsStore::change_count`] increments on every change, so app.rs can
//!   diff it and emit `accounts-changed` / `settings` / `discovered-changed`.
//! - Locating Claude / enumerating processes is the caller's job: `list_accounts` takes the
//!   process list and Main's data dir; `list_discovered` / `adopt_profile` take Main's account
//!   uuid; `sync_mcp_config` takes Main's config candidates. `add_account` / `adopt_profile` do
//!   not sync MCP themselves — app.rs calls `sync_mcp_config` right after.
//! - `remove_account` does not kill processes (app.rs kills first). It does refuse to delete a
//!   folder that programs still run from (via `claude_app::processes_under`).
//! - `rename_account(label: Some(""))` clears the label; `None` = unchanged.
//!
//! API:
//! ```ignore
//! impl AccountsStore {
//!   pub fn open(user_data:&Path, app_data:&Path) -> AccountsStore
//!   pub fn list_accounts(&self, procs:&[Proc], main_data_dir:Option<&str>) -> Vec<Account>
//!   pub fn get(&self, id:&str) -> Option<StoredAccount>
//!   pub fn effective_data_dir(&self, id:&str, main_data_dir:Option<&str>) -> Option<String>
//!   pub fn add_account(&mut self, name:&str, color:Option<&str>, label:Option<&str>) -> Result<StoredAccount, ApiError>
//!   pub fn rename_account(&mut self, id:&str, name:Option<&str>, color:Option<&str>, label:Option<&str>) -> Result<(), ApiError>
//!   pub fn remove_account_checked(&mut self, id:&str, delete_data:bool, busy_probe:&dyn Fn(&str)->Vec<(u32,String)>) -> Result<(), ApiError>
//!   pub fn reorder(&mut self, ids:&[String])
//!   pub fn settings(&self) -> Settings
//!   pub fn set_settings(&mut self, patch: serde_json::Value) -> Result<Settings, ApiError>
//!   pub fn list_discovered(&mut self, main_account_uuid:Option<&str>) -> Vec<Discovered>
//!   pub fn adopt_profile(&mut self, data_dir:&str, name:Option<&str>, color:Option<&str>, label:Option<&str>, main_account_uuid:Option<&str>) -> Result<StoredAccount, ApiError>
//!   pub fn ignore_profile(&mut self, data_dir:&str)
//!   pub fn sync_mcp_config(&self, id:&str, candidates:&[String]) -> Result<bool, String>
//!   pub fn start_with_windows(enabled:bool) -> Result<(),String>
//! }
//! pub const PALETTE: [&str; 8];
//! pub fn initials(name:&str)->String;  pub fn name_key(name:&str)->String;
//! pub fn count_sessions(dir:&Path, account_uuid:Option<&str>)->u32;  pub fn code_ready(dir:&Path, account_uuid:Option<&str>)->bool
//! ```

use crate::brand;
use crate::core::claude_app::Proc;
use crate::core::limits::Limited;
use crate::util::{json, log, paths};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Saved bubble position (`{x, y}` in the store, rounded integers).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct StoredAccount {
    pub id: String,
    pub name: String,
    #[serde(rename = "dataDir")]
    pub data_dir: Option<String>,
    pub color: String,
    pub label: Option<String>,
    pub order: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Settings {
    #[serde(rename = "startWithWindows")]
    pub start_with_windows: bool,
    #[serde(rename = "shareMcpConfig")]
    pub share_mcp_config: bool,
    #[serde(rename = "showBubble")]
    pub show_bubble: bool,
    #[serde(rename = "bubblePosition")]
    pub bubble_position: Option<Point>,
    /// system | light | dark (default dark)
    pub theme: String,
    /// e.g. "Ctrl+Shift+Space"
    pub hotkey: String,
    #[serde(rename = "panelSide")]
    pub panel_side: String,
    /// icon | tray (default icon)
    #[serde(rename = "bubbleMode")]
    pub bubble_mode: String,
    /// Route `claude://` sign-in links to the account that is signing in.
    /// Default true; stores written before this setting existed read as true.
    #[serde(rename = "signInRouting", default = "default_true")]
    pub sign_in_routing: bool,
    /// Hand an account's running session over to another account when it hits its plan limit.
    /// Default false; written to the store only while on.
    #[serde(rename = "autoHandover", default, skip_serializing_if = "is_false")]
    pub auto_handover: bool,
    /// Show waiting sessions and approve Claude Code's next action from the panel (installs our
    /// hooks into the user's Claude Code settings). Default false; written to the store only while on.
    #[serde(rename = "approveFromPanel", default, skip_serializing_if = "is_false")]
    pub approve_from_panel: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    /// The settings of a new store.
    fn default() -> Self {
        Settings {
            start_with_windows: false,
            share_mcp_config: true,
            show_bubble: true,
            bubble_position: None,
            theme: "dark".into(),
            hotkey: "Ctrl+Shift+Space".into(),
            panel_side: "auto".into(),
            bubble_mode: "icon".into(),
            sign_in_routing: true,
            auto_handover: false,
            approve_from_panel: false,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Store {
    /// 2
    pub version: u32,
    pub accounts: Vec<StoredAccount>,
    pub settings: Settings,
    #[serde(rename = "ignoredDataDirs", default)]
    pub ignored_data_dirs: Vec<String>,
}

impl Default for Store {
    fn default() -> Self {
        Store { version: 2, accounts: Vec::new(), settings: Settings::default(), ignored_data_dirs: Vec::new() }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Account {
    pub id: String,
    pub name: String,
    #[serde(rename = "dataDir")]
    pub data_dir: Option<String>,
    pub color: String,
    pub initials: String,
    pub label: Option<String>,
    #[serde(rename = "isMain")]
    pub is_main: bool,
    #[serde(rename = "signedIn")]
    pub signed_in: bool,
    #[serde(rename = "accountUuid")]
    pub account_uuid: Option<String>,
    pub running: bool,
    pub pid: Option<u32>,
    #[serde(rename = "sessionCount")]
    pub session_count: u32,
    #[serde(rename = "codeReady")]
    pub code_ready: bool,
    #[serde(rename = "sameSignInAs")]
    pub same_sign_in_as: Option<String>,
    /// The account hit its plan limit (filled in by the app; never stored).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limited: Option<Limited>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Discovered {
    #[serde(rename = "dataDir")]
    pub data_dir: String,
    #[serde(rename = "folderName")]
    pub folder_name: String,
    #[serde(rename = "accountUuid")]
    pub account_uuid: Option<String>,
    #[serde(rename = "signedIn")]
    pub signed_in: bool,
    #[serde(rename = "sameSignInAs")]
    pub same_sign_in_as: Option<String>,
    #[serde(rename = "sessionCount")]
    pub session_count: u32,
}

/// Failure shape for IPC: `{ok:false, error, reason}`.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    /// "name-taken" | "same-sign-in" | "invalid" | "not-found" | "main" | "io"
    /// (core::sessions also uses "running").
    pub reason: &'static str,
    pub error: String,
}

impl ApiError {
    pub fn new(reason: &'static str, error: impl Into<String>) -> ApiError {
        ApiError { reason, error: error.into() }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.error, self.reason)
    }
}

impl std::error::Error for ApiError {}

pub const PALETTE: [&str; 8] = ["#F08A5D", "#5B9CF6", "#3DD6A1", "#B388FF", "#F6C453", "#F2709C", "#4FD1DB", "#9BA3B5"];

/// Allowed `bubbleMode` values.
pub const BUBBLE_MODES: [&str; 2] = ["icon", "tray"];

const STORE_VERSION: u32 = 2;
const STORE_FILE: &str = brand::STORE_FILE;
const MCP_FILE: &str = "claude_desktop_config.json";
const MAX_LABEL: usize = 100;
const NAME_ERROR: &str =
    "Use 1-32 characters: letters, digits, spaces, dot, dash or underscore, starting with a letter or digit.";
const COLOR_ERROR: &str = "Colour must look like #RRGGBB.";

// ---------------------------------------------------------------------------------------------
// Pure helpers (JS semantics)
// ---------------------------------------------------------------------------------------------

/// JS `\s` / `String.prototype.trim` whitespace set.
fn is_js_ws(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_ws)
}

/// `s.replace(/\s+/g, ' ')`.
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.chars() {
        if is_js_ws(c) {
            if !in_ws {
                out.push(' ');
            }
            in_ws = true;
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out
}

/// JS `s.slice(0, n)` measured in UTF-16 code units (never splits a character).
fn slice_utf16(s: &str, n: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for c in s.chars() {
        used += c.len_utf16();
        if used > n {
            break;
        }
        out.push(c);
    }
    out
}

/// Replace every char not accepted by `keep` with '-' (one per UTF-16 unit).
fn dashify(s: &str, keep: impl Fn(char) -> bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if keep(c) {
            out.push(c);
        } else {
            for _ in 0..c.len_utf16() {
                out.push('-');
            }
        }
    }
    out
}

/// JS truthiness of a JSON value.
fn js_truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0 && !f.is_nan()).unwrap_or(true),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// Up to 2 uppercase letters from the first two words of `name`.
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = js_trim(name)
        .split(|c: char| is_js_ws(c) || c == '.' || c == '_' || c == '-')
        .filter(|w| !w.is_empty())
        .collect();
    match words.len() {
        0 => "?".into(),
        1 => {
            // Single word: first two letters/digits ("Design" -> "DE").
            let chars: String = words[0].chars().filter(|c| c.is_ascii_alphanumeric()).take(2).collect();
            if chars.is_empty() {
                "?".into()
            } else {
                chars.to_uppercase()
            }
        }
        _ => {
            let mut s = String::new();
            s.extend(words[0].chars().next());
            s.extend(words[1].chars().next());
            s.to_uppercase()
        }
    }
}

/// Display form of a user-typed name: trimmed with internal whitespace collapsed.
fn clean_name(name: &str) -> String {
    collapse_ws(js_trim(name))
}

/// Comparison key for account names: trimmed, internal whitespace collapsed, lower-cased.
pub fn name_key(name: &str) -> String {
    clean_name(name).to_lowercase()
}

/// Folder-safe form of an account name: `[^A-Za-z0-9._-]` -> '-'.
pub fn sanitize_name(name: &str) -> String {
    dashify(name, |c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// `^[A-Za-z0-9][A-Za-z0-9 ._-]{0,31}$`
fn name_re(n: &str) -> bool {
    let mut chars = n.chars();
    let Some(first) = chars.next() else { return false };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    let mut count = 1;
    for c in chars {
        if !(c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-')) {
            return false;
        }
        count += 1;
    }
    count <= 32
}

/// `^#[0-9A-Fa-f]{6}$`
fn color_re(c: &str) -> bool {
    let b = c.as_bytes();
    b.len() == 7 && b[0] == b'#' && b[1..].iter().all(|x| x.is_ascii_hexdigit())
}

/// Validate a user-typed account name; returns an error string or None.
pub fn validate_name(name: &str) -> Option<&'static str> {
    let n = clean_name(name);
    if n.is_empty() {
        return Some("Name is required.");
    }
    if !name_re(&n) {
        return Some(NAME_ERROR);
    }
    None
}

fn name_taken_err(existing: &str) -> ApiError {
    ApiError::new("name-taken", format!("An account named \"{existing}\" already exists."))
}

fn clean_label(label: Option<&str>) -> Option<String> {
    let t = js_trim(label?);
    if t.is_empty() {
        None
    } else {
        Some(slice_utf16(t, MAX_LABEL))
    }
}

/// JS `Math.round` (halves round towards +∞), saturating into i32.
fn js_round(f: f64) -> i32 {
    (f + 0.5).floor() as i32
}

/// Validated settings partial. Unknown keys are dropped.
#[derive(Default, Debug)]
struct SettingsPatch {
    start_with_windows: Option<bool>,
    share_mcp_config: Option<bool>,
    show_bubble: Option<bool>,
    sign_in_routing: Option<bool>,
    auto_handover: Option<bool>,
    approve_from_panel: Option<bool>,
    bubble_position: Option<Option<Point>>,
    theme: Option<String>,
    panel_side: Option<String>,
    bubble_mode: Option<String>,
    hotkey: Option<String>,
}

fn sanitize_settings(partial: Option<&Value>) -> SettingsPatch {
    let mut out = SettingsPatch::default();
    let Some(Value::Object(p)) = partial else { return out };
    let b = |k: &str| p.get(k).and_then(Value::as_bool);
    out.start_with_windows = b("startWithWindows");
    out.share_mcp_config = b("shareMcpConfig");
    out.show_bubble = b("showBubble");
    out.sign_in_routing = b("signInRouting");
    out.auto_handover = b("autoHandover");
    out.approve_from_panel = b("approveFromPanel");
    if let Some(bp) = p.get("bubblePosition") {
        match bp {
            Value::Null => out.bubble_position = Some(None),
            Value::Object(o) => {
                let fin = |k: &str| {
                    o.get(k).and_then(|v| if v.is_number() { v.as_f64() } else { None }).filter(|f| f.is_finite())
                };
                if let (Some(x), Some(y)) = (fin("x"), fin("y")) {
                    out.bubble_position = Some(Some(Point { x: js_round(x), y: js_round(y) }));
                }
            }
            _ => {}
        }
    }
    let pick =
        |k: &str, allowed: &[&str]| p.get(k).and_then(Value::as_str).filter(|s| allowed.contains(s)).map(String::from);
    out.theme = pick("theme", &["system", "light", "dark"]);
    out.panel_side = pick("panelSide", &["auto", "left", "right"]);
    out.bubble_mode = pick("bubbleMode", &BUBBLE_MODES);
    if let Some(h) = p.get("hotkey").and_then(Value::as_str) {
        let t = js_trim(h);
        if !t.is_empty() && h.encode_utf16().count() <= 64 {
            out.hotkey = Some(t.to_string());
        }
    }
    out
}

fn apply_settings(s: &mut Settings, p: SettingsPatch) {
    if let Some(v) = p.start_with_windows {
        s.start_with_windows = v;
    }
    if let Some(v) = p.share_mcp_config {
        s.share_mcp_config = v;
    }
    if let Some(v) = p.show_bubble {
        s.show_bubble = v;
    }
    if let Some(v) = p.sign_in_routing {
        s.sign_in_routing = v;
    }
    if let Some(v) = p.auto_handover {
        s.auto_handover = v;
    }
    if let Some(v) = p.approve_from_panel {
        s.approve_from_panel = v;
    }
    if let Some(v) = p.bubble_position {
        s.bubble_position = v;
    }
    if let Some(v) = p.theme {
        s.theme = v;
    }
    if let Some(v) = p.panel_side {
        s.panel_side = v;
    }
    if let Some(v) = p.bubble_mode {
        s.bubble_mode = v;
    }
    if let Some(v) = p.hotkey {
        s.hotkey = v;
    }
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).ok()?.modified().ok()
}

fn is_dir(p: &Path) -> bool {
    std::fs::metadata(p).map(|m| m.is_dir()).unwrap_or(false)
}

/// `path.basename(p)` for a Windows path string.
fn basename(p: &str) -> String {
    Path::new(p).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// `path.resolve(p)`.
fn resolve(p: &str) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| PathBuf::from(p))
}

/// `^Claude-.` (case-insensitive) and not `Claude-NativeHost`.
fn is_claude_profile_leaf(leaf: &str) -> bool {
    let l = leaf.to_lowercase();
    l.starts_with("claude-") && l.chars().count() > 7 && l != "claude-nativehost"
}

/// Files whose presence marks a `Claude-*` folder as a Claude profile.
const PROFILE_MARKER_FILES: [&str; 4] = ["config.json", "Local State", "Preferences", "claude_desktop_config.json"];
/// Folder whose presence marks a `Claude-*` folder as a Claude profile.
const PROFILE_MARKER_DIR: &str = "claude-code-sessions";

/// Discovery leaf rule: a profile leaf that is never a native-host folder
/// (`Claude-NativeHost` or anything ending in `-NativeHost`, case-insensitive).
fn is_discoverable_leaf(leaf: &str) -> bool {
    is_claude_profile_leaf(leaf) && !leaf.to_lowercase().ends_with("-nativehost")
}

/// Does `dir` hold at least one profile marker (a marker file, or the Code-sessions folder)?
fn has_profile_marker(dir: &Path) -> bool {
    PROFILE_MARKER_FILES.iter().any(|f| std::fs::metadata(dir.join(f)).map(|m| m.is_file()).unwrap_or(false))
        || is_dir(&dir.join(PROFILE_MARKER_DIR))
}

fn read_config(dir: &Path) -> Option<Value> {
    json::read::<Value>(&dir.join("config.json"))
}

/// `lastKnownAccountUuid` when it is a non-empty string.
fn uuid_from_config(cfg: Option<&Value>) -> Option<String> {
    cfg?.get("lastKnownAccountUuid").and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from)
}

fn signed_in_from_config(cfg: Option<&Value>) -> bool {
    cfg.map(|c| js_truthy(c.get("oauth:tokenCache"))).unwrap_or(false)
}

fn list_dir_names(p: &Path, dirs_only: bool) -> Option<Vec<String>> {
    let rd = std::fs::read_dir(p).ok()?;
    let mut out = Vec::new();
    for e in rd.flatten() {
        if dirs_only && !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        out.push(e.file_name().to_string_lossy().into_owned());
    }
    Some(out)
}

/// Count `local_*.json` Code-session records under `<dataDir>\claude-code-sessions\<acct>\<org>\`.
/// Only the profile's current account-uuid folder when it is known, every uuid folder otherwise.
pub fn count_sessions(dir: &Path, account_uuid: Option<&str>) -> u32 {
    if dir.as_os_str().is_empty() {
        return 0;
    }
    let root = dir.join("claude-code-sessions");
    let accts: Vec<String> = match account_uuid.filter(|u| !u.is_empty()) {
        Some(u) => vec![u.to_string()],
        None => match list_dir_names(&root, true) {
            Some(v) => v,
            None => return 0,
        },
    };
    let mut n = 0u32;
    for a in accts {
        let Some(orgs) = list_dir_names(&root.join(&a), true) else { continue };
        for o in orgs {
            let Some(files) = list_dir_names(&root.join(&a).join(&o), false) else { continue };
            for f in files {
                let l = f.to_lowercase();
                if l.starts_with("local_") && l.ends_with(".json") && l.len() >= "local_.json".len() {
                    n += 1;
                }
            }
        }
    }
    n
}

/// True when the profile can receive a transferred session: its account uuid is known and
/// `claude-code-sessions\<uuid>\` holds at least one org folder.
pub fn code_ready(dir: &Path, account_uuid: Option<&str>) -> bool {
    let Some(u) = account_uuid.filter(|u| !u.is_empty()) else { return false };
    if dir.as_os_str().is_empty() {
        return false;
    }
    list_dir_names(&dir.join("claude-code-sessions").join(u), true).map(|v| !v.is_empty()).unwrap_or(false)
}

/// Least-used palette colour (ties -> palette order) = round-robin that survives removals.
fn next_color<'a>(used_colors: impl Iterator<Item = &'a str>) -> &'static str {
    let mut used = [0usize; 8];
    for c in used_colors {
        let c = c.to_uppercase();
        if let Some(i) = PALETTE.iter().position(|p| *p == c) {
            used[i] += 1;
        }
    }
    let mut best = 0;
    for i in 0..PALETTE.len() {
        if used[i] < used[best] {
            best = i;
        }
    }
    PALETTE[best]
}

/// Record as read from disk, before colours are assigned.
struct RawRec {
    id: String,
    name: String,
    data_dir: Option<String>,
    color: Option<String>,
    label: Option<String>,
    order: f64,
}

/// Normalise a record read from disk (`cleanRecord`); None if unusable.
fn clean_record(r: &Value) -> Option<RawRec> {
    let o = r.as_object()?;
    let rid = o.get("id").and_then(Value::as_str);
    let id = match rid {
        Some("main") => "main".to_string(),
        Some(s) if !s.is_empty() => s.to_string(),
        _ => uuid::Uuid::new_v4().to_string(),
    };
    let data_dir = if id == "main" {
        None
    } else {
        o.get("dataDir").and_then(Value::as_str).map(js_trim).filter(|s| !s.is_empty()).map(String::from)
    };
    if id != "main" && data_dir.is_none() {
        return None;
    }
    let name = match o.get("name").and_then(Value::as_str).map(js_trim).filter(|s| !s.is_empty()) {
        Some(n) => slice_utf16(n, 64),
        None if id == "main" => "Main".to_string(),
        None => basename(data_dir.as_deref().unwrap_or("")),
    };
    let color = o.get("color").and_then(Value::as_str).filter(|c| color_re(c)).map(|c| c.to_uppercase());
    let label = clean_label(o.get("label").and_then(Value::as_str));
    let order = o
        .get("order")
        .and_then(|v| if v.is_number() { v.as_f64() } else { None })
        .filter(|f| f.is_finite())
        .unwrap_or(0.0);
    Some(RawRec { id, name, data_dir, color, label, order })
}

fn listed_in<'a>(mut dirs: impl Iterator<Item = &'a str>, dir: &str) -> bool {
    dirs.any(|d| paths::same_path(d, dir))
}

/// Guarantee invariants (`normalise`): Main present exactly once, unique ids/dataDirs, colours.
fn normalise(s: &Value) -> Store {
    let mut sorted: Vec<RawRec> = s
        .get("accounts")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(clean_record).collect())
        .unwrap_or_default();
    sorted.sort_by(|a, b| a.order.partial_cmp(&b.order).unwrap_or(std::cmp::Ordering::Equal));
    let mut out: Vec<RawRec> = Vec::new();
    for r in sorted {
        if out.iter().any(|x| x.id == r.id) {
            continue;
        }
        if let Some(d) = &r.data_dir {
            if listed_in(out.iter().filter_map(|x| x.data_dir.as_deref()), d) {
                continue;
            }
        }
        out.push(r);
    }
    if !out.iter().any(|x| x.id == "main") {
        let color = next_color(out.iter().filter_map(|x| x.color.as_deref()));
        out.insert(
            0,
            RawRec {
                id: "main".into(),
                name: "Main".into(),
                data_dir: None,
                color: Some(color.into()),
                label: None,
                order: 0.0,
            },
        );
    }
    for i in 0..out.len() {
        if out[i].color.is_none() {
            let c = next_color(out.iter().filter_map(|x| x.color.as_deref()));
            out[i].color = Some(c.into());
        }
    }
    let mut settings = Settings::default();
    apply_settings(&mut settings, sanitize_settings(s.get("settings")));
    let ignored = s
        .get("ignoredDataDirs")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
        .unwrap_or_default();
    Store {
        version: STORE_VERSION,
        accounts: out
            .into_iter()
            .map(|r| StoredAccount {
                id: r.id,
                name: r.name,
                data_dir: r.data_dir,
                color: r.color.unwrap_or_default(),
                label: r.label,
                order: 0,
            })
            .collect(),
        settings,
        ignored_data_dirs: ignored,
    }
}

/// Is `dir` directly under `app_data` with a `Claude-*` leaf (the only folders we ever delete)?
fn is_deletable_profile_dir(dir: &str, app_data: &Path) -> bool {
    if dir.is_empty() {
        return false;
    }
    let abs = resolve(dir);
    let leaf = abs.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let parent = abs.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    paths::same_path(&parent, &app_data.to_string_lossy()) && is_claude_profile_leaf(&leaf)
}

/// `fs.rmSync(dir, {recursive, force, maxRetries: 5, retryDelay: 200})`.
fn remove_dir_retry(dir: &Path) -> std::io::Result<()> {
    let mut attempt = 0;
    loop {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                if attempt >= 5 {
                    return Err(e);
                }
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------------------------

/// The persistent store; folder paths are injectable for tests.
#[derive(Debug)]
pub struct AccountsStore {
    user_data: PathBuf,
    app_data: PathBuf,
    store: Store,
    /// Incremented on every change to accounts, settings or the found folders.
    changes: u64,
}

/// A found folder before identity is read.
struct Found {
    data_dir: String,
    folder_name: String,
}

impl AccountsStore {
    /// Load (or create) the store, normalise it and write it back.
    pub fn open(user_data: &Path, app_data: &Path) -> AccountsStore {
        let mut me = AccountsStore {
            user_data: user_data.to_path_buf(),
            app_data: app_data.to_path_buf(),
            store: Store::default(),
            changes: 0,
        };
        me.load();
        me
    }

    /// `<user_data>\sessionmux.json`.
    pub fn store_path(&self) -> PathBuf {
        self.user_data.join(STORE_FILE)
    }

    /// Counter bumped on every change (accounts, settings, discovered). app.rs compares it to decide what to emit.
    #[allow(dead_code)] // used only by the unit tests
    pub fn change_count(&self) -> u64 {
        self.changes
    }

    /// In-memory store snapshot (exactly what is written to disk).
    #[allow(dead_code)] // used only by the unit tests
    pub fn snapshot(&self) -> Store {
        self.store.clone()
    }

    fn load(&mut self) {
        let store_file = self.store_path();
        let mut raw: Option<Value> = None;
        if store_file.exists() {
            raw = json::read::<Value>(&store_file).filter(|v| v.is_object() || v.is_array());
            if raw.is_none() {
                // Corrupt store: keep a copy for forensics, then rebuild from scratch.
                let mut bak = store_file.clone().into_os_string();
                bak.push(format!(".corrupt-{}", log::now_millis()));
                let _ = std::fs::copy(&store_file, &bak);
                log::warn(&format!("accounts: store unreadable, backed up to {}", PathBuf::from(bak).display()));
            }
        }
        let src = match raw {
            Some(r) => r,
            None => Self::fresh_store(),
        };
        self.store = normalise(&src);
        // Discovery only lists found folders; adding one is the user's choice (adopt_profile).
        if let Err(e) = self.save() {
            log::error(&format!("accounts: failed to write store: {e}"));
        }
    }

    fn save(&mut self) -> std::io::Result<()> {
        for (i, a) in self.store.accounts.iter_mut().enumerate() {
            a.order = i as u32;
        }
        json::write_atomic(&self.store_path(), &self.store)
    }

    fn changed(&mut self) {
        self.changes = self.changes.wrapping_add(1);
    }

    /// A fresh store with only the Main account and default settings, as a raw JSON value.
    fn fresh_store() -> Value {
        let color = next_color(std::iter::empty());
        let settings = serde_json::to_value(Settings::default()).unwrap_or(Value::Null);
        json!({
            "version": STORE_VERSION,
            "accounts": [{ "id": "main", "name": "Main", "dataDir": null, "color": color, "label": null, "order": 0 }],
            "settings": settings,
            "ignoredDataDirs": []
        })
    }

    /// The existing record whose name clashes with `name` (see name_key).
    fn name_taken(&self, name: &str, except_id: Option<&str>) -> Option<&StoredAccount> {
        let n = name_key(name);
        self.store.accounts.iter().find(|a| Some(a.id.as_str()) != except_id && name_key(&a.name) == n)
    }

    fn is_listed(&self, dir: &str) -> bool {
        listed_in(self.store.accounts.iter().filter_map(|a| a.data_dir.as_deref()), dir)
    }

    /// Scan `app_data\Claude-*` for profile folders that are not accounts yet (`scanProfiles`).
    fn scan_profiles(&self, ignored: &[String]) -> Vec<Found> {
        let Ok(rd) = std::fs::read_dir(&self.app_data) else { return Vec::new() };
        let mut out = Vec::new();
        for ent in rd.flatten() {
            if !ent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = ent.file_name().to_string_lossy().into_owned();
            if !is_discoverable_leaf(&name) {
                continue;
            }
            let dir = self.app_data.join(&name);
            let dir_s = dir.to_string_lossy().into_owned();
            if self.is_listed(&dir_s) {
                continue;
            }
            if ignored.iter().any(|d| paths::same_path(d, &dir_s)) {
                continue;
            }
            // Is it a real Claude profile folder (PROFILE_MARKERS)?
            if !has_profile_marker(&dir) {
                continue;
            }
            out.push(Found { data_dir: dir_s, folder_name: name });
        }
        out.sort_by(|a, b| {
            a.folder_name
                .to_lowercase()
                .cmp(&b.folder_name.to_lowercase())
                .then_with(|| b.folder_name.cmp(&a.folder_name))
        });
        out
    }

    /// Effective data dir of a record (Main -> located main data dir).
    fn effective_dir(rec: &StoredAccount, main_data_dir: Option<&str>) -> Option<String> {
        rec.data_dir
            .clone()
            .filter(|d| !d.is_empty())
            .or_else(|| main_data_dir.filter(|d| !d.is_empty()).map(String::from))
    }

    /// `(name, accountUuid)` of every account in display order, Main's uuid given by the caller.
    fn identities(&self, main_account_uuid: Option<&str>) -> Vec<(String, Option<String>)> {
        self.store
            .accounts
            .iter()
            .map(|r| {
                let uuid = match &r.data_dir {
                    None => main_account_uuid.filter(|u| !u.is_empty()).map(String::from),
                    Some(d) => uuid_from_config(read_config(Path::new(d)).as_ref()),
                };
                (r.name.clone(), uuid)
            })
            .collect()
    }

    fn to_account(rec: &StoredAccount, main_data_dir: Option<&str>, procs: &[Proc]) -> Account {
        let dir = Self::effective_dir(rec, main_data_dir);
        let cfg = dir.as_deref().and_then(|d| read_config(Path::new(d)));
        let mine: Vec<&Proc> = procs
            .iter()
            .filter(|p| {
                let pd = p.data_dir.as_deref().filter(|d| !d.is_empty());
                match (&rec.data_dir, pd) {
                    (Some(rd), Some(pd)) => paths::same_path(pd, rd),
                    (Some(_), None) => false,
                    (None, pd) => pd.is_none(),
                }
            })
            .collect();
        let account_uuid = uuid_from_config(cfg.as_ref());
        let dir_path = dir.as_deref().map(Path::new).unwrap_or(Path::new(""));
        Account {
            id: rec.id.clone(),
            name: rec.name.clone(),
            data_dir: rec.data_dir.clone(),
            color: rec.color.clone(),
            initials: initials(&rec.name),
            label: rec.label.clone(),
            is_main: rec.id == "main",
            signed_in: signed_in_from_config(cfg.as_ref()),
            running: !mine.is_empty(),
            pid: mine.first().map(|p| p.pid),
            session_count: count_sessions(dir_path, account_uuid.as_deref()),
            code_ready: code_ready(dir_path, account_uuid.as_deref()),
            account_uuid,
            same_sign_in_as: None,
            limited: None,
        }
    }

    /// All accounts in display order with live running/pid/sessionCount and `sameSignInAs`
    /// (every account sharing a non-null accountUuid with an earlier one points at that one).
    pub fn list_accounts(&self, procs: &[Proc], main_data_dir: Option<&str>) -> Vec<Account> {
        let mut list: Vec<Account> =
            self.store.accounts.iter().map(|r| Self::to_account(r, main_data_dir, procs)).collect();
        let mut first: Vec<(String, String)> = Vec::new(); // (uuid, name)
        for a in list.iter_mut() {
            a.same_sign_in_as = None;
            let Some(u) = a.account_uuid.clone() else { continue };
            match first.iter().find(|(fu, _)| *fu == u) {
                Some((_, name)) => a.same_sign_in_as = Some(name.clone()),
                None => first.push((u, a.name.clone())),
            }
        }
        list
    }

    pub fn get(&self, id: &str) -> Option<StoredAccount> {
        self.store.accounts.iter().find(|a| a.id == id).cloned()
    }

    /// Data dir for identity/session lookups: the account's own folder, Main -> `main_data_dir`.
    #[allow(dead_code)] // used only by the unit tests
    pub fn effective_data_dir(&self, id: &str, main_data_dir: Option<&str>) -> Option<String> {
        let rec = self.store.accounts.iter().find(|a| a.id == id)?;
        Self::effective_dir(rec, main_data_dir)
    }

    /// Create a new account + its `%APPDATA%\Claude-<name>` folder. The caller syncs MCP next.
    pub fn add_account(
        &mut self,
        name: &str,
        color: Option<&str>,
        label: Option<&str>,
    ) -> Result<StoredAccount, ApiError> {
        if let Some(err) = validate_name(name) {
            return Err(ApiError::new("invalid", err));
        }
        let clean = clean_name(name);
        if let Some(clash) = self.name_taken(&clean, None) {
            return Err(name_taken_err(&clash.name));
        }
        if let Some(c) = color {
            if !color_re(c) {
                return Err(ApiError::new("invalid", COLOR_ERROR));
            }
        }
        let base = self.app_data.join(format!("Claude-{}", sanitize_name(&clean))).to_string_lossy().into_owned();
        let mut data_dir = base.clone();
        let mut i = 2;
        while Path::new(&data_dir).exists() || self.is_listed(&data_dir) {
            data_dir = format!("{base}-{i}");
            i += 1;
        }
        if let Err(e) = std::fs::create_dir_all(&data_dir) {
            return Err(ApiError::new("io", format!("Could not create the profile folder: {e}")));
        }
        let rec = StoredAccount {
            id: uuid::Uuid::new_v4().to_string(),
            name: clean,
            data_dir: Some(data_dir.clone()),
            color: match color {
                Some(c) => c.to_uppercase(),
                None => next_color(self.store.accounts.iter().map(|a| a.color.as_str())).into(),
            },
            label: clean_label(label),
            order: self.store.accounts.len() as u32,
        };
        self.store.accounts.push(rec);
        // A re-added folder should no longer be suppressed from discovery.
        let prev_ignored = self.store.ignored_data_dirs.clone();
        self.store.ignored_data_dirs.retain(|d| !paths::same_path(d, &data_dir));
        if let Err(e) = self.save() {
            self.store.accounts.pop();
            self.store.ignored_data_dirs = prev_ignored;
            return Err(ApiError::new("io", format!("Could not save accounts: {e}")));
        }
        self.changed();
        self.store.accounts.last().cloned().ok_or_else(|| ApiError::new("io", "Could not save accounts."))
    }

    /// Rename / recolour / relabel. Renaming never moves the data folder (keeps sign-ins).
    /// `None` = leave unchanged; `label: Some("")` clears the label.
    pub fn rename_account(
        &mut self,
        id: &str,
        name: Option<&str>,
        color: Option<&str>,
        label: Option<&str>,
    ) -> Result<(), ApiError> {
        let Some(idx) = self.store.accounts.iter().position(|a| a.id == id) else {
            return Err(ApiError::new("not-found", "Account not found."));
        };
        let mut next = self.store.accounts[idx].clone();
        if let Some(n) = name {
            if let Some(err) = validate_name(n) {
                return Err(ApiError::new("invalid", err));
            }
            if let Some(clash) = self.name_taken(n, Some(id)) {
                return Err(name_taken_err(&clash.name));
            }
            next.name = clean_name(n);
        }
        if let Some(c) = color {
            if !color_re(c) {
                return Err(ApiError::new("invalid", COLOR_ERROR));
            }
            next.color = c.to_uppercase();
        }
        if label.is_some() {
            next.label = clean_label(label);
        }
        let prev = std::mem::replace(&mut self.store.accounts[idx], next);
        if let Err(e) = self.save() {
            self.store.accounts[idx] = prev;
            return Err(ApiError::new("io", format!("Could not save accounts: {e}")));
        }
        self.changed();
        Ok(())
    }

    /// Remove an account (never Main), optionally deleting its data folder. Deletes only a
    /// `Claude-*` folder directly under `%APPDATA%`, and refuses while `busy_probe` reports
    /// programs running from it (the app passes `claude_app::processes_under` results). A kept
    /// folder is recorded in `ignoredDataDirs` so discovery does not offer it again. The caller
    /// closes the account's Claude first.
    pub fn remove_account_checked(
        &mut self,
        id: &str,
        delete_data: bool,
        busy_probe: &dyn Fn(&str) -> Vec<(u32, String)>,
    ) -> Result<(), ApiError> {
        if id == "main" {
            return Err(ApiError::new("main", "The Main account cannot be removed."));
        }
        let Some(rec) = self.get(id) else {
            return Err(ApiError::new("not-found", "Account not found."));
        };
        let mut deleted = false;
        if let (true, Some(dir)) = (delete_data, rec.data_dir.as_deref()) {
            if is_deletable_profile_dir(dir, &self.app_data) {
                let busy = busy_probe(dir);
                if !busy.is_empty() {
                    let names: Vec<String> = busy
                        .iter()
                        .take(3)
                        .map(|(_, n)| {
                            let leaf = basename(n);
                            if leaf.is_empty() {
                                "process".to_string()
                            } else {
                                leaf
                            }
                        })
                        .collect();
                    return Err(ApiError::new(
                        "io",
                        format!(
                            "Programs are still running from this account's folder ({}). Close them and try again; nothing was deleted.",
                            names.join(", ")
                        ),
                    ));
                }
                if let Err(e) = remove_dir_retry(Path::new(dir)) {
                    return Err(ApiError::new(
                        "io",
                        format!("Could not delete the profile folder (is Claude still running?): {e}"),
                    ));
                }
                deleted = true;
            } else {
                log::warn(&format!("accounts: refusing to delete non-profile folder {dir}"));
            }
        }
        self.store.accounts.retain(|a| a.id != id);
        // Data kept on disk -> remember it so discovery doesn't offer it again.
        if let (false, Some(dir)) = (deleted, rec.data_dir.as_deref()) {
            if !self.store.ignored_data_dirs.iter().any(|d| paths::same_path(d, dir)) {
                self.store.ignored_data_dirs.push(dir.to_string());
            }
        }
        let saved = self.save();
        self.changed();
        saved.map_err(|e| ApiError::new("io", format!("Could not save accounts: {e}")))
    }

    /// Reorder by id list; unknown ids ignored, unlisted accounts keep relative order at the end.
    pub fn reorder(&mut self, ids: &[String]) {
        let mut rest = std::mem::take(&mut self.store.accounts);
        let mut out = Vec::with_capacity(rest.len());
        for id in ids {
            if let Some(i) = rest.iter().position(|a| &a.id == id) {
                out.push(rest.remove(i));
            }
        }
        out.extend(rest);
        self.store.accounts = out;
        if let Err(e) = self.save() {
            log::error(&format!("accounts: failed to write store: {e}"));
        }
        self.changed();
    }

    pub fn settings(&self) -> Settings {
        self.store.settings.clone()
    }

    /// Merge a validated partial into settings; returns the full settings. Invalid / unknown
    /// keys are ignored (never an error); only a failed write is an error.
    pub fn set_settings(&mut self, patch: serde_json::Value) -> Result<Settings, ApiError> {
        let before = self.store.settings.clone();
        apply_settings(&mut self.store.settings, sanitize_settings(Some(&patch)));
        if self.store.settings != before {
            if let Err(e) = self.save() {
                self.store.settings = before;
                return Err(ApiError::new("io", format!("Could not save settings: {e}")));
            }
            self.changed();
        }
        Ok(self.settings())
    }

    /// Found `%APPDATA%\Claude-*` profile folders that are not accounts (rescanned on every
    /// call). Never changes the store.
    pub fn list_discovered(&mut self, main_account_uuid: Option<&str>) -> Vec<Discovered> {
        let found = self.scan_profiles(&self.store.ignored_data_dirs);
        if found.is_empty() {
            return Vec::new();
        }
        let ids = self.identities(main_account_uuid);
        found
            .into_iter()
            .map(|f| {
                let cfg = read_config(Path::new(&f.data_dir));
                let account_uuid = uuid_from_config(cfg.as_ref());
                let same = account_uuid
                    .as_ref()
                    .and_then(|u| ids.iter().find(|(_, au)| au.as_ref() == Some(u)))
                    .map(|(n, _)| n.clone());
                Discovered {
                    session_count: count_sessions(Path::new(&f.data_dir), account_uuid.as_deref()),
                    signed_in: signed_in_from_config(cfg.as_ref()),
                    same_sign_in_as: same,
                    account_uuid,
                    folder_name: f.folder_name,
                    data_dir: f.data_dir,
                }
            })
            .collect()
    }

    /// Turn a found profile folder into an account (keeps the folder and its sign-in).
    /// Same-sign-in check first (reason `same-sign-in`), then the name (default: folder name
    /// minus "Claude-"; a clash is `name-taken`, never suffixed). The caller syncs MCP next.
    pub fn adopt_profile(
        &mut self,
        data_dir: &str,
        name: Option<&str>,
        color: Option<&str>,
        label: Option<&str>,
        main_account_uuid: Option<&str>,
    ) -> Result<StoredAccount, ApiError> {
        if js_trim(data_dir).is_empty() {
            return Err(ApiError::new("invalid", "Unknown profile folder."));
        }
        // Ignored folders qualify too: adopting one un-ignores it.
        let Some(found) = self.scan_profiles(&[]).into_iter().find(|d| paths::same_path(&d.data_dir, data_dir)) else {
            if self.is_listed(data_dir) {
                return Err(ApiError::new("invalid", "That profile folder is already an account."));
            }
            return Err(ApiError::new("not-found", "That profile folder no longer exists."));
        };
        // One account per claude.ai sign-in.
        if let Some(found_uuid) = uuid_from_config(read_config(Path::new(&found.data_dir)).as_ref()) {
            if let Some((same, _)) =
                self.identities(main_account_uuid).into_iter().find(|(_, u)| u.as_deref() == Some(found_uuid.as_str()))
            {
                return Err(ApiError::new(
                    "same-sign-in",
                    format!("This folder is signed in to the same claude.ai account as {same}."),
                ));
            }
        }
        let clean = match name {
            None => default_name_for(&found.folder_name),
            Some(n) if js_trim(n).is_empty() => default_name_for(&found.folder_name),
            Some(n) => {
                if let Some(err) = validate_name(n) {
                    return Err(ApiError::new("invalid", err));
                }
                clean_name(n)
            }
        };
        if let Some(clash) = self.name_taken(&clean, None) {
            return Err(name_taken_err(&clash.name));
        }
        if let Some(c) = color {
            if !color_re(c) {
                return Err(ApiError::new("invalid", COLOR_ERROR));
            }
        }
        let rec = StoredAccount {
            id: uuid::Uuid::new_v4().to_string(),
            name: clean.clone(),
            data_dir: Some(found.data_dir.clone()),
            color: match color {
                Some(c) => c.to_uppercase(),
                None => next_color(self.store.accounts.iter().map(|a| a.color.as_str())).into(),
            },
            label: clean_label(label),
            order: self.store.accounts.len() as u32,
        };
        let prev_ignored = self.store.ignored_data_dirs.clone();
        self.store.accounts.push(rec);
        self.store.ignored_data_dirs.retain(|d| !paths::same_path(d, &found.data_dir));
        if let Err(e) = self.save() {
            self.store.accounts.pop();
            self.store.ignored_data_dirs = prev_ignored;
            return Err(ApiError::new("io", format!("Could not save accounts: {e}")));
        }
        log::info(&format!("accounts: adopted profile {} as \"{}\"", found.data_dir, clean));
        self.changed();
        self.store.accounts.last().cloned().ok_or_else(|| ApiError::new("io", "Could not save accounts."))
    }

    /// Hide a found profile folder from discovery (persisted in ignoredDataDirs). Idempotent.
    pub fn ignore_profile(&mut self, data_dir: &str) {
        if js_trim(data_dir).is_empty() {
            return;
        }
        if self.store.ignored_data_dirs.iter().any(|d| paths::same_path(d, data_dir)) {
            return;
        }
        self.store.ignored_data_dirs.push(resolve(data_dir).to_string_lossy().into_owned());
        if let Err(e) = self.save() {
            self.store.ignored_data_dirs.pop();
            log::error(&format!("accounts: could not save accounts: {e}"));
            return;
        }
        self.changed();
    }

    /// Copy Main's MCP config (the newest existing of `candidates`) into account `id` when
    /// sharing is on and Main's copy is newer. Ok(true) = copied.
    pub fn sync_mcp_config(&self, id: &str, candidates: &[String]) -> Result<bool, String> {
        let Some(rec) = self.store.accounts.iter().find(|a| a.id == id) else { return Ok(false) };
        let Some(dir) = rec.data_dir.as_deref().filter(|_| rec.id != "main") else { return Ok(false) };
        if !self.store.settings.share_mcp_config {
            return Ok(false);
        }
        let mut newest: Option<(&str, SystemTime)> = None;
        for c in candidates {
            if let Some(m) = mtime(Path::new(c)) {
                if newest.map(|(_, nm)| m > nm).unwrap_or(true) {
                    newest = Some((c.as_str(), m));
                }
            }
        }
        let Some((src, src_m)) = newest else { return Ok(false) };
        let dst = Path::new(dir).join(MCP_FILE);
        if let Some(dm) = mtime(&dst) {
            if dm >= src_m {
                return Ok(false);
            }
        }
        let mut tmp = dst.clone().into_os_string();
        tmp.push(format!(".tmp-{}", std::process::id()));
        let tmp = PathBuf::from(tmp);
        let result = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(dir)?;
            std::fs::copy(src, &tmp)?; // CopyFileExW keeps the source mtime, like fs.copyFileSync
            json::rename_with_retry(&tmp, &dst)
        })();
        match result {
            Ok(()) => {
                log::info(&format!("accounts: synced MCP config {} -> {}", src, dst.display()));
                Ok(true)
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                log::warn(&format!("accounts: MCP sync failed: {e}"));
                Err(e.to_string())
            }
        }
    }

    /// `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value "Sessionmux" = `"<exe>" --hidden`
    /// (enabled) or removed (disabled).
    pub fn start_with_windows(enabled: bool) -> Result<(), String> {
        use crate::util::win::wide;
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
        use windows::Win32::System::Registry::{
            RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ,
        };

        let subkey = wide(r"Software\Microsoft\Windows\CurrentVersion\Run");
        let value_name = wide(brand::RUN_VALUE);
        let mut key = HKEY::default();
        // SAFETY: `subkey` is a NUL-terminated UTF-16 buffer that outlives the call and `key`
        // is a valid out-pointer.
        let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(subkey.as_ptr()), 0, KEY_SET_VALUE, &mut key) };
        if rc != ERROR_SUCCESS {
            return Err(format!("could not open the Run key (error {})", rc.0));
        }
        let result = if enabled {
            let exe = std::env::current_exe().map_err(|e| e.to_string());
            match exe {
                Err(e) => Err(e),
                Ok(exe) => {
                    let cmd = format!("\"{}\" --hidden", exe.display());
                    let data: Vec<u8> = wide(&cmd).iter().flat_map(|u| u.to_le_bytes()).collect();
                    // SAFETY: `key` is an open key handle; `value_name` is NUL-terminated;
                    // `data` is a NUL-terminated REG_SZ byte buffer alive for the call.
                    let rc = unsafe { RegSetValueExW(key, PCWSTR(value_name.as_ptr()), 0, REG_SZ, Some(&data)) };
                    if rc == ERROR_SUCCESS {
                        Ok(())
                    } else {
                        Err(format!("could not write the Run value (error {})", rc.0))
                    }
                }
            }
        } else {
            // SAFETY: `key` is an open key handle and `value_name` is NUL-terminated.
            let rc = unsafe { RegDeleteValueW(key, PCWSTR(value_name.as_ptr())) };
            if rc == ERROR_SUCCESS || rc == ERROR_FILE_NOT_FOUND {
                Ok(())
            } else {
                Err(format!("could not remove the Run value (error {})", rc.0))
            }
        };
        // SAFETY: `key` was opened above and is closed exactly once.
        let _ = unsafe { RegCloseKey(key) };
        result
    }
}

/// Default account name for a found folder: "Claude-work" -> "work", made valid (not uniquified).
fn default_name_for(folder_name: &str) -> String {
    let stripped = if folder_name.len() >= 7 && folder_name[..7].eq_ignore_ascii_case("claude-") {
        &folder_name[7..]
    } else {
        folder_name
    };
    let replaced = dashify(stripped, |c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-'));
    let lead_trimmed = replaced.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
    let sliced = slice_utf16(lead_trimmed, 29);
    let mut base = js_trim(&sliced).to_string();
    if base.is_empty() || validate_name(&base).is_some() {
        base = "Profile".into();
    }
    clean_name(&base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Env {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        user_data: PathBuf,
        app_data: PathBuf,
        local: PathBuf,
        main_data_dir: PathBuf,
        main_config: PathBuf,
    }

    impl Env {
        fn new() -> Env {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().to_path_buf();
            let e = Env {
                user_data: root.join("userData"),
                app_data: root.join("Roaming"),
                local: root.join("Local"),
                main_data_dir: root.join("Local").join("Claude-Data").join("claude_test"),
                main_config: root.join("Roaming").join("Claude").join("claude_desktop_config.json"),
                root,
                _tmp: tmp,
            };
            for d in [&e.user_data, &e.app_data, &e.local, &e.main_data_dir] {
                fs::create_dir_all(d).unwrap();
            }
            e
        }
        fn store(&self) -> AccountsStore {
            AccountsStore::open(&self.user_data, &self.app_data)
        }
        fn main_dir(&self) -> Option<String> {
            Some(self.main_data_dir.to_string_lossy().into_owned())
        }
        /// What app.rs passes: Main's uuid read from its located data dir.
        fn main_uuid(&self) -> Option<String> {
            uuid_from_config(read_config(&self.main_data_dir).as_ref())
        }
        fn candidates(&self) -> Vec<String> {
            vec![
                self.main_config.to_string_lossy().into_owned(),
                self.root.join("nope.json").to_string_lossy().into_owned(),
            ]
        }
        fn list(&self, s: &AccountsStore, procs: &[Proc]) -> Vec<Account> {
            s.list_accounts(procs, self.main_dir().as_deref())
        }
        fn discovered(&self, s: &mut AccountsStore) -> Vec<Discovered> {
            s.list_discovered(self.main_uuid().as_deref())
        }
        fn adopt(
            &self,
            s: &mut AccountsStore,
            dir: &Path,
            name: Option<&str>,
            color: Option<&str>,
        ) -> Result<StoredAccount, ApiError> {
            s.adopt_profile(&dir.to_string_lossy(), name, color, None, self.main_uuid().as_deref())
        }
        fn disk(&self) -> Value {
            serde_json::from_str(&fs::read_to_string(self.user_data.join("sessionmux.json")).unwrap()).unwrap()
        }
    }

    fn mk_profile(dir: &Path, config: Value) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("config.json"), serde_json::to_string(&config).unwrap()).unwrap();
    }

    /// Create `dir` and an empty file `name` inside it.
    fn mk_file(dir: &Path, name: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(name), "{}").unwrap();
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn discovery_marker_rules() {
        let env = Env::new();
        let a = &env.app_data;
        // Each marker on its own makes a candidate.
        mk_file(&a.join("Claude-Cfg"), "config.json");
        mk_file(&a.join("Claude-LocalState"), "Local State");
        mk_file(&a.join("Claude-Prefs"), "Preferences");
        mk_file(&a.join("Claude-3p"), "claude_desktop_config.json");
        fs::create_dir_all(a.join("Claude-Sessions").join("claude-code-sessions")).unwrap();
        // Not candidates: no marker, only Local Storage, a marker name used by the wrong kind.
        fs::create_dir_all(a.join("Claude-Empty")).unwrap();
        fs::create_dir_all(a.join("Claude-LsOnly").join("Local Storage")).unwrap();
        fs::create_dir_all(a.join("Claude-DirCfg").join("config.json")).unwrap();
        mk_file(&a.join("Claude-FileSessions"), "claude-code-sessions");
        mk_file(&a.join("Claude-Other"), "notes.txt");
        let mut st = env.store();
        let names: Vec<String> = env.discovered(&mut st).into_iter().map(|d| d.folder_name).collect();
        assert_eq!(names, ["Claude-3p", "Claude-Cfg", "Claude-LocalState", "Claude-Prefs", "Claude-Sessions"]);
    }

    #[test]
    fn discovery_skips_native_host_folders() {
        let env = Env::new();
        let a = &env.app_data;
        // The real browser-extension native host layout, plus markers that would otherwise qualify.
        let nh = a.join("Claude-NativeHost");
        mk_file(&nh, "com.anthropic.claude_browser_extension.json");
        mk_file(&nh, "config.json");
        fs::create_dir_all(nh.join("1.0.0")).unwrap();
        mk_file(&a.join("Claude-Work-NativeHost"), "Preferences");
        mk_file(&a.join("Claude-x-nativehost"), "config.json");
        mk_file(&a.join("Claude-NativeHostess"), "config.json");
        mk_file(&a.join("Claude-Work"), "config.json");
        assert!(!is_discoverable_leaf("Claude-NativeHost"));
        assert!(!is_discoverable_leaf("Claude-Foo-NATIVEHOST"));
        assert!(is_discoverable_leaf("Claude-NativeHostess"));
        assert!(!is_discoverable_leaf("Claude-"));
        let mut st = env.store();
        let names: Vec<String> = env.discovered(&mut st).into_iter().map(|d| d.folder_name).collect();
        assert_eq!(names, ["Claude-NativeHostess", "Claude-Work"]);
    }

    #[test]
    fn session_feature_settings() {
        let env = Env::new();
        let mut st = env.store();
        let s = st.settings();
        assert!(!s.auto_handover && !s.approve_from_panel);
        // Off is the default and is not written, so stores from older builds keep their bytes.
        assert!(env.disk()["settings"].get("autoHandover").is_none());
        assert!(env.disk()["settings"].get("approveFromPanel").is_none());
        for bad in [json!("yes"), json!(1), json!(null), json!([])] {
            let s = st.set_settings(json!({ "autoHandover": bad.clone(), "approveFromPanel": bad })).unwrap();
            assert!(!s.auto_handover && !s.approve_from_panel);
        }
        let s = st.set_settings(json!({ "autoHandover": true, "approveFromPanel": true })).unwrap();
        assert!(s.auto_handover && s.approve_from_panel);
        assert_eq!(env.disk()["settings"]["autoHandover"], json!(true));
        assert_eq!(env.disk()["settings"]["approveFromPanel"], json!(true));
        let again = env.store().settings();
        assert!(again.auto_handover && again.approve_from_panel);
        let s = st.set_settings(json!({ "autoHandover": false })).unwrap();
        assert!(!s.auto_handover && s.approve_from_panel);
        assert!(env.disk()["settings"].get("autoHandover").is_none());
    }

    #[test]
    fn sign_in_routing_setting() {
        let env = Env::new();
        let mut st = env.store();
        assert!(st.settings().sign_in_routing);
        assert_eq!(env.disk()["settings"]["signInRouting"], json!(true));
        // Non-booleans are ignored exactly like the other boolean settings.
        for bad in [json!("no"), json!(0), json!(null), json!({})] {
            assert!(st.set_settings(json!({ "signInRouting": bad })).unwrap().sign_in_routing);
        }
        assert!(!st.set_settings(json!({"signInRouting": false})).unwrap().sign_in_routing);
        assert_eq!(env.disk()["settings"]["signInRouting"], json!(false));
        assert!(!env.store().settings().sign_in_routing);
        // A store written before the setting existed reads it as true (normalise + plain serde).
        let mut v = env.disk();
        v["settings"].as_object_mut().unwrap().remove("signInRouting");
        fs::write(env.user_data.join("sessionmux.json"), serde_json::to_string(&v).unwrap()).unwrap();
        assert!(env.store().settings().sign_in_routing);
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        old.as_object_mut().unwrap().remove("signInRouting");
        assert!(serde_json::from_value::<Settings>(old).unwrap().sign_in_routing);
    }

    /// A settings key older builds wrote; stores that still carry it must load and drop it.
    const RETIRED_KEY: &str = "tamperGesture";

    #[test]
    fn retired_settings_key_is_ignored() {
        let env = Env::new();
        let mut v = serde_json::to_value(Store::default()).unwrap();
        v["settings"][RETIRED_KEY] = json!(false);
        fs::write(env.user_data.join("sessionmux.json"), serde_json::to_string(&v).unwrap()).unwrap();
        let mut st = env.store();
        assert_eq!(st.settings(), Settings::default());
        assert!(env.disk()["settings"].get(RETIRED_KEY).is_none());
        // The settings patch whitelist drops it too.
        let mut patch = json!({});
        patch[RETIRED_KEY] = json!(false);
        st.set_settings(patch).unwrap();
        assert!(env.disk()["settings"].get(RETIRED_KEY).is_none());
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        old[RETIRED_KEY] = json!(true);
        assert_eq!(serde_json::from_value::<Settings>(old).unwrap(), Settings::default());
    }

    fn proc_(pid: u32, data_dir: Option<String>) -> Proc {
        Proc { pid, exe: String::new(), cmdline: String::new(), data_dir, hwnd: 0 }
    }

    fn keys(v: &Value) -> Vec<String> {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    }

    fn sorted(v: &[&str]) -> Vec<String> {
        let mut k: Vec<String> = v.iter().map(|s| s.to_string()).collect();
        k.sort();
        k
    }

    const ACCOUNT_KEYS: [&str; 14] = [
        "id",
        "name",
        "dataDir",
        "color",
        "initials",
        "label",
        "isMain",
        "signedIn",
        "accountUuid",
        "running",
        "pid",
        "sessionCount",
        "codeReady",
        "sameSignInAs",
    ];
    const DISCOVERED_KEYS: [&str; 6] =
        ["dataDir", "folderName", "accountUuid", "signedIn", "sameSignInAs", "sessionCount"];

    fn set_mtime(p: &Path, offset_secs: u64) {
        let t = SystemTime::now() + std::time::Duration::from_secs(offset_secs);
        fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
    }

    #[test]
    fn initials_rules() {
        assert_eq!(initials("Design"), "DE");
        assert_eq!(initials("work account"), "WA");
        assert_eq!(initials("a"), "A");
        assert_eq!(initials("my.side-proj"), "MS");
        assert_eq!(initials("  "), "?");
        assert_eq!(initials("!!"), "?");
    }

    #[test]
    fn name_helpers() {
        assert_eq!(name_key("  My \t  Work "), "my work");
        assert_eq!(sanitize_name("My Work/x"), "My-Work-x");
        assert_eq!(default_name_for("Claude-work"), "work");
        assert_eq!(default_name_for("Claude-SIDE project"), "SIDE project");
        assert_eq!(default_name_for("claude-__x"), "x");
        assert_eq!(default_name_for("Claude-!!!"), "Profile");
        assert!(validate_name("x".repeat(32).as_str()).is_none());
        assert!(validate_name("x".repeat(33).as_str()).is_some());
    }

    #[test]
    fn fresh_store_main_only_default_settings() {
        let env = Env::new();
        let s = env.store();
        let list = env.list(&s, &[]);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "main");
        assert_eq!(list[0].data_dir, None);
        assert!(list[0].is_main);
        assert_eq!(s.settings(), Settings::default());
        assert_eq!(s.settings().theme, "dark");
        let disk = env.disk();
        assert_eq!(disk["version"], 2);
        assert_eq!(disk["accounts"][0]["id"], "main");
    }

    #[test]
    fn fresh_store_lists_main_and_discovers_profiles() {
        let env = Env::new();
        let design = env.app_data.join("Claude-Design");
        mk_profile(&design, json!({"lastKnownAccountUuid": "acct-design", "oauth:tokenCache": "x"}));
        mk_file(&env.app_data.join("Claude-Work"), "Local State");
        mk_profile(&env.app_data.join("Claude-NativeHost"), json!({}));
        fs::create_dir_all(env.app_data.join("Claude-Empty")).unwrap();
        mk_profile(&env.app_data.join("Claude"), json!({}));
        mk_profile(&env.main_data_dir, json!({"lastKnownAccountUuid": "acct-main"}));

        let mut st = env.store();
        assert_eq!(env.list(&st, &[]).iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["Main"]);
        let mut found: Vec<String> = env.discovered(&mut st).iter().map(|d| d.folder_name.clone()).collect();
        found.sort();
        assert_eq!(found, ["Claude-Design", "Claude-Work"]);
        env.adopt(&mut st, &design, Some("Design"), None).unwrap();
        let list = env.list(&st, &[]);
        assert_eq!(list.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["Main", "Design"]);
        assert_eq!(env.discovered(&mut st).iter().map(|d| d.folder_name.as_str()).collect::<Vec<_>>(), ["Claude-Work"]);
        for a in &list {
            assert_eq!(keys(&serde_json::to_value(a).unwrap()), sorted(&ACCOUNT_KEYS));
        }
        let (main, d) = (&list[0], &list[1]);
        assert_eq!(main.id, "main");
        assert_eq!(main.account_uuid.as_deref(), Some("acct-main"));
        assert!(!main.signed_in);
        assert_eq!(d.account_uuid.as_deref(), Some("acct-design"));
        assert!(d.signed_in);
        assert_eq!(d.initials, "DE");
        assert_eq!(list.iter().map(|a| a.color.as_str()).collect::<Vec<_>>(), PALETTE[..2]);
        let ids: Vec<String> = list.iter().map(|a| a.id.clone()).collect();
        let s2 = env.store();
        assert_eq!(env.list(&s2, &[]).iter().map(|a| a.id.clone()).collect::<Vec<_>>(), ids);
    }

    #[test]
    fn live_state_running_pid_and_session_count() {
        let env = Env::new();
        let design = env.app_data.join("Claude-Design");
        mk_profile(&design, json!({}));
        let org = design.join("claude-code-sessions").join("acct").join("org");
        fs::create_dir_all(&org).unwrap();
        fs::write(org.join("local_1.json"), "{}").unwrap();
        fs::write(org.join("local_2.json"), "{}").unwrap();
        fs::write(org.join("scheduled-tasks.json"), "{}").unwrap();
        let main_org = env.main_data_dir.join("claude-code-sessions").join("a").join("o");
        fs::create_dir_all(&main_org).unwrap();
        fs::write(main_org.join("local_x.json"), "{}").unwrap();
        let procs = vec![proc_(100, None), proc_(200, Some(s(&design).to_uppercase() + "\\"))];
        let mut st = env.store();
        let d_rec = env.adopt(&mut st, &design, None, None).unwrap();
        let list = env.list(&st, &procs);
        let (main, d) = (&list[0], &list[1]);
        assert!(main.running);
        assert_eq!(main.pid, Some(100));
        assert_eq!(main.session_count, 1);
        assert!(d.running);
        assert_eq!(d.pid, Some(200));
        assert_eq!(d.session_count, 2);
        let again = env.list(&st, &[]);
        assert!(!again[1].running);
        assert_eq!(again[1].pid, None);
        assert_eq!(st.effective_data_dir(&d_rec.id, env.main_dir().as_deref()), Some(s(&design)));
        assert_eq!(st.effective_data_dir("main", env.main_dir().as_deref()), env.main_dir());
        assert_eq!(st.get("nope"), None);
        assert_eq!(st.effective_data_dir("nope", None), None);
    }

    #[test]
    fn session_count_follows_current_uuid_and_code_ready() {
        let env = Env::new();
        let design = env.app_data.join("Claude-Design");
        mk_profile(&design, json!({"lastKnownAccountUuid": "cur"}));
        let cur = design.join("claude-code-sessions").join("cur").join("org");
        let old = design.join("claude-code-sessions").join("old").join("org");
        fs::create_dir_all(&cur).unwrap();
        fs::create_dir_all(&old).unwrap();
        fs::write(cur.join("local_1.json"), "{}").unwrap();
        for n in ["a", "b", "c"] {
            fs::write(old.join(format!("local_{n}.json")), "{}").unwrap();
        }
        let fresh = env.app_data.join("Claude-Fresh");
        mk_profile(&fresh, json!({"lastKnownAccountUuid": "f"}));
        fs::create_dir_all(fresh.join("claude-code-sessions").join("f")).unwrap();
        let mut st = env.store();
        env.adopt(&mut st, &design, None, None).unwrap();
        env.adopt(&mut st, &fresh, None, None).unwrap();
        let list = env.list(&st, &[]);
        let d = list.iter().find(|a| a.name == "Design").unwrap();
        let f = list.iter().find(|a| a.name == "Fresh").unwrap();
        assert_eq!(d.session_count, 1);
        assert!(d.code_ready);
        assert_eq!(f.session_count, 0);
        assert!(!f.code_ready);
        assert!(!list.iter().find(|a| a.id == "main").unwrap().code_ready);
        // Unknown uuid -> every uuid folder counts.
        assert_eq!(count_sessions(&design, None), 4);
    }

    #[test]
    fn remove_refuses_to_delete_a_folder_programs_run_from() {
        let env = Env::new();
        let busy = std::cell::RefCell::new(vec![(9u32, "claude.exe".to_string())]);
        let probe = |_: &str| busy.borrow().clone();
        let mut st = env.store();
        let a = st.add_account("Busy", None, None).unwrap();
        let dir = PathBuf::from(a.data_dir.clone().unwrap());
        mk_profile(&dir, json!({}));
        let r = st.remove_account_checked(&a.id, true, &probe).unwrap_err();
        assert!(r.error.contains("still running"), "{}", r.error);
        assert!(r.error.contains("claude.exe"));
        assert!(dir.exists());
        assert!(env.list(&st, &[]).iter().any(|x| x.id == a.id));
        busy.borrow_mut().clear();
        st.remove_account_checked(&a.id, true, &probe).unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn add_account_rules_folder_naming_mcp_sync_change_events() {
        let env = Env::new();
        fs::create_dir_all(env.main_config.parent().unwrap()).unwrap();
        fs::write(&env.main_config, r#"{"mcpServers":{"a":{}}}"#).unwrap();
        fs::create_dir_all(env.app_data.join("Claude-My-Work")).unwrap();
        let mut st = env.store();
        let c0 = st.change_count();
        for bad in ["", "   ", "-lead", &"x".repeat(33), "bad/slash", "emoji\u{1F600}"] {
            let r = st.add_account(bad, None, None);
            assert!(r.is_err(), "should reject {bad:?}");
            assert!(!r.unwrap_err().error.is_empty());
        }
        assert!(st.add_account("main", None, None).is_err());
        assert!(st.add_account("Ok", Some("red"), None).is_err());

        let r = st.add_account("My Work", None, Some("me@example.com")).unwrap();
        // app.rs syncs MCP right after add.
        assert_eq!(st.sync_mcp_config(&r.id, &env.candidates()), Ok(true));
        let dd = r.data_dir.clone().unwrap();
        assert_eq!(dd, s(&env.app_data.join("Claude-My-Work-2")));
        assert_eq!(r.label.as_deref(), Some("me@example.com"));
        let acc = env.list(&st, &[]).into_iter().find(|a| a.id == r.id).unwrap();
        assert_eq!(acc.initials, "MW");
        assert!(Path::new(&dd).exists());
        let copied: Value = json::read(&Path::new(&dd).join("claude_desktop_config.json")).unwrap();
        assert_eq!(copied, json!({"mcpServers": {"a": {}}}));
        assert!(st.add_account("MY WORK", None, None).is_err());
        assert_eq!(st.change_count() - c0, 1);
    }

    #[test]
    fn sync_mcp_only_copies_when_main_newer_and_sharing_on() {
        let env = Env::new();
        fs::create_dir_all(env.main_config.parent().unwrap()).unwrap();
        fs::write(&env.main_config, r#"{"v":1}"#).unwrap();
        let mut st = env.store();
        let a = st.add_account("A", None, None).unwrap();
        assert_eq!(st.sync_mcp_config(&a.id, &env.candidates()), Ok(true));
        let dst = Path::new(a.data_dir.as_deref().unwrap()).join("claude_desktop_config.json");
        assert_eq!(fs::read_to_string(&dst).unwrap(), r#"{"v":1}"#);
        // Account's copy is newer -> untouched.
        fs::write(&dst, r#"{"local":true}"#).unwrap();
        set_mtime(&dst, 60);
        assert_eq!(st.sync_mcp_config(&a.id, &env.candidates()), Ok(false));
        // Main newer -> copied.
        fs::write(&env.main_config, r#"{"v":2}"#).unwrap();
        set_mtime(&env.main_config, 120);
        assert_eq!(st.sync_mcp_config(&a.id, &env.candidates()), Ok(true));
        assert_eq!(fs::read_to_string(&dst).unwrap(), r#"{"v":2}"#);
        // Disabled -> no copy; Main -> never.
        st.set_settings(json!({"shareMcpConfig": false})).unwrap();
        set_mtime(&env.main_config, 180);
        assert_eq!(st.sync_mcp_config(&a.id, &env.candidates()), Ok(false));
        assert_eq!(st.sync_mcp_config("main", &env.candidates()), Ok(false));
        // No candidate exists -> nothing to copy.
        st.set_settings(json!({"shareMcpConfig": true})).unwrap();
        assert_eq!(st.sync_mcp_config(&a.id, &[s(&env.root.join("missing.json"))]), Ok(false));
    }

    #[test]
    fn rename_account_rules() {
        let env = Env::new();
        let mut st = env.store();
        let a = st.add_account("Alpha", None, None).unwrap();
        st.add_account("Beta", None, None).unwrap();
        assert!(st.rename_account(&a.id, Some("beta"), None, None).is_err());
        assert!(st.rename_account(&a.id, Some("!!"), None, None).is_err());
        assert!(st.rename_account(&a.id, None, Some("#12345"), None).is_err());
        assert_eq!(st.rename_account("missing", Some("X"), None, None).unwrap_err().reason, "not-found");
        st.rename_account(&a.id, Some("ALPHA"), None, None).unwrap();
        st.rename_account(&a.id, Some("Gamma"), Some("#abcdef"), Some(" g@x ")).unwrap();
        st.rename_account("main", Some("Home"), None, None).unwrap();
        let list = env.list(&st, &[]);
        let g = list.iter().find(|x| x.id == a.id).unwrap();
        assert_eq!(g.name, "Gamma");
        assert_eq!(g.color, "#ABCDEF");
        assert_eq!(g.label.as_deref(), Some("g@x"));
        assert_eq!(g.data_dir, a.data_dir);
        assert_eq!(list[0].name, "Home");
        st.rename_account(&a.id, None, None, Some("")).unwrap(); // clears the label
        assert_eq!(st.get(&a.id).unwrap().label, None);
    }

    #[test]
    fn remove_account_main_refused_deletes_only_claude_dirs_no_rediscovery() {
        let env = Env::new();
        let idle = |_: &str| Vec::new();
        let mut st = env.store();
        assert_eq!(st.remove_account_checked("main", true, &idle).unwrap_err().reason, "main");
        assert_eq!(st.remove_account_checked("nope", true, &idle).unwrap_err().reason, "not-found");

        let a = st.add_account("Gone", None, None).unwrap();
        let ad = PathBuf::from(a.data_dir.clone().unwrap());
        mk_profile(&ad, json!({}));
        st.remove_account_checked(&a.id, true, &idle).unwrap();
        assert!(!ad.exists());

        // Keep data -> folder stays and is not re-discovered on next load.
        let k = st.add_account("Keep", None, None).unwrap();
        let kd = PathBuf::from(k.data_dir.clone().unwrap());
        mk_profile(&kd, json!({}));
        st.remove_account_checked(&k.id, false, &idle).unwrap();
        assert!(kd.exists());
        let mut s2 = env.store();
        assert_eq!(env.list(&s2, &[]).iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), ["main"]);
        assert!(env.discovered(&mut s2).is_empty());

        // A folder outside %APPDATA%\Claude-* is never deleted.
        let outside = env.root.join("elsewhere").join("Claude-X");
        mk_profile(&outside, json!({}));
        fs::write(
            env.user_data.join("sessionmux.json"),
            json!({"version": 2, "accounts": [{"id": "main", "name": "Main", "dataDir": null}, {"id": "x", "name": "X", "dataDir": s(&outside)}], "settings": {}}).to_string(),
        )
        .unwrap();
        let mut s3 = env.store();
        s3.remove_account_checked("x", true, &idle).unwrap();
        assert!(outside.exists());
    }

    #[test]
    fn reorder_and_settings_validation_persistence() {
        let env = Env::new();
        let mut st = env.store();
        let a = st.add_account("A", None, None).unwrap();
        let b = st.add_account("B", None, None).unwrap();
        st.reorder(&[b.id.clone(), "bogus".into(), "main".into()]);
        let ids = |st: &AccountsStore| env.list(st, &[]).iter().map(|x| x.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&st), [b.id.clone(), "main".into(), a.id.clone()]);

        assert_eq!(st.settings().bubble_mode, "icon");
        assert_eq!(st.set_settings(json!({"bubbleMode": "tray"})).unwrap().bubble_mode, "tray");
        assert_eq!(st.set_settings(json!({"bubbleMode": "dock"})).unwrap().bubble_mode, "tray");
        assert_eq!(st.set_settings(json!({"bubbleMode": null})).unwrap().bubble_mode, "tray");
        let out = st
            .set_settings(json!({"theme": "light", "showBubble": false, "bubblePosition": {"x": 10.4, "y": 20}, "theme2": "x", "panelSide": "nowhere", "startWithWindows": "yes"}))
            .unwrap();
        assert_eq!(out.theme, "light");
        assert!(!out.show_bubble);
        assert_eq!(out.bubble_position, Some(Point { x: 10, y: 20 }));
        assert_eq!(out.panel_side, "auto");
        assert!(!out.start_with_windows);
        assert!(!keys(&serde_json::to_value(&out).unwrap()).contains(&"theme2".to_string()));
        // More validation: hotkey trimmed/bounded, position null, panelSide valid, non-object patch.
        assert_eq!(st.set_settings(json!({"hotkey": "  Ctrl+Alt+O "})).unwrap().hotkey, "Ctrl+Alt+O");
        assert_eq!(st.set_settings(json!({"hotkey": "x".repeat(65)})).unwrap().hotkey, "Ctrl+Alt+O");
        assert_eq!(st.set_settings(json!({"hotkey": "   "})).unwrap().hotkey, "Ctrl+Alt+O");
        assert_eq!(
            st.set_settings(json!({"bubblePosition": {"x": -2.5, "y": "1"}})).unwrap().bubble_position,
            Some(Point { x: 10, y: 20 })
        );
        assert_eq!(
            st.set_settings(json!({"bubblePosition": {"x": -2.5, "y": 7.5}})).unwrap().bubble_position,
            Some(Point { x: -2, y: 8 })
        );
        assert_eq!(st.set_settings(json!({"panelSide": "left", "theme": "system"})).unwrap().panel_side, "left");
        assert_eq!(st.set_settings(json!("garbage")).unwrap().theme, "system");
        st.set_settings(json!({"bubblePosition": {"x": 10.4, "y": 20}, "theme": "light"})).unwrap();

        let s2 = env.store();
        assert_eq!(s2.settings().theme, "light");
        assert_eq!(s2.settings().bubble_mode, "tray");
        assert_eq!(ids(&s2), [b.id.clone(), "main".into(), a.id.clone()]);
        // Unchanged patch -> no change event.
        let c = st.change_count();
        st.set_settings(json!({"theme": "light"})).unwrap();
        assert_eq!(st.change_count(), c);
    }

    #[test]
    fn discovery_never_auto_adds() {
        let env = Env::new();
        mk_profile(&env.main_data_dir, json!({"lastKnownAccountUuid": "acct-main"}));
        let work = env.app_data.join("Claude-work");
        mk_profile(&work, json!({"lastKnownAccountUuid": "acct-main", "oauth:tokenCache": "x"}));
        let org = work.join("claude-code-sessions").join("acct-main").join("org");
        fs::create_dir_all(&org).unwrap();
        fs::write(org.join("local_1.json"), "{}").unwrap();
        mk_file(&env.app_data.join("Claude-New"), "Local State");
        mk_profile(&env.app_data.join("Claude-NativeHost"), json!({}));
        fs::create_dir_all(env.app_data.join("Claude-Empty")).unwrap();

        let mut st = env.store();
        assert_eq!(env.list(&st, &[]).iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["main"]);
        assert_eq!(env.disk()["accounts"].as_array().unwrap().len(), 1);

        let found = env.discovered(&mut st);
        assert_eq!(found.iter().map(|d| d.folder_name.as_str()).collect::<Vec<_>>(), ["Claude-New", "Claude-work"]);
        for d in &found {
            assert_eq!(keys(&serde_json::to_value(d).unwrap()), sorted(&DISCOVERED_KEYS));
        }
        let (n, w) = (&found[0], &found[1]);
        assert_eq!(w.data_dir, s(&work));
        assert_eq!(w.account_uuid.as_deref(), Some("acct-main"));
        assert!(w.signed_in);
        assert_eq!(w.same_sign_in_as.as_deref(), Some("Main"));
        assert_eq!(w.session_count, 1);
        assert_eq!(n.account_uuid, None);
        assert!(!n.signed_in);
        assert_eq!(n.same_sign_in_as, None);
        assert_eq!(n.session_count, 0);

        mk_profile(&env.app_data.join("Claude-Later"), json!({}));
        assert_eq!(env.discovered(&mut st).len(), 3);
        let s2 = env.store();
        assert_eq!(env.list(&s2, &[]).iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["main"]);
    }

    #[test]
    fn adopt_profile_adds_found_folder_validation_unignores() {
        let env = Env::new();
        let work = env.app_data.join("Claude-work");
        mk_profile(&work, json!({"lastKnownAccountUuid": "acct-w", "oauth:tokenCache": "x"}));
        let other = env.app_data.join("Claude-other");
        mk_profile(&other, json!({}));
        let mut st = env.store();
        let c0 = st.change_count();

        assert_eq!(
            env.adopt(&mut st, &env.app_data.join("Claude-missing"), None, None).unwrap_err().reason,
            "not-found"
        );
        assert!(st.adopt_profile("", None, None, None, None).is_err());
        assert!(env.adopt(&mut st, &work, Some("bad/name"), None).is_err());
        assert!(env.adopt(&mut st, &work, Some("MAIN"), None).is_err());
        assert!(env.adopt(&mut st, &work, None, Some("red")).is_err());
        assert_eq!(st.change_count(), c0);

        let r = st.adopt_profile(&s(&work).to_uppercase(), None, None, None, env.main_uuid().as_deref()).unwrap();
        assert_eq!(r.name, "work");
        assert_eq!(r.data_dir, Some(s(&work)));
        let acc = env.list(&st, &[]).into_iter().find(|a| a.id == r.id).unwrap();
        assert!(acc.signed_in);
        assert_eq!(acc.account_uuid.as_deref(), Some("acct-w"));
        assert_eq!(keys(&serde_json::to_value(&acc).unwrap()), sorted(&ACCOUNT_KEYS));
        assert_eq!(st.change_count() - c0, 1);
        assert!(env.adopt(&mut st, &work, None, None).is_err()); // already an account
        assert_eq!(
            env.discovered(&mut st).iter().map(|d| d.folder_name.as_str()).collect::<Vec<_>>(),
            ["Claude-other"]
        );

        st.ignore_profile(&s(&other));
        let r2 = env.adopt(&mut st, &other, Some("Side"), Some("#abcdef")).unwrap();
        assert_eq!(r2.name, "Side");
        assert_eq!(r2.color, "#ABCDEF");
        let disk = env.disk();
        assert_eq!(disk["ignoredDataDirs"], json!([]));
        assert_eq!(
            disk["accounts"].as_array().unwrap().iter().map(|a| a["name"].as_str().unwrap()).collect::<Vec<_>>(),
            ["Main", "work", "Side"]
        );
    }

    #[test]
    fn duplicate_names_refused_on_add_rename_adopt() {
        let env = Env::new();
        let mut st = env.store();
        let work = st.add_account("My Work", None, None).unwrap();
        assert_eq!(work.name, "My Work");
        let c0 = st.change_count();
        for v in ["My Work", "my work", "MY WORK", "  my work  ", "my   work", "My\tWork"] {
            let e = st.add_account(v, None, None).unwrap_err();
            assert_eq!(e.reason, "name-taken", "add should reject {v:?}");
            assert_eq!(e.error, "An account named \"My Work\" already exists.");
        }
        let m = st.add_account(" MAIN ", None, None).unwrap_err();
        assert_eq!(m.reason, "name-taken");
        assert_eq!(m.error, "An account named \"Main\" already exists.");
        assert_eq!(st.change_count(), c0);
        let spaced = st.add_account("  Side   Project ", None, None).unwrap();
        assert_eq!(spaced.name, "Side Project");
        assert_eq!(st.add_account("side project", None, None).unwrap_err().reason, "name-taken");
        let c1 = st.change_count();

        let r1 = st.rename_account(&spaced.id, Some("my  WORK"), None, None).unwrap_err();
        assert_eq!(r1.reason, "name-taken");
        assert_eq!(r1.error, "An account named \"My Work\" already exists.");
        assert_eq!(st.rename_account("main", Some("my work"), None, None).unwrap_err().reason, "name-taken");
        assert_eq!(st.change_count(), c1);
        st.rename_account(&work.id, Some(" my   work "), None, None).unwrap();
        assert_eq!(st.get(&work.id).unwrap().name, "my work");

        let found = env.app_data.join("Claude-SIDE project");
        mk_profile(&found, json!({}));
        let a1 = env.adopt(&mut st, &found, None, None).unwrap_err();
        assert_eq!(a1.reason, "name-taken");
        assert_eq!(a1.error, "An account named \"Side Project\" already exists.");
        let a2 = env.adopt(&mut st, &found, Some("MY WORK"), None).unwrap_err();
        assert_eq!(a2.reason, "name-taken");
        assert_eq!(a2.error, "An account named \"my work\" already exists.");
        assert_eq!(env.adopt(&mut st, &found, Some("  side  PROJECT"), None).unwrap_err().reason, "name-taken");
        assert_eq!(env.discovered(&mut st).iter().map(|d| d.data_dir.clone()).collect::<Vec<_>>(), [s(&found)]);
        let a3 = env.adopt(&mut st, &found, Some("Third"), None).unwrap();
        assert_eq!(a3.name, "Third");
        assert_eq!(
            env.list(&st, &[]).iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["Main", "my work", "Side Project", "Third"]
        );
    }

    #[test]
    fn adopt_refuses_same_sign_in() {
        let env = Env::new();
        mk_profile(&env.main_data_dir, json!({"lastKnownAccountUuid": "u-main"}));
        let design = env.app_data.join("Claude-Design");
        mk_profile(&design, json!({"lastKnownAccountUuid": "u-design", "oauth:tokenCache": "x"}));
        let copy_of_main = env.app_data.join("Claude-again");
        mk_profile(&copy_of_main, json!({"lastKnownAccountUuid": "u-main", "oauth:tokenCache": "x"}));
        let copy_of_design = env.app_data.join("Claude-design2");
        mk_profile(&copy_of_design, json!({"lastKnownAccountUuid": "u-design"}));
        let fresh = env.app_data.join("Claude-fresh");
        mk_profile(&fresh, json!({}));
        let mut st = env.store();
        env.adopt(&mut st, &design, Some("Design"), None).unwrap();
        let c0 = st.change_count();

        let r1 = env.adopt(&mut st, &copy_of_main, None, None).unwrap_err();
        assert_eq!(r1.reason, "same-sign-in");
        assert_eq!(r1.error, "This folder is signed in to the same claude.ai account as Main.");
        let r2 = env.adopt(&mut st, &copy_of_design, Some("Unique"), None).unwrap_err();
        assert_eq!(r2.reason, "same-sign-in");
        assert_eq!(r2.error, "This folder is signed in to the same claude.ai account as Design.");
        assert_eq!(st.change_count(), c0);
        let found = env.discovered(&mut st);
        assert_eq!(
            found.iter().map(|x| (x.folder_name.as_str(), x.same_sign_in_as.as_deref())).collect::<Vec<_>>(),
            [("Claude-again", Some("Main")), ("Claude-design2", Some("Design")), ("Claude-fresh", None)]
        );
        env.adopt(&mut st, &fresh, None, None).unwrap();
        st.rename_account("main", Some("Home"), None, None).unwrap();
        assert_eq!(
            env.adopt(&mut st, &copy_of_main, None, None).unwrap_err().error,
            "This folder is signed in to the same claude.ai account as Home."
        );
    }

    #[test]
    fn ignore_profile_hides_and_persists() {
        let env = Env::new();
        let work = env.app_data.join("Claude-work");
        mk_profile(&work, json!({}));
        let mut st = env.store();
        assert_eq!(env.discovered(&mut st).len(), 1);
        let c0 = st.change_count();
        st.ignore_profile("");
        assert_eq!(st.change_count(), c0);
        st.ignore_profile(&s(&work));
        assert_eq!(st.change_count(), c0 + 1);
        assert!(env.discovered(&mut st).is_empty());
        st.ignore_profile(&s(&work)); // idempotent
        assert_eq!(st.change_count(), c0 + 1);
        assert_eq!(st.snapshot().ignored_data_dirs.len(), 1);
        let mut s2 = env.store();
        assert!(env.discovered(&mut s2).is_empty());
        assert_eq!(env.list(&s2, &[]).iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["main"]);
        assert!(work.exists());
    }

    #[test]
    fn same_sign_in_as_points_at_first() {
        let env = Env::new();
        mk_profile(&env.main_data_dir, json!({"lastKnownAccountUuid": "u-main"}));
        let design = env.app_data.join("Claude-Design");
        let work = env.app_data.join("Claude-work");
        let solo = env.app_data.join("Claude-solo");
        mk_profile(&design, json!({"lastKnownAccountUuid": "u-design", "oauth:tokenCache": "x"}));
        mk_profile(&work, json!({"lastKnownAccountUuid": "u-design", "oauth:tokenCache": "x"}));
        mk_profile(&solo, json!({}));
        fs::write(
            env.user_data.join("sessionmux.json"),
            json!({"version": 2, "accounts": [
                {"id": "main", "name": "Main", "dataDir": null, "order": 0},
                {"id": "d", "name": "Design", "dataDir": s(&design), "order": 1},
                {"id": "w", "name": "work", "dataDir": s(&work), "order": 2},
                {"id": "s", "name": "solo", "dataDir": s(&solo), "order": 3}], "settings": {}})
            .to_string(),
        )
        .unwrap();
        let mut st = env.store();
        let list = env.list(&st, &[]);
        assert_eq!(list.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["main", "d", "w", "s"]);
        assert_eq!(
            list.iter().map(|a| a.same_sign_in_as.as_deref()).collect::<Vec<_>>(),
            [None, None, Some("Design"), None]
        );
        st.reorder(&["w".into(), "main".into(), "d".into(), "s".into()]);
        let list = env.list(&st, &[]);
        assert_eq!(
            list.iter().map(|a| (a.id.as_str(), a.same_sign_in_as.as_deref())).collect::<Vec<_>>(),
            [("w", None), ("main", None), ("d", Some("work")), ("s", None)]
        );
    }

    #[test]
    fn corrupt_store_is_backed_up_and_rebuilt() {
        let env = Env::new();
        fs::write(env.user_data.join("sessionmux.json"), "{not json").unwrap();
        let st = env.store();
        assert_eq!(env.list(&st, &[]).iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), ["main"]);
        assert!(fs::read_dir(&env.user_data)
            .unwrap()
            .flatten()
            .any(|f| f.file_name().to_string_lossy().starts_with("sessionmux.json.corrupt-")));
    }

    #[test]
    fn normalise_drops_unknown_keys_dedupes_and_assigns_colours() {
        let env = Env::new();
        fs::write(
            env.user_data.join("sessionmux.json"),
            json!({"version": 1, "extra": true, "accounts": [
                {"id": "a", "name": "  A  ", "dataDir": " C:\\x\\Claude-A ", "color": "#abcdef", "order": 2, "junk": 1},
                {"id": "b", "name": "", "dataDir": "c:\\X\\claude-a\\", "order": 3},
                {"id": "c", "dataDir": "C:\\x\\Claude-C", "color": "nope", "label": "  l  ", "order": 1.5},
                {"id": "d", "name": "no dir"},
                "garbage"
            ], "settings": {"theme": "light", "bogus": 1}, "ignoredDataDirs": ["C:\\i", 5]})
            .to_string(),
        )
        .unwrap();
        let st = env.store();
        let disk = env.disk();
        assert_eq!(keys(&disk), sorted(&["version", "accounts", "settings", "ignoredDataDirs"]));
        assert_eq!(disk["version"], 2);
        let accs = disk["accounts"].as_array().unwrap();
        assert_eq!(accs.iter().map(|a| a["id"].as_str().unwrap()).collect::<Vec<_>>(), ["main", "c", "a"]);
        for a in accs {
            assert_eq!(keys(a), sorted(&["id", "name", "dataDir", "color", "label", "order"]));
        }
        assert_eq!(accs[1]["name"], "Claude-C");
        assert_eq!(accs[1]["label"], "l");
        assert_eq!(accs[2]["name"], "A");
        assert_eq!(accs[2]["dataDir"], "C:\\x\\Claude-A");
        assert_eq!(accs[2]["color"], "#ABCDEF");
        assert_eq!(accs.iter().map(|a| a["order"].as_u64().unwrap()).collect::<Vec<_>>(), [0, 1, 2]);
        // Main gets the least-used colour first (palette[0]), then C gets palette[1].
        assert_eq!(accs[0]["color"], PALETTE[0]);
        assert_eq!(accs[1]["color"], PALETTE[1]);
        assert_eq!(disk["ignoredDataDirs"], json!(["C:\\i"]));
        assert_eq!(st.settings().theme, "light");
        assert_eq!(keys(&disk["settings"]), keys(&serde_json::to_value(Settings::default()).unwrap()));
    }

    /// The store's exact on-disk bytes (`JSON.stringify(state, null, 2)` layout) are reproduced.
    #[test]
    fn store_bytes_are_stable() {
        let env = Env::new();
        let sample = "{\n  \"version\": 2,\n  \"accounts\": [\n    {\n      \"id\": \"main\",\n      \"name\": \"Main\",\n      \"dataDir\": null,\n      \"color\": \"#F08A5D\",\n      \"label\": null,\n      \"order\": 0\n    },\n    {\n      \"id\": \"11111111-2222-4333-8444-555555555555\",\n      \"name\": \"Design\",\n      \"dataDir\": \"C:\\\\Users\\\\example\\\\AppData\\\\Roaming\\\\Claude-Design\",\n      \"color\": \"#5B9CF6\",\n      \"label\": \"me@example.com\",\n      \"order\": 1\n    }\n  ],\n  \"settings\": {\n    \"startWithWindows\": false,\n    \"shareMcpConfig\": true,\n    \"showBubble\": true,\n    \"bubblePosition\": {\n      \"x\": 1840,\n      \"y\": 512\n    },\n    \"theme\": \"dark\",\n    \"hotkey\": \"Ctrl+Shift+Space\",\n    \"panelSide\": \"auto\",\n    \"bubbleMode\": \"tray\"\n  },\n  \"ignoredDataDirs\": [\n    \"C:\\\\Users\\\\example\\\\AppData\\\\Roaming\\\\Claude-old\"\n  ]\n}";
        fs::write(env.user_data.join("sessionmux.json"), sample).unwrap();
        let st = env.store();
        // Same bytes, plus Sessionmux's own `signInRouting` (default true) after bubbleMode.
        let expected = with_sign_in_routing(sample);
        assert!(expected.contains("\"bubbleMode\": \"tray\",\n    \"signInRouting\": true\n  },"));
        assert_eq!(fs::read_to_string(env.user_data.join("sessionmux.json")).unwrap(), expected);
        let parsed: Store = serde_json::from_str(sample).unwrap();
        assert_eq!(st.snapshot(), parsed);
        // Written once with the key, the store is a fixed point.
        let again = env.store();
        assert_eq!(fs::read_to_string(env.user_data.join("sessionmux.json")).unwrap(), expected);
        assert_eq!(again.snapshot(), parsed);
    }

    /// A store text as Sessionmux writes it: a missing `signInRouting` is inserted as `true` after
    /// the bubbleMode line, and a retired settings line is dropped.
    fn with_sign_in_routing(text: &str) -> String {
        let mut text = text.to_string();
        if let Some(at) = text.find(&format!("\n    \"{RETIRED_KEY}\": ")) {
            let eol = at + 1 + text[at + 1..].find('\n').expect("end of settings line");
            let start = if text[..at].ends_with(',') { at - 1 } else { at };
            text = format!("{}{}", &text[..start], &text[eol..]);
        }
        if !text.contains("\"signInRouting\"") {
            let at = text.find("\n    \"bubbleMode\": ").expect("settings line");
            let eol = at + 1 + text[at + 1..].find('\n').expect("end of settings line");
            text = format!("{},\n    \"signInRouting\": true{}", &text[..eol], &text[eol..]);
        }
        text
    }

    /// Fixture: a copy of the real `%APPDATA%\Sessionmux\sessionmux.json` (read-only; skipped if
    /// absent) survives open + save with the same keys, names, ids and bytes.
    #[test]
    fn real_store_fixture_round_trip() {
        let real = paths::app_data().join(brand::DATA_DIR_NAME).join(brand::STORE_FILE);
        let Ok(original) = fs::read_to_string(&real) else {
            eprintln!("no real store at {}; fixture test skipped", real.display());
            return;
        };
        let env = Env::new();
        fs::write(env.user_data.join("sessionmux.json"), &original).unwrap();
        let st = env.store();
        let written = fs::read_to_string(env.user_data.join("sessionmux.json")).unwrap();
        // A store from before `signInRouting` existed gains it; a retired key is dropped.
        let original = with_sign_in_routing(&original);
        let before: Value = serde_json::from_str(original.trim_start_matches('\u{FEFF}')).unwrap();
        let after: Value = serde_json::from_str(&written).unwrap();
        assert_eq!(keys(&after), keys(&before));
        assert_eq!(keys(&after["settings"]), keys(&before["settings"]));
        let names = |v: &Value| {
            v["accounts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| (a["id"].clone(), a["name"].clone(), keys(a)))
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&after), names(&before));
        assert_eq!(after, before, "normalised store must be a fixed point");
        assert_eq!(written, original, "byte-identical after a round trip");
        assert_eq!(st.snapshot().accounts.len(), before["accounts"].as_array().unwrap().len());
    }
}
