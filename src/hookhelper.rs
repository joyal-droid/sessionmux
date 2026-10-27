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

//! The Claude Code hook helpers: `Sessionmux.exe --notify-hook`, `--approve-hook` and
//! `--remove-hooks`.
//!
//! API:
//! ```ignore
//! pub fn run_notify_hook() -> i32
//! pub fn run_approve_hook() -> i32
//! pub fn run_remove_hooks() -> i32
//! ```
//!
//! Each runs without UI, COM or the single-instance mutex and always returns 0. A helper that is
//! unsure (unreadable input, the wrong event, no running app, a pipe error, a panic) prints
//! nothing, so Claude's own prompt decides; nothing is ever approved without the user.
//!
//! Input is the hook's JSON on stdin, at most 64 KB, read within 5 s. `hook_event_name` must be
//! `PermissionRequest` for `--approve-hook` and `Notification` or `StopFailure` for
//! `--notify-hook`. Each helper has one time budget counted from its start, shared by reading
//! stdin, connecting and waiting for the app, so it always ends before Claude Code's time-out:
//! 85 s for the approval helper (90 s time-out), 4 s for the notification helper (5 s). The
//! approval helper prints at most one `hookSpecificOutput` object on stdout; it never sends `updatedInput` or
//! `updatedPermissions`. Nothing else is ever written to stdout or stderr. The log names the tool
//! and the outcome only, never the tool input or a message text; a helper that cannot connect
//! to the app (not running, the pipe stayed busy, not served by this user) logs one line.

use crate::brand;
use crate::core::hooks;
use crate::core::pipe::{self, Decision, PipeError, Request};
use crate::util::{log, paths};
use serde_json::Value;
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Largest hook input read from stdin.
const MAX_INPUT: usize = 64 * 1024;
/// The hook input must arrive within this.
const READ_DEADLINE: Duration = Duration::from_secs(5);
/// The approval helper's whole run, from start to answer (Claude Code gives it 90 s).
const APPROVE_BUDGET: Duration = Duration::from_secs(85);
/// The notification helper's whole run (Claude Code gives it 5 s).
const NOTIFY_BUDGET: Duration = Duration::from_secs(4);
/// Largest message text forwarded by the notification helper.
const MAX_TEXT: usize = 2 * 1024;
/// Largest input summary of an approve request.
const MAX_SUMMARY: usize = 4 * 1024;
/// Characters of the input JSON shown for tools without a dedicated summary.
const JSON_HEAD: usize = 512;
/// Largest user note carried by a deny.
const MAX_NOTE: usize = 1024;

/// Which helper is reading the input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Notify,
    Approve,
}

/// `--notify-hook`: forward one `Notification` / `StopFailure` event to the running app.
pub fn run_notify_hook() -> i32 {
    let deadline = Instant::now() + NOTIFY_BUDGET;
    guarded(|| {
        log::init(&paths::log_file());
        let Some(input) = read_stdin(deadline) else { return };
        let Some(req) = notify_request(&input, now_secs()) else { return };
        match pipe::client_send_notify(&req, deadline) {
            Ok(()) => {}
            Err(e) if e.is_connect() => log_no_connect("notification", &e),
            Err(e) => log::warn(&format!("hook: notification not delivered ({e})")),
        }
    })
}

/// `--approve-hook`: forward one `PermissionRequest` and print the user's decision, if any.
pub fn run_approve_hook() -> i32 {
    let deadline = Instant::now() + APPROVE_BUDGET;
    guarded(|| {
        log::init(&paths::log_file());
        let Some(input) = read_stdin(deadline) else { return };
        if let Some(out) = approve_with(&input, &pipe::pipe_name(), deadline) {
            let mut so = std::io::stdout().lock();
            let _ = so.write_all(out.as_bytes());
            let _ = so.flush();
        }
    })
}

/// `--remove-hooks` (run by the uninstaller): take our entries out of the user's settings file.
pub fn run_remove_hooks() -> i32 {
    guarded(|| {
        log::init(&paths::log_file());
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                log::warn(&format!("hooks: --remove-hooks: cannot tell our own path ({e})"));
                return;
            }
        };
        match hooks::remove(&hooks::user_settings_path(), &exe) {
            Ok(r) => log::info(&format!("hooks: --remove-hooks removed {} entries", r.removed)),
            Err(e) => log::warn(&format!("hooks: --remove-hooks: {e}")),
        }
    })
}

/// Run `f` so that a panic ends the helper quietly with exit code 0. Release builds abort on
/// panic, so the panic hook itself leaves the process there; elsewhere the unwind is caught.
fn guarded(f: impl FnOnce()) -> i32 {
    std::panic::set_hook(Box::new(|_| {
        if cfg!(panic = "abort") {
            std::process::exit(0);
        }
    }));
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    0
}

fn now_secs() -> u64 {
    log::now_millis() / 1000
}

/// The whole of stdin, when it is at most [`MAX_INPUT`] bytes and arrives within
/// [`READ_DEADLINE`] and before `deadline`.
fn read_stdin(deadline: Instant) -> Option<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("hook-stdin".into())
        .spawn(move || {
            let mut buf = Vec::new();
            let r = std::io::stdin().lock().take(MAX_INPUT as u64 + 1).read_to_end(&mut buf);
            let _ = tx.send(r.ok().map(|_| buf));
        })
        .ok()?;
    let wait = deadline.saturating_duration_since(Instant::now()).min(READ_DEADLINE);
    let buf = rx.recv_timeout(wait).ok()??;
    (buf.len() <= MAX_INPUT).then_some(buf)
}

/// The hook input as a JSON object whose `hook_event_name` fits `mode`.
fn parse_input(bytes: &[u8], mode: Mode) -> Option<serde_json::Map<String, Value>> {
    if bytes.len() > MAX_INPUT {
        return None;
    }
    let Value::Object(map) = serde_json::from_slice::<Value>(bytes).ok()? else { return None };
    let event = map.get("hook_event_name")?.as_str()?;
    let fits = match mode {
        Mode::Approve => event == "PermissionRequest",
        Mode::Notify => event == "Notification" || event == "StopFailure",
    };
    fits.then_some(map)
}

fn text<'a>(map: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    map.get(key).and_then(Value::as_str)
}

/// The notify request for one hook input, or `None` when the input does not fit.
fn notify_request(bytes: &[u8], time: u64) -> Option<Request> {
    let map = parse_input(bytes, Mode::Notify)?;
    let event = text(&map, "hook_event_name")?.to_string();
    let session_id = clip(text(&map, "session_id")?, 256).to_string();
    let cwd = clip(text(&map, "cwd").unwrap_or(""), MAX_TEXT).to_string();
    let (notification_type, message) = if event == "StopFailure" {
        let message = ["last_assistant_message", "error_details", "message"]
            .iter()
            .find_map(|k| text(&map, k).filter(|s| !s.trim().is_empty()))
            .unwrap_or("");
        (text(&map, "error"), message)
    } else {
        (text(&map, "notification_type"), text(&map, "message").unwrap_or(""))
    };
    Some(Request::Notify {
        event,
        notification_type: notification_type.map(|s| clip(s, 128).to_string()),
        session_id,
        cwd,
        message: clip(message, MAX_TEXT).to_string(),
        time,
    })
}

/// The approve request for one hook input, or `None` when the input does not fit.
fn approve_request(bytes: &[u8], time: u64, request_id: String) -> Option<Request> {
    let map = parse_input(bytes, Mode::Approve)?;
    let tool_name = clip(text(&map, "tool_name")?, 256).to_string();
    if tool_name.is_empty() {
        return None;
    }
    let session_id = clip(text(&map, "session_id")?, 256).to_string();
    let cwd = clip(text(&map, "cwd").unwrap_or(""), MAX_TEXT).to_string();
    let input = map.get("tool_input").cloned().unwrap_or(Value::Null);
    Some(Request::Approve {
        request_id,
        session_id,
        cwd,
        mcp_server: mcp_parts(&tool_name).map(|(s, _)| s.to_string()),
        summary: summarize(&tool_name, &input),
        tool_name,
        time,
    })
}

/// Send the approve request built from `bytes` to the pipe `name` and wait until `deadline` at
/// the latest; the text to print on stdout, or `None` to print nothing.
fn approve_with(bytes: &[u8], name: &str, deadline: Instant) -> Option<String> {
    let req = approve_request(bytes, now_secs(), uuid::Uuid::new_v4().simple().to_string())?;
    let Request::Approve { tool_name, .. } = &req else { return None };
    let tool = log_name(tool_name);
    let decision = match pipe::client_approve_at(name, &req, deadline) {
        Ok(d) => d,
        Err(e) if e.is_connect() => {
            log_no_connect(&format!("approval for {tool}"), &e);
            return None;
        }
        Err(e) => {
            log::warn(&format!("hook: approval for {tool} not delivered ({e})"));
            return None;
        }
    };
    let outcome = match &decision {
        Some(Decision::Allow) => "allowed",
        Some(Decision::Deny { .. }) => "denied",
        None => "left to Claude",
    };
    log::info(&format!("hook: approval for {tool} {outcome}"));
    decision.as_ref().map(decision_json)
}

/// The one log line of a helper that could not reach the app; it carries no hook input.
fn log_no_connect(what: &str, e: &PipeError) {
    match e {
        PipeError::NotRunning => log::info(&format!("hook: {what} not sent: Sessionmux is not running")),
        _ => log::warn(&format!("hook: {what} not sent: could not connect to Sessionmux ({e})")),
    }
}

/// A tool name fit for the log: letters, digits, `_`, `-` and `.`, at most 64 characters.
fn log_name(tool: &str) -> String {
    tool.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')).take(64).collect()
}

/// The exact stdout of the approval helper for `decision`.
fn decision_json(decision: &Decision) -> String {
    match decision {
        Decision::Allow => {
            r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}"#
                .to_string()
        }
        Decision::Deny { message } => {
            let base = format!("Denied from {}", brand::NAME);
            let text = match message.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
                Some(note) => format!("{base}: {}", clip(note, MAX_NOTE)),
                None => base,
            };
            let quoted = serde_json::to_string(&text).unwrap_or_else(|_| format!("\"Denied from {}\"", brand::NAME));
            format!(
                r#"{{"hookSpecificOutput":{{"hookEventName":"PermissionRequest","decision":{{"behavior":"deny","message":{quoted}}}}}}}"#
            )
        }
    }
}

/// `(server, tool)` of an MCP tool name `mcp__<server>__<tool>`.
fn mcp_parts(tool: &str) -> Option<(&str, &str)> {
    let rest = tool.strip_prefix("mcp__")?;
    let (server, name) = rest.split_once("__")?;
    (!server.is_empty() && !name.is_empty()).then_some((server, name))
}

/// The first [`JSON_HEAD`] characters of `input` as compact JSON, with `…` when cut.
fn json_head(input: &Value) -> String {
    let s = serde_json::to_string(input).unwrap_or_default();
    let mut out: String = s.chars().take(JSON_HEAD).collect();
    if out.len() < s.len() {
        out.push('…');
    }
    out
}

/// What the tool is about to do, at most [`MAX_SUMMARY`] bytes: the command of a shell tool, the
/// paths of a file tool (with the old and new string lengths of an edit), the URL or query of a
/// web tool, the server, tool and start of the input of an MCP tool, else the start of the input.
fn summarize(tool: &str, input: &Value) -> String {
    let st = |k: &str| input.get(k).and_then(Value::as_str);
    let chars = |k: &str| st(k).map(|s| s.chars().count()).unwrap_or(0);
    let lines = |pairs: &[(&str, Option<&str>)]| {
        let v: Vec<String> = pairs.iter().filter_map(|(k, s)| s.map(|s| format!("{k}: {s}"))).collect();
        (!v.is_empty()).then(|| v.join("\n"))
    };
    let text = if let Some((server, name)) = mcp_parts(tool) {
        Some(format!("server: {server}\ntool: {name}\n{}", json_head(input)))
    } else {
        match tool {
            "Bash" | "PowerShell" => st("command").map(str::to_string),
            "Read" | "Write" => st("file_path").map(str::to_string),
            "Edit" => st("file_path").map(|p| {
                format!("{p}\nold: {} characters, new: {} characters", chars("old_string"), chars("new_string"))
            }),
            "MultiEdit" => st("file_path").map(|p| {
                let n = input.get("edits").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
                format!("{p}\nedits: {n}")
            }),
            "NotebookEdit" => st("notebook_path").map(str::to_string),
            "Glob" | "Grep" => lines(&[("pattern", st("pattern")), ("path", st("path"))]),
            "WebFetch" => st("url").map(str::to_string),
            "WebSearch" => st("query").map(str::to_string),
            _ => None,
        }
    };
    bounded(&text.unwrap_or_else(|| json_head(input)), MAX_SUMMARY)
}

/// `s` cut to at most `max` bytes, ending in `…` when cut.
fn bounded(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut out = clip(s, max - '…'.len_utf8()).to_string();
    out.push('…');
    out
}

/// `s` cut to at most `max` bytes on a character boundary.
fn clip(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::pipe::Server;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    const SID: &str = "0d2a6f5e-6a3c-4c1e-9a43-7f1d0c2b9e11";

    fn permission(tool: &str, input: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "session_id": SID,
            "transcript_path": "x.jsonl",
            "cwd": r"C:\work\demo",
            "permission_mode": "default",
            "hook_event_name": "PermissionRequest",
            "tool_name": tool,
            "tool_input": input,
        }))
        .unwrap()
    }

    fn approve_parts(req: &Request) -> (&str, &str, Option<&str>, &str) {
        match req {
            Request::Approve { session_id, tool_name, mcp_server, summary, .. } => {
                (session_id, tool_name, mcp_server.as_deref(), summary)
            }
            _ => panic!("not an approve request"),
        }
    }

    #[test]
    fn approve_input_parsing() {
        let req = approve_request(&permission("Bash", json!({ "command": "cargo test" })), 7, "ab".repeat(16)).unwrap();
        assert_eq!(approve_parts(&req), (SID, "Bash", None, "cargo test"));
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["kind"], "approve");
        assert_eq!(v["requestId"], "ab".repeat(16));
        assert_eq!(v["cwd"], r"C:\work\demo");
        assert_eq!(v["time"], 7);

        let req = approve_request(&permission("mcp__files__read_file", json!({ "path": "a.txt" })), 0, "c".repeat(32));
        let req = req.unwrap();
        let (_, tool, server, summary) = approve_parts(&req);
        assert_eq!((tool, server), ("mcp__files__read_file", Some("files")));
        assert_eq!(summary, "server: files\ntool: read_file\n{\"path\":\"a.txt\"}");

        // The wrong event, a broken document, a missing tool or session, oversized input.
        let mut wrong: Value = serde_json::from_slice(&permission("Bash", json!({}))).unwrap();
        wrong["hook_event_name"] = json!("PreToolUse");
        assert!(approve_request(&serde_json::to_vec(&wrong).unwrap(), 0, String::new()).is_none());
        wrong["hook_event_name"] = json!("Notification");
        assert!(approve_request(&serde_json::to_vec(&wrong).unwrap(), 0, String::new()).is_none());
        assert!(approve_request(b"{\"hook_event_name\":\"PermissionRequest\"", 0, String::new()).is_none());
        assert!(approve_request(b"[1,2]", 0, String::new()).is_none());
        assert!(approve_request(b"", 0, String::new()).is_none());
        let no_tool = json!({ "hook_event_name": "PermissionRequest", "session_id": SID });
        assert!(approve_request(&serde_json::to_vec(&no_tool).unwrap(), 0, String::new()).is_none());
        let no_session = json!({ "hook_event_name": "PermissionRequest", "tool_name": "Bash" });
        assert!(approve_request(&serde_json::to_vec(&no_session).unwrap(), 0, String::new()).is_none());
        let big = permission("Bash", json!({ "command": "x".repeat(MAX_INPUT) }));
        assert!(approve_request(&big, 0, String::new()).is_none());
    }

    #[test]
    fn notify_input_parsing() {
        let n = json!({
            "session_id": SID, "cwd": r"C:\work\demo", "hook_event_name": "Notification",
            "notification_type": "idle_prompt", "message": "Claude is waiting for your input", "title": "Claude Code",
        });
        let req = notify_request(&serde_json::to_vec(&n).unwrap(), 9).unwrap();
        assert_eq!(
            req,
            Request::Notify {
                event: "Notification".into(),
                notification_type: Some("idle_prompt".into()),
                session_id: SID.into(),
                cwd: r"C:\work\demo".into(),
                message: "Claude is waiting for your input".into(),
                time: 9,
            }
        );

        let s = json!({
            "session_id": SID, "cwd": "C:\\w", "hook_event_name": "StopFailure", "error": "rate_limit",
            "error_details": "429 Too Many Requests",
            "last_assistant_message": format!("You've hit your session limit · resets 3:45pm{}", "!".repeat(5000)),
        });
        let Request::Notify { event, notification_type, message, .. } =
            notify_request(&serde_json::to_vec(&s).unwrap(), 0).unwrap()
        else {
            panic!()
        };
        assert_eq!((event.as_str(), notification_type.as_deref()), ("StopFailure", Some("rate_limit")));
        assert!(message.starts_with("You've hit your session limit · resets 3:45pm"));
        assert_eq!(message.len(), MAX_TEXT);

        let s = json!({ "session_id": SID, "hook_event_name": "StopFailure", "error": "rate_limit", "error_details": "429" });
        let Request::Notify { message, cwd, .. } = notify_request(&serde_json::to_vec(&s).unwrap(), 0).unwrap() else {
            panic!()
        };
        assert_eq!((message.as_str(), cwd.as_str()), ("429", ""));

        let p = permission("Bash", json!({}));
        assert!(notify_request(&p, 0).is_none());
        assert!(notify_request(b"not json", 0).is_none());
    }

    #[test]
    fn decision_output_is_exact() {
        assert_eq!(
            decision_json(&Decision::Allow),
            r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}"#
        );
        assert_eq!(
            decision_json(&Decision::Deny { message: None }),
            r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny","message":"Denied from Sessionmux"}}}"#
        );
        assert_eq!(
            decision_json(&Decision::Deny { message: Some("  ".into()) }),
            decision_json(&Decision::Deny { message: None })
        );
        let out = decision_json(&Decision::Deny { message: Some("use \"cargo check\"\nfirst".into()) });
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["hookSpecificOutput"]["decision"]["message"],
            "Denied from Sessionmux: use \"cargo check\"\nfirst"
        );
        assert!(v["hookSpecificOutput"]["decision"].get("updatedInput").is_none());
    }

    #[test]
    fn summary_per_tool_family() {
        assert_eq!(summarize("Bash", &json!({ "command": "ls -la", "description": "list" })), "ls -la");
        assert_eq!(summarize("PowerShell", &json!({ "command": "Get-ChildItem" })), "Get-ChildItem");
        assert_eq!(summarize("Read", &json!({ "file_path": r"C:\a\b.rs" })), r"C:\a\b.rs");
        assert_eq!(summarize("Write", &json!({ "file_path": r"C:\a\c.rs", "content": "secret" })), r"C:\a\c.rs");
        assert_eq!(
            summarize("Edit", &json!({ "file_path": "x.rs", "old_string": "abc", "new_string": "✓✓" })),
            "x.rs\nold: 3 characters, new: 2 characters"
        );
        assert_eq!(summarize("MultiEdit", &json!({ "file_path": "x.rs", "edits": [{}, {}] })), "x.rs\nedits: 2");
        assert_eq!(summarize("NotebookEdit", &json!({ "notebook_path": "n.ipynb" })), "n.ipynb");
        assert_eq!(summarize("Glob", &json!({ "pattern": "**/*.rs", "path": "src" })), "pattern: **/*.rs\npath: src");
        assert_eq!(summarize("Grep", &json!({ "pattern": "fn main" })), "pattern: fn main");
        assert_eq!(
            summarize("WebFetch", &json!({ "url": "https://example.com/", "prompt": "p" })),
            "https://example.com/"
        );
        assert_eq!(summarize("WebSearch", &json!({ "query": "rust pipes" })), "rust pipes");
        assert_eq!(summarize("Task", &json!({ "a": 1 })), "{\"a\":1}");
        // A known tool without its field falls back to the input JSON.
        assert_eq!(summarize("Bash", &json!({ "cmd": 1 })), "{\"cmd\":1}");
        assert_eq!(summarize("mcp__x", &json!(null)), "null");

        let long = json!({ "data": "z".repeat(2000) });
        let head = summarize("Other", &long);
        assert_eq!(head.chars().count(), JSON_HEAD + 1);
        assert!(head.ends_with('…'));
        let mcp = summarize("mcp__srv__tool", &long);
        assert!(mcp.starts_with("server: srv\ntool: tool\n{\"data\":\"zzz"));
        assert!(mcp.ends_with('…'));
    }

    #[test]
    fn summary_is_bounded_at_4_kb() {
        let s = summarize("Bash", &json!({ "command": "e".repeat(10_000) }));
        assert_eq!(s.len(), MAX_SUMMARY);
        assert!(s.ends_with('…'));
        let s = summarize("Bash", &json!({ "command": "✓".repeat(3000) }));
        assert!(s.len() <= MAX_SUMMARY && s.ends_with('…'));
        let s = summarize("Edit", &json!({ "file_path": "p".repeat(5000), "old_string": "a", "new_string": "b" }));
        assert!(s.len() <= MAX_SUMMARY);
        let exact = "f".repeat(MAX_SUMMARY);
        assert_eq!(summarize("Read", &json!({ "file_path": exact })), exact);
    }

    #[test]
    fn approve_round_trip_through_a_test_server() {
        let name = format!("{}.test-{}", pipe::pipe_name(), uuid::Uuid::new_v4().simple());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let rec = seen.clone();
        let server = Server::start_at(
            &name,
            Box::new(move |req, resp| {
                let deny = matches!(&req, Request::Approve { tool_name, .. } if tool_name == "Write");
                rec.lock().unwrap().push(req);
                std::thread::spawn(move || {
                    let d = if deny { Decision::Deny { message: Some("no".into()) } } else { Decision::Allow };
                    let _ = resp.decide(d);
                });
            }),
        )
        .unwrap();

        let out = approve_with(
            &permission("Bash", json!({ "command": "cargo test" })),
            &name,
            Instant::now() + Duration::from_secs(10),
        );
        assert_eq!(out.as_deref(), Some(decision_json(&Decision::Allow).as_str()));
        let out = approve_with(
            &permission("Write", json!({ "file_path": "a" })),
            &name,
            Instant::now() + Duration::from_secs(10),
        );
        assert_eq!(out, Some(decision_json(&Decision::Deny { message: Some("no".into()) })));
        // The wrong event never reaches the app and prints nothing.
        assert_eq!(
            approve_with(b"{\"hook_event_name\":\"Stop\"}", &name, Instant::now() + Duration::from_secs(1)),
            None
        );

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        let (sid, tool, _, summary) = approve_parts(&seen[0]);
        assert_eq!((sid, tool, summary), (SID, "Bash", "cargo test"));
        drop(seen);
        server.stop();

        // No app: nothing printed.
        assert_eq!(
            approve_with(
                &permission("Bash", json!({ "command": "x" })),
                &name,
                Instant::now() + Duration::from_secs(1)
            ),
            None
        );
    }

    /// The built exe, run as a helper with input that does not fit, exits 0 and prints nothing.
    #[test]
    fn helper_exe_is_silent_on_unfit_input() {
        use std::process::{Command, Stdio};
        let Some(exe) = std::env::current_exe().ok().and_then(|p| Some(p.parent()?.parent()?.join(brand::EXE))) else {
            return;
        };
        if !exe.is_file() {
            return;
        }
        let stop = br#"{"hook_event_name":"Stop","session_id":"x"}"#.to_vec();
        for (arg, input) in [
            ("--approve-hook", stop.clone()),
            ("--notify-hook", stop),
            ("--approve-hook", b"not json".to_vec()),
            ("--notify-hook", vec![b'{'; MAX_INPUT + 10]),
        ] {
            let mut child = Command::new(&exe)
                .arg(arg)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let mut stdin = child.stdin.take().unwrap();
            let _ = stdin.write_all(&input);
            drop(stdin);
            let out = child.wait_with_output().unwrap();
            assert_eq!(out.status.code(), Some(0), "{arg}");
            assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{arg}");
        }
    }

    #[test]
    fn budgets_end_before_the_hook_timeouts() {
        assert!(APPROVE_BUDGET < Duration::from_secs(hooks::APPROVE_TIMEOUT));
        assert!(NOTIFY_BUDGET < Duration::from_secs(hooks::NOTIFY_TIMEOUT));
        // A deadline already past ends the helper at once.
        let name = format!("{}.test-{}", pipe::pipe_name(), uuid::Uuid::new_v4().simple());
        assert_eq!(approve_with(&permission("Bash", json!({ "command": "x" })), &name, Instant::now()), None);
    }

    #[test]
    fn log_names_are_plain() {
        assert_eq!(log_name("mcp__srv__tool"), "mcp__srv__tool");
        assert_eq!(log_name("Bash\n[ERROR] x"), "BashERRORx");
        assert_eq!(log_name(&"a".repeat(100)).len(), 64);
    }
}
