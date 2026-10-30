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

//! Cowork session listing / transfer / deletion.
//!
//! Layout recap:
//! ```text
//!   <dataDir>/local-agent-mode-sessions/<acct>/<org>/local_<uuid>.json      session record
//!   <dataDir>/local-agent-mode-sessions/<acct>/<org>/<8 hex>/               session folder
//!                                    (or local_<uuid>/ when another session has that prefix)
//!     outputs/  uploads/                                                     the session's files
//!     .claude/projects/<dir>/<cliSessionId>.jsonl (+ <cliSessionId>/)         transcript
//!   <dataDir>/local-agent-mode-sessions/<acct>/<org>/agent/                  not listed
//! ```
//! `<acct>` and `<org>` are full uuids or their first 8 characters. Everything else next to the
//! records (manifests, spaces, memory, plugins, ...) belongs to the whole account and is never
//! copied. Only plain Cowork sessions and chats are listed: dispatched, scheduled and background
//! sessions are tied to things that do not exist in another account.
//!
//! Claude loads these records when it starts and keeps them in memory while it runs, so
//! `running` is the account's Claude being open: a copy into a running Claude shows after it is
//! restarted, a move out of a running Claude leaves the source in place (`source-left-open`) and
//! a delete waits until it is closed.
//!
//! A transfer writes a fresh record with a new id that holds only what a new session needs; the
//! keys tied to the source account (spaces, connectors, prompts, scheduled tasks, ...) are left
//! out, as Claude does when it imports a session itself.

use super::*;

/// Folder below the data folder that holds the Cowork records.
const ROOT: &str = "local-agent-mode-sessions";
/// Names never copied from a session's outputs or uploads (compared in lower case).
const SKIP_NAMES: [&str; 5] = [".claude", ".git", ".mcp.json", "claude.md", "claude.local.md"];
/// Longest process name Claude accepts.
const PROCESS_NAME_MAX: usize = 32;

const NAME_WORDS_A: [&str; 24] = [
    "amber", "brisk", "calm", "clever", "eager", "gentle", "golden", "happy", "jolly", "keen", "lively", "lucky",
    "mellow", "nimble", "quiet", "rapid", "silver", "steady", "sunny", "swift", "tidy", "vivid", "warm", "witty",
];
const NAME_WORDS_B: [&str; 24] = [
    "badger", "beacon", "brook", "cedar", "comet", "falcon", "fern", "harbor", "heron", "lagoon", "maple", "meadow",
    "orchid", "otter", "pebble", "pine", "quartz", "raven", "river", "sparrow", "summit", "thistle", "willow",
    "zephyr",
];

/// One Cowork session record file.
struct CoworkRecord {
    file: PathBuf,
    /// The `<acct>/<org>` folder that holds it.
    dir: PathBuf,
    session_id: String,
}

fn root(a: &SessionAccount) -> PathBuf {
    Path::new(&a.data_dir).join(ROOT)
}

/// The first 8 characters of a name (the short folder form), or the name itself.
fn short(name: &str) -> &str {
    name.get(..8).unwrap_or(name)
}

/// `local_*.json` files directly in `dir`, as (file, session id).
fn record_files_in(dir: &Path) -> Vec<(PathBuf, String)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for ent in rd.flatten() {
        if !ent.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let name = ent.file_name().to_string_lossy().into_owned();
        let l = name.to_lowercase();
        if !(l.starts_with("local_") && l.ends_with(".json") && l.len() > "local_.json".len()) {
            continue;
        }
        out.push((dir.join(&name), name[..name.len() - ".json".len()].to_string()));
    }
    out
}

/// Every record of every `<acct>/<org>` folder of the account (the `agent/` folders are not
/// looked into).
fn records(a: &SessionAccount) -> Vec<CoworkRecord> {
    let base = root(a);
    let mut out = Vec::new();
    for acct in list_dirs(&base) {
        for org in list_dirs(&base.join(&acct)) {
            let dir = base.join(&acct).join(&org);
            for (file, session_id) in record_files_in(&dir) {
                out.push(CoworkRecord { file, dir: dir.clone(), session_id });
            }
        }
    }
    out
}

fn find(a: &SessionAccount, session_id: &str) -> Option<CoworkRecord> {
    records(a).into_iter().find(|r| r.session_id == session_id)
}

/// Plain Cowork sessions (no `sessionType`) and chats; every other type is left out.
fn listed_type(rec: &OVal) -> bool {
    match rec.get("sessionType") {
        None | Some(OVal::Null) => true,
        Some(OVal::Str(t)) => t == "chat",
        _ => false,
    }
}

/// The folder of a session: `local_<uuid>/` when it exists, else the 8-character short name.
fn session_folder(dir: &Path, session_id: &str) -> PathBuf {
    let full = dir.join(session_id);
    if full.is_dir() {
        return full;
    }
    let rest = session_id.strip_prefix("local_").unwrap_or(session_id);
    dir.join(short(rest))
}

/// The main transcript of a session: `.claude/projects/session/<cli>.jsonl`, else the first
/// other folder of `.claude/projects` that has it.
fn find_transcript(folder: &Path, cli: &str) -> Option<PathBuf> {
    let projects = folder.join(".claude").join("projects");
    let file = format!("{cli}.jsonl");
    let first = projects.join("session").join(&file);
    if first.is_file() {
        return Some(first);
    }
    let mut dirs = list_dirs(&projects);
    dirs.sort();
    dirs.into_iter().map(|d| projects.join(d).join(&file)).find(|p| p.is_file())
}

fn cli_of(rec: &OVal) -> Option<String> {
    rec.get_str("cliSessionId").filter(|c| !c.is_empty()).map(String::from)
}

/// Cowork sessions of an account (unsorted). A record that does not parse is skipped.
pub(super) fn list(a: &SessionAccount) -> Vec<Session> {
    let mut out = Vec::new();
    for r in records(a) {
        if !valid_session_id(&r.session_id) {
            continue;
        }
        let rec = match read_oval(&r.file) {
            Some(rec @ OVal::Obj(_)) => rec,
            _ => {
                log::warn(&format!("sessions: unreadable record {}", r.file.display()));
                continue;
            }
        };
        if !listed_type(&rec) {
            continue;
        }
        let folder = session_folder(&r.dir, &r.session_id);
        let cli = cli_of(&rec);
        let has_transcript = cli.as_deref().and_then(|c| find_transcript(&folder, c)).is_some();
        let selected = match rec.get("userSelectedFolders") {
            Some(OVal::Arr(v)) => v.iter().find_map(|x| match x {
                OVal::Str(s) if !s.is_empty() => Some(s.clone()),
                _ => None,
            }),
            _ => None,
        };
        let cwd = selected.unwrap_or_default();
        let open = if cwd.is_empty() { lossy(&folder.join("outputs")) } else { cwd.clone() };
        out.push(Session {
            session_id: r.session_id,
            cli_session_id: cli.unwrap_or_default(),
            title: rec.get_str("title").unwrap_or("").to_string(),
            is_scratch: false,
            last_activity_at: OVal::js_number_or_zero(rec.get("lastActivityAt")),
            created_at: OVal::js_number_or_zero(rec.get("createdAt")),
            model: OVal::display_or_empty(rec.get("model")),
            effort: String::new(),
            is_archived: rec.get("isArchived").map(OVal::truthy).unwrap_or(false),
            completed_turns: OVal::js_number_or_zero(rec.get("completedTurns")).min(u64::from(u32::MAX)) as u32,
            running: a.running,
            has_transcript,
            kind: KIND_COWORK,
            folder: open,
            cwd,
        });
    }
    out
}

/// The `<acct>/<org>` folder new Cowork records of `to` go into. The account comes from the
/// data folder; the organisation is the one already used for Cowork, else the one its Code
/// sessions use. None when neither is known.
fn target_dir(to: &SessionAccount) -> Option<PathBuf> {
    let acct = Sessions::account_uuid_of(to)?;
    let base = root(to);
    let code_org = newest_dir(&Path::new(&to.data_dir).join("claude-code-sessions").join(&acct));
    // Short folder names win when both forms exist.
    let pick = |parent: &Path, name: &str| -> Option<PathBuf> {
        [short(name), name].into_iter().map(|n| parent.join(n)).find(|p| p.is_dir())
    };
    match pick(&base, &acct) {
        Some(acct_dir) => {
            if let Some(dir) = code_org.as_deref().and_then(|o| pick(&acct_dir, o)) {
                return Some(dir);
            }
            if let Some(o) = newest_dir(&acct_dir) {
                return Some(acct_dir.join(o));
            }
            code_org.map(|o| acct_dir.join(o))
        }
        None => code_org.map(|o| base.join(&acct).join(o)),
    }
}

/// Every processName / vmProcessName of the records under `base` (the `agent/` folders too).
fn process_names(base: &Path) -> HashSet<String> {
    let mut names = HashSet::new();
    for acct in list_dirs(base) {
        for org in list_dirs(&base.join(&acct)) {
            let dir = base.join(&acct).join(&org);
            for d in [dir.clone(), dir.join("agent")] {
                for (file, _) in record_files_in(&d) {
                    if let Some(rec) = read_oval(&file) {
                        for k in ["processName", "vmProcessName"] {
                            if let Some(n) = rec.get_str(k) {
                                names.insert(n.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    names
}

/// A process name in Claude's own form (`word-word-word`) that no record in `taken` uses.
fn unique_process_name(taken: &HashSet<String>) -> String {
    for _ in 0..1000 {
        let r = uuid::Uuid::new_v4();
        let b = r.as_bytes();
        let first = NAME_WORDS_A[b[0] as usize % NAME_WORDS_A.len()];
        let mut second = NAME_WORDS_A[b[1] as usize % NAME_WORDS_A.len()];
        if second == first {
            second = NAME_WORDS_A[(b[1] as usize + 1) % NAME_WORDS_A.len()];
        }
        let third = NAME_WORDS_B[b[2] as usize % NAME_WORDS_B.len()];
        let name = format!("{first}-{second}-{third}");
        if name.len() <= PROCESS_NAME_MAX && !taken.contains(&name) {
            return name;
        }
    }
    let hex = uuid::Uuid::new_v4().simple().to_string();
    format!("session-{}", &hex[..12])
}

/// The folder name for a new session in `dir`: the 8-character short form of its uuid, or
/// `local_<uuid>` when a folder or another session there already starts with that prefix.
fn new_folder_name(dir: &Path, session_id: &str) -> String {
    let rest = session_id.strip_prefix("local_").unwrap_or(session_id);
    let prefix = short(rest).to_lowercase();
    let claimed = std::fs::symlink_metadata(dir.join(short(rest))).is_ok()
        || record_files_in(dir).iter().any(|(_, id)| {
            id != session_id
                && id.strip_prefix("local_").map(|r| r.to_lowercase().starts_with(&prefix)).unwrap_or(false)
        });
    if claimed {
        session_id.to_string()
    } else {
        short(rest).to_string()
    }
}

/// Copy the files and folders of `src` into `dst` (never links), leaving out [`SKIP_NAMES`]
/// at every level. Files another process keeps locked are counted in `skipped`.
fn copy_session_files(src: &Path, dst: &Path, skipped: &mut usize) -> io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for ent in std::fs::read_dir(src)? {
        let ent = ent?;
        let name = ent.file_name().to_string_lossy().into_owned();
        if SKIP_NAMES.contains(&name.to_lowercase().as_str()) {
            continue;
        }
        let ft = ent.file_type()?;
        let d = dst.join(ent.file_name());
        if ft.is_dir() {
            copy_session_files(&ent.path(), &d, skipped)?;
        } else if ft.is_file() {
            if d.exists() {
                continue;
            }
            copy_file_tolerant(&ent.path(), &d, skipped)?;
        } else {
            log::debug(&format!("sessions: skipped link or special entry {}", ent.path().display()));
        }
    }
    Ok(())
}

/// The first folder of `p` that does not exist yet (what `create_dir_all(p)` would create), or
/// None when `p` exists.
fn first_missing(p: &Path) -> Option<PathBuf> {
    if p.exists() {
        return None;
    }
    let mut cur = p.to_path_buf();
    while let Some(parent) = cur.parent() {
        if parent.as_os_str().is_empty() || parent.exists() {
            break;
        }
        cur = parent.to_path_buf();
    }
    Some(cur)
}

/// A key of the source record, when it has one.
fn copy_key(next: &mut OVal, rec: &OVal, key: &str) {
    if let Some(v) = rec.get(key) {
        next.set(key, v.clone());
    }
}

impl Sessions {
    /// Copy or move a Cowork session to another account. Anything created is removed again if
    /// a later step fails.
    pub fn transfer_cowork(
        &self,
        from: &SessionAccount,
        to: &SessionAccount,
        session_id: &str,
        mode: &str,
    ) -> TransferResult {
        let mut created: Vec<PathBuf> = Vec::new();
        match transfer_inner(from, to, session_id, mode, &mut created) {
            Ok(r) => r,
            Err(e) => {
                log::error(&format!("sessions: Cowork transfer failed: {e}"));
                for p in created.iter().rev() {
                    let _ = rm_rf(p);
                }
                TransferResult::fail(Some("error"), Some(e))
            }
        }
    }

    /// Delete a Cowork session record (its folder stays). Refused while the account's Claude is
    /// open, since Claude would write the record again.
    pub fn delete_cowork(&self, a: &SessionAccount, session_id: &str) -> Result<(), ApiError> {
        if a.data_dir.is_empty() {
            return Err(ApiError::new("io", "Source account has no data folder"));
        }
        if !valid_session_id(session_id) {
            return Err(ApiError::new("invalid", "Invalid session id"));
        }
        if a.running {
            return Err(ApiError::new("running", "Claude is open for this account — quit it first"));
        }
        let Some(found) = find(a, session_id) else {
            return Err(ApiError::new("not-found", "Session not found"));
        };
        match rm_rf(&found.file) {
            Ok(()) => {
                log::info(&format!("sessions: deleted Cowork record {session_id}"));
                Ok(())
            }
            Err(e) => {
                log::error(&format!("sessions: Cowork delete failed: {e}"));
                Err(ApiError::new("io", e.to_string()))
            }
        }
    }
}

fn transfer_inner(
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
    if mode == "move" && paths::same_path(&from.data_dir, &to.data_dir) {
        return Ok(TransferResult::fail(Some("same-account"), Some("Source and target are the same account".into())));
    }

    // 1. Source record and folder.
    let Some(src) = find(from, session_id) else {
        return Ok(TransferResult::fail(Some("not-found"), Some("Session not found".into())));
    };
    let record = match read_record_settled(&src.file) {
        Some(r @ OVal::Obj(_)) => r,
        _ => return Ok(TransferResult::fail(None, Some("Session record is unreadable".into()))),
    };
    if !listed_type(&record) {
        return Ok(TransferResult::fail(None, Some("This kind of Cowork session can't be transferred".into())));
    }
    let src_folder = session_folder(&src.dir, session_id);

    // 2. Target folder.
    let Some(dir) = target_dir(to) else {
        return Ok(TransferResult::fail(Some("target-never-opened"), None));
    };
    if let Some(top) = first_missing(&dir) {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        created.push(top);
    }

    // 3. New identity and session folder.
    let new_session_id = format!("local_{}", uuid::Uuid::new_v4());
    let folder = dir.join(new_folder_name(&dir, &new_session_id));
    created.push(folder.clone()); // registered first so a failed copy is rolled back
    let outputs = folder.join("outputs");
    std::fs::create_dir_all(&outputs).map_err(|e| e.to_string())?;

    // 4. Transcript (and its sibling folder), outputs and uploads.
    let mut warnings: Vec<String> = Vec::new();
    let mut skipped = 0usize;
    let cli = cli_of(&record);
    let transcript = cli.as_deref().and_then(|c| find_transcript(&src_folder, c));
    match (cli.as_deref(), transcript.as_deref().and_then(read_snapshot)) {
        (Some(c), Some(bytes)) => {
            let dst_dir = folder.join(".claude").join("projects").join("session");
            std::fs::create_dir_all(&dst_dir).map_err(|e| e.to_string())?;
            std::fs::write(dst_dir.join(format!("{c}.jsonl")), bytes).map_err(|e| e.to_string())?;
            let sib = transcript.as_deref().and_then(Path::parent).map(|p| p.join(c));
            if let Some(sib) = sib.filter(|p| p.is_dir()) {
                skipped += copy_dir(&sib, &dst_dir.join(c), false).map_err(|e| e.to_string())?;
            }
        }
        _ => warnings.push("no-transcript".into()),
    }
    for sub in ["outputs", "uploads"] {
        let s = src_folder.join(sub);
        if std::fs::symlink_metadata(&s).map(|m| m.is_dir()).unwrap_or(false) {
            copy_session_files(&s, &folder.join(sub), &mut skipped).map_err(|e| e.to_string())?;
        }
    }
    #[cfg(test)]
    if FAIL_COPY.with(|f| f.get()) {
        return Err("disk full".into());
    }
    if skipped > 0 {
        warnings.push(format!("files-skipped:{skipped}"));
    }

    // 5. The new record: only what a new session needs, in Claude's own key order.
    let name = unique_process_name(&process_names(&root(to)));
    let now = OVal::Num(log::now_millis().into());
    let mut next = OVal::Obj(Vec::new());
    next.set("sessionId", OVal::Str(new_session_id.clone()));
    if let Some(c) = &cli {
        next.set("cliSessionId", OVal::Str(c.clone()));
    }
    next.set("processName", OVal::Str(name.clone()));
    next.set("vmProcessName", OVal::Str(name));
    next.set("cwd", OVal::Str(lossy(&outputs)));
    if let Some(v @ OVal::Bool(_)) = record.get("hostLoopMode") {
        next.set("hostLoopMode", v.clone());
    }
    if record.get_str("sessionType") == Some("chat") {
        next.set("sessionType", OVal::Str("chat".into()));
    }
    if let Some(t @ OVal::Str(_)) = record.get("title") {
        next.set("title", t.clone());
    }
    for key in ["createdAt", "lastActivityAt"] {
        match record.get(key) {
            Some(v @ OVal::Num(_)) => next.set(key, v.clone()),
            _ => next.set(key, now.clone()),
        }
    }
    next.set("isArchived", OVal::Bool(matches!(record.get("isArchived"), Some(OVal::Bool(true)))));
    for key in ["userSelectedFolders", "resolvedFolderKinds"] {
        match record.get(key) {
            Some(OVal::Arr(_)) => copy_key(&mut next, &record, key),
            _ => next.set(key, OVal::Arr(Vec::new())),
        }
    }
    next.set("folderMountNames", OVal::Obj(Vec::new()));
    next.set("pendingNotifications", OVal::Arr(Vec::new()));
    let dst_record = dir.join(format!("{new_session_id}.json"));
    write_record_atomic(&dst_record, &next).map_err(|e| e.to_string())?;
    created.push(dst_record);

    // 6. Move: remove the source only when its Claude is closed (an open one keeps it in memory
    // and would write it again).
    let source_running = from.running;
    if mode == "move" {
        created.clear(); // past the point of no return; keep the copy.
        if source_running {
            warnings.push("source-left-open".into());
            log::info(&format!("sessions: Claude is open for {session_id}; its source files stay in place"));
        } else if let Err(e) = rm_rf(&src.file) {
            warnings.push("source-not-removed".into());
            log::warn(&format!("sessions: copied but could not remove source {}: {e}", src.file.display()));
        } else if rel_inside(&src.dir, &lossy(&src_folder)).map(|r| !r.is_empty()).unwrap_or(false)
            && std::fs::symlink_metadata(&src_folder).is_ok()
        {
            // Only a folder inside the record's own <acct>/<org> folder is ever deleted.
            if let Err(e) = rm_rf(&src_folder) {
                warnings.push("folder-not-removed".into());
                log::warn(&format!("sessions: could not remove {}: {e}", src_folder.display()));
            }
        }
    }

    log::info(&format!("sessions: Cowork {mode} {session_id} -> {new_session_id}"));
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::fs;

    const ACCT_A: &str = "aaaaaaaa-0000-0000-0000-000000000001";
    const ACCT_B: &str = "bbbbbbbb-0000-0000-0000-000000000002";
    const ORG_A: &str = "a0a0a0a0-0000-0000-0000-0000000000aa";
    const ORG_B: &str = "b0b0b0b0-0000-0000-0000-0000000000bb";
    const UUID1: &str = "c1c1c1c1-1111-4111-8111-111111111111";
    const UUID2: &str = "c2c2c2c2-2222-4222-8222-222222222222";
    const CLI1: &str = "d1d1d1d1-1111-4111-8111-111111111111";

    fn write_json(f: &Path, v: &Value) {
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(f, serde_json::to_string(v).unwrap()).unwrap();
    }
    fn read_json(f: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(f).unwrap()).unwrap()
    }
    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    struct E {
        _tmp: tempfile::TempDir,
        dir_a: PathBuf,
        dir_b: PathBuf,
        /// Cowork records of A (short account and organisation folder names).
        rec_a: PathBuf,
        /// Session 1's folder (short name).
        folder1: PathBuf,
        api: Sessions,
        a: SessionAccount,
        b: SessionAccount,
    }

    fn account(id: &str, dir: &Path, uuid: &str, running: bool) -> SessionAccount {
        SessionAccount {
            id: id.into(),
            name: id.to_uppercase(),
            data_dir: s(dir),
            is_main: false,
            account_uuid: Some(uuid.into()),
            running,
        }
    }

    fn setup() -> E {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let dir_a = root.join("Claude-A");
        let dir_b = root.join("Claude-B");
        write_json(&dir_a.join("config.json"), &json!({"lastKnownAccountUuid": ACCT_A}));
        write_json(&dir_b.join("config.json"), &json!({"lastKnownAccountUuid": ACCT_B}));
        fs::create_dir_all(dir_b.join("claude-code-sessions").join(ACCT_B).join(ORG_B)).unwrap();

        let rec_a = dir_a.join(ROOT).join(short(ACCT_A)).join(short(ORG_A));
        // Session 1: a Cowork session with transcript, outputs, uploads and account-bound keys.
        write_json(
            &rec_a.join(format!("local_{UUID1}.json")),
            &json!({
                "sessionId": format!("local_{UUID1}"), "processName": "old-name-one", "vmProcessName": "old-name-one",
                "cliSessionId": CLI1, "cwd": "/sessions/old-name-one", "hostLoopMode": false, "title": "Research notes",
                "createdAt": 100, "lastActivityAt": 4000, "isArchived": false, "spaceId": "space-1",
                "accountName": "A", "emailAddress": "a@example.com", "systemPrompt": "x", "enabledMcpTools": {"t": true},
                "userSelectedFolders": ["C:\\Work\\Docs"], "resolvedFolderKinds": ["folder"],
                "folderMountNames": {"C:\\Work\\Docs": "Docs"}, "importedFrom": {"k": 1},
                "pendingNotifications": [{"n": 1}],
            }),
        );
        let folder1 = rec_a.join(short(UUID1));
        let tdir = folder1.join(".claude").join("projects").join("-sessions-old-name-one");
        fs::create_dir_all(tdir.join(CLI1)).unwrap();
        fs::write(tdir.join(format!("{CLI1}.jsonl")), "{\"type\":\"user\"}\n").unwrap();
        fs::write(tdir.join(CLI1).join("sub.json"), "{}").unwrap();
        fs::create_dir_all(folder1.join(".claude").join("statsig")).unwrap();
        fs::write(folder1.join(".claude").join("statsig").join("cache"), "account cache").unwrap();
        fs::create_dir_all(folder1.join("outputs").join("deep").join(".git")).unwrap();
        fs::write(folder1.join("outputs").join("report.md"), "report").unwrap();
        fs::write(folder1.join("outputs").join("CLAUDE.md"), "instructions").unwrap();
        fs::write(folder1.join("outputs").join(".mcp.json"), "{}").unwrap();
        fs::write(folder1.join("outputs").join("deep").join("data.csv"), "1,2").unwrap();
        fs::write(folder1.join("outputs").join("deep").join("claude.local.md"), "x").unwrap();
        fs::create_dir_all(folder1.join("uploads")).unwrap();
        fs::write(folder1.join("uploads").join("brief.pdf"), "pdf").unwrap();
        fs::write(folder1.join("audit.jsonl"), "{}").unwrap();
        // Account-wide files next to the records.
        write_json(&rec_a.join("spaces.json"), &json!({"spaces": []}));
        fs::create_dir_all(rec_a.join("memory")).unwrap();

        // Session 2: a chat in full-name folders with its own local_<uuid> session folder.
        let rec_a_full = dir_a.join(ROOT).join(ACCT_A).join(ORG_A);
        write_json(
            &rec_a_full.join(format!("local_{UUID2}.json")),
            &json!({"sessionId": format!("local_{UUID2}"), "processName": "p2", "cwd": "C:\\x", "sessionType": "chat",
                    "title": "Quick chat", "createdAt": 50, "lastActivityAt": 6000, "isArchived": true}),
        );
        let folder2 = rec_a_full.join(format!("local_{UUID2}"));
        fs::create_dir_all(folder2.join(".claude").join("projects").join("session")).unwrap();
        fs::write(folder2.join(".claude").join("projects").join("session").join("no-cli.jsonl"), "").unwrap();

        // Left out: other types, a broken record, the agent folder.
        for (n, t) in [(3, "agent"), (4, "dispatch_child"), (5, "radar"), (6, "scheduled")] {
            write_json(
                &rec_a.join(format!("local_{n}{n}{n}{n}{n}{n}{n}{n}-0000-4000-8000-000000000000.json")),
                &json!({"sessionId": "x", "processName": "p", "cwd": "c", "sessionType": t, "title": t,
                        "createdAt": 1, "lastActivityAt": 1}),
            );
        }
        fs::write(rec_a.join("local_99999999-0000-4000-8000-000000000000.json"), "{ not json").unwrap();
        write_json(
            &rec_a.join("agent").join("local_88888888-0000-4000-8000-000000000000.json"),
            &json!({"sessionId": "y", "processName": "taken-agent-name", "cwd": "c", "title": "Agent",
                    "createdAt": 1, "lastActivityAt": 1}),
        );

        let a = account("a", &dir_a, ACCT_A, false);
        let b = account("b", &dir_b, ACCT_B, true);
        E { api: Sessions { home: root.join("home") }, dir_a, dir_b, rec_a, folder1, a, b, _tmp: tmp }
    }

    fn id1() -> String {
        format!("local_{UUID1}")
    }

    /// The Cowork records folder of B a transfer creates (full names, from its Code folder).
    fn rec_b(e: &E) -> PathBuf {
        e.dir_b.join(ROOT).join(ACCT_B).join(ORG_B)
    }

    #[test]
    fn list_includes_cowork_and_chat_only() {
        let e = setup();
        let list = e.api.list_all(&e.a);
        let titles: Vec<&str> = list.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["Quick chat", "Research notes"]);
        let (chat, cw) = (&list[0], &list[1]);
        assert!(list.iter().all(|s| s.kind == KIND_COWORK));
        assert_eq!(cw.session_id, id1());
        assert_eq!(cw.cli_session_id, CLI1);
        assert!(cw.has_transcript, "transcript found in a non-'session' projects folder");
        assert!(!cw.is_archived);
        assert_eq!(cw.cwd, "C:\\Work\\Docs");
        assert_eq!(cw.folder, "C:\\Work\\Docs");
        assert_eq!(cw.last_activity_at, 4000);
        assert!(!cw.running);
        assert!(chat.is_archived);
        assert!(!chat.has_transcript);
        assert_eq!(chat.cwd, "");
        assert_eq!(
            chat.folder,
            s(&e.dir_a.join(ROOT).join(ACCT_A).join(ORG_A).join(format!("local_{UUID2}")).join("outputs"))
        );
        // Code-only listing is unchanged.
        assert!(e.api.list(&e.a).is_empty());
        // Running follows the account's Claude.
        let running = e.api.list_all(&SessionAccount { running: true, ..e.a.clone() });
        assert!(running.iter().all(|s| s.running));
        assert_eq!(serde_json::to_value(cw).unwrap()["kind"], "cowork");
    }

    #[test]
    fn list_mixes_code_and_cowork_by_last_activity() {
        let e = setup();
        let code = e.dir_a.join("claude-code-sessions").join(ACCT_A).join(ORG_A);
        write_json(
            &code.join("local_code1.json"),
            &json!({"sessionId": "local_code1", "cwd": "C:\\p", "title": "Code one", "lastActivityAt": 5000}),
        );
        let list = e.api.list_all(&e.a);
        let kinds: Vec<(&str, &str)> = list.iter().map(|s| (s.title.as_str(), s.kind)).collect();
        assert_eq!(kinds, [("Quick chat", "cowork"), ("Code one", "code"), ("Research notes", "cowork")]);
        assert_eq!(list[1].folder, "C:\\p");
    }

    #[test]
    fn copy_writes_fresh_record_with_importer_keys_only() {
        let e = setup();
        let r = e.api.transfer_cowork(&e.a, &e.b, &id1(), "copy");
        assert!(r.ok, "{r:?}");
        assert!(r.warnings.is_empty(), "{r:?}");
        assert!(r.target_running);
        assert!(!r.source_running);
        let nid = r.new_session_id.clone().unwrap();
        assert_ne!(nid, id1());
        assert!(uuid::Uuid::parse_str(nid.strip_prefix("local_").unwrap()).is_ok());

        // Source untouched.
        assert!(e.rec_a.join(format!("{}.json", id1())).exists());
        assert!(e.folder1.join("outputs").join("report.md").exists());

        let dir = rec_b(&e);
        let raw = fs::read_to_string(dir.join(format!("{nid}.json"))).unwrap();
        let rec: Value = serde_json::from_str(&raw).unwrap();
        let keys: Vec<&str> = rec.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        let mut want = vec![
            "sessionId",
            "cliSessionId",
            "processName",
            "vmProcessName",
            "cwd",
            "hostLoopMode",
            "title",
            "createdAt",
            "lastActivityAt",
            "isArchived",
            "userSelectedFolders",
            "folderMountNames",
            "resolvedFolderKinds",
            "pendingNotifications",
        ];
        let mut got = keys.clone();
        got.sort();
        want.sort();
        assert_eq!(got, want, "{raw}");
        assert!(raw.starts_with("{\n  \"sessionId\": "));
        assert_eq!(rec["sessionId"], nid.as_str());
        assert_eq!(rec["cliSessionId"], CLI1);
        assert_ne!(rec["processName"], "old-name-one");
        assert_eq!(rec["processName"], rec["vmProcessName"]);
        assert!(rec["processName"].as_str().unwrap().len() <= PROCESS_NAME_MAX);
        assert_eq!(rec["hostLoopMode"], false);
        assert_eq!(rec["title"], "Research notes");
        assert_eq!(rec["createdAt"], 100);
        assert_eq!(rec["lastActivityAt"], 4000);
        assert_eq!(rec["isArchived"], false);
        assert_eq!(rec["userSelectedFolders"], json!(["C:\\Work\\Docs"]));
        assert_eq!(rec["resolvedFolderKinds"], json!(["folder"]));
        assert_eq!(rec["folderMountNames"], json!({}));
        assert_eq!(rec["pendingNotifications"], json!([]));

        // Session folder: 8-character name, transcript under projects/session, filtered outputs.
        let uuid = nid.strip_prefix("local_").unwrap();
        let folder = dir.join(&uuid[..8]);
        assert_eq!(rec["cwd"], s(&folder.join("outputs")).as_str());
        let t = folder.join(".claude").join("projects").join("session");
        assert_eq!(fs::read_to_string(t.join(format!("{CLI1}.jsonl"))).unwrap(), "{\"type\":\"user\"}\n");
        assert!(t.join(CLI1).join("sub.json").exists());
        assert!(!folder.join(".claude").join("statsig").exists(), "account caches not copied");
        assert_eq!(fs::read_to_string(folder.join("outputs").join("report.md")).unwrap(), "report");
        assert!(folder.join("outputs").join("deep").join("data.csv").exists());
        assert!(!folder.join("outputs").join("CLAUDE.md").exists());
        assert!(!folder.join("outputs").join(".mcp.json").exists());
        assert!(!folder.join("outputs").join("deep").join(".git").exists());
        assert!(!folder.join("outputs").join("deep").join("claude.local.md").exists());
        assert!(folder.join("uploads").join("brief.pdf").exists());
        assert!(!folder.join("audit.jsonl").exists());
        // Nothing account-wide came across.
        assert!(!dir.join("spaces.json").exists());
        assert!(!dir.join("memory").exists());

        // Listed in the target, with its transcript.
        let list = e.api.list_all(&e.b);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].session_id, nid);
        assert!(list[0].has_transcript);
    }

    #[test]
    fn copy_of_chat_keeps_type_and_uses_long_folder_when_prefix_is_claimed() {
        let e = setup();
        let dir = rec_b(&e);
        // Every 8-character prefix is claimed by a folder, so the new folder is local_<uuid>.
        assert_eq!(new_folder_name(&dir, "local_c3c3c3c3-0000-4000-8000-000000000000"), "c3c3c3c3");
        fs::create_dir_all(dir.join("c3c3c3c3")).unwrap();
        assert_eq!(
            new_folder_name(&dir, "local_c3c3c3c3-0000-4000-8000-000000000000"),
            "local_c3c3c3c3-0000-4000-8000-000000000000"
        );
        // A record with the same prefix claims it too.
        write_json(&dir.join("local_c4c4c4c4-9999-4000-8000-000000000000.json"), &json!({}));
        assert_eq!(
            new_folder_name(&dir, "local_c4c4c4c4-0000-4000-8000-000000000000"),
            "local_c4c4c4c4-0000-4000-8000-000000000000"
        );

        let r = e.api.transfer_cowork(&e.a, &e.b, &format!("local_{UUID2}"), "copy");
        assert!(r.ok, "{r:?}");
        assert_eq!(r.warnings, ["no-transcript"]);
        let rec = read_json(&dir.join(format!("{}.json", r.new_session_id.unwrap())));
        assert_eq!(rec["sessionType"], "chat");
        assert_eq!(rec["isArchived"], true);
        assert!(rec.get("cliSessionId").is_none());
        assert!(rec.get("hostLoopMode").is_none());
        assert_eq!(rec["userSelectedFolders"], json!([]));
    }

    #[test]
    fn process_name_is_unique_across_the_target_data_folder() {
        let mut taken = HashSet::new();
        for a in NAME_WORDS_A {
            for b in NAME_WORDS_A {
                for c in NAME_WORDS_B {
                    taken.insert(format!("{a}-{b}-{c}"));
                }
            }
        }
        let name = unique_process_name(&taken);
        assert!(!taken.contains(&name));
        assert!(name.len() <= PROCESS_NAME_MAX);

        let e = setup();
        let names = process_names(&root(&e.a));
        assert!(names.contains("old-name-one"));
        assert!(names.contains("taken-agent-name"), "agent records count too");
    }

    #[test]
    fn move_removes_source_record_and_folder() {
        let e = setup();
        let r = e.api.transfer_cowork(&e.a, &e.b, &id1(), "move");
        assert!(r.ok, "{r:?}");
        assert!(r.warnings.is_empty(), "{r:?}");
        assert!(!e.rec_a.join(format!("{}.json", id1())).exists());
        assert!(!e.folder1.exists());
        assert!(e.rec_a.join("spaces.json").exists(), "account-wide files stay");
        assert!(rec_b(&e).join(format!("{}.json", r.new_session_id.unwrap())).exists());
    }

    #[test]
    fn move_while_source_claude_runs_keeps_source() {
        let e = setup();
        let from = SessionAccount { running: true, ..e.a.clone() };
        let r = e.api.transfer_cowork(&from, &e.b, &id1(), "move");
        assert!(r.ok, "{r:?}");
        assert!(r.source_running);
        assert_eq!(r.warnings, ["source-left-open"]);
        assert!(e.rec_a.join(format!("{}.json", id1())).exists());
        assert!(e.folder1.join("outputs").join("report.md").exists());
        assert!(rec_b(&e).join(format!("{}.json", r.new_session_id.unwrap())).exists());
    }

    #[test]
    fn target_folder_prefers_existing_cowork_folder() {
        let e = setup();
        let short_dir = e.dir_b.join(ROOT).join(short(ACCT_B)).join(short(ORG_B));
        fs::create_dir_all(&short_dir).unwrap();
        assert_eq!(target_dir(&e.b), Some(short_dir.clone()));
        let r = e.api.transfer_cowork(&e.a, &e.b, &id1(), "copy");
        assert!(r.ok, "{r:?}");
        assert!(short_dir.join(format!("{}.json", r.new_session_id.unwrap())).exists());
        assert!(!rec_b(&e).exists());
        // A Cowork folder without a Code folder is enough.
        fs::remove_dir_all(e.dir_b.join("claude-code-sessions")).unwrap();
        assert_eq!(target_dir(&e.b), Some(short_dir));
    }

    #[test]
    fn failures_leave_nothing_behind() {
        let e = setup();
        // No account or organisation known for the target.
        fs::remove_dir_all(e.dir_b.join("claude-code-sessions")).unwrap();
        let r = e.api.transfer_cowork(&e.a, &e.b, &id1(), "copy");
        assert!(!r.ok);
        assert_eq!(r.reason.as_deref(), Some("target-never-opened"));
        assert!(!e.dir_b.join(ROOT).exists());

        // A failed copy is rolled back, including the folders it created.
        fs::create_dir_all(e.dir_b.join("claude-code-sessions").join(ACCT_B).join(ORG_B)).unwrap();
        FAIL_COPY.with(|f| f.set(true));
        let r = e.api.transfer_cowork(&e.a, &e.b, &id1(), "move");
        FAIL_COPY.with(|f| f.set(false));
        assert!(!r.ok);
        assert_eq!(r.reason.as_deref(), Some("error"));
        assert!(!e.dir_b.join(ROOT).exists(), "created folders removed");
        assert!(e.rec_a.join(format!("{}.json", id1())).exists(), "source kept");

        // Excluded types, unknown ids and same-account moves are refused.
        let sched = "local_66666666-0000-4000-8000-000000000000";
        assert_eq!(e.api.transfer_cowork(&e.a, &e.b, sched, "copy").reason, None);
        assert!(!e.api.transfer_cowork(&e.a, &e.b, sched, "copy").ok);
        assert_eq!(e.api.transfer_cowork(&e.a, &e.b, "local_nope", "copy").reason.as_deref(), Some("not-found"));
        assert_eq!(e.api.transfer_cowork(&e.a, &e.a, &id1(), "move").reason.as_deref(), Some("same-account"));
        assert!(!e.api.transfer_cowork(&e.a, &e.b, "../x", "copy").ok);
    }

    #[test]
    fn delete_removes_record_only_and_waits_for_claude_to_close() {
        let e = setup();
        let open = SessionAccount { running: true, ..e.a.clone() };
        assert_eq!(e.api.delete_cowork(&open, &id1()).unwrap_err().reason, "running");
        assert!(e.rec_a.join(format!("{}.json", id1())).exists());
        e.api.delete_cowork(&e.a, &id1()).unwrap();
        assert!(!e.rec_a.join(format!("{}.json", id1())).exists());
        assert!(e.folder1.exists(), "session folder stays");
        assert_eq!(e.api.delete_cowork(&e.a, &id1()).unwrap_err().reason, "not-found");
        assert_eq!(e.api.delete_cowork(&e.a, "bad").unwrap_err().reason, "invalid");
    }
}
