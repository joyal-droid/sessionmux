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

//! Panel IPC message types.
//!
//! API:
//! ```ignore
//! pub trait IpcHandler: Send+Sync { fn handle(&self, req: IpcRequest); }   // called on the UI thread
//! pub struct IpcRequest { pub id:u64, pub name:String, pub args:Vec<serde_json::Value> }
//! pub fn parse_request(json:&str) -> Option<IpcRequest>   // reject >64 KB, unknown shapes
//! ```
//!
//! Wire format (web/shim.js): `{"id":<u64>,"name":"<method>","args":[...]}`. `id` and `args` are
//! optional (default 0 / `[]`); any other key, a non-object, a non-identifier name or an id that
//! is not a non-negative integer is rejected.
//!
//! Requests with id 0 are notifications that expect no reply. Besides the `window.sessionmux` method
//! names, the handler receives these internal names:
//! - `__ready`  — the page's DOM is ready and panel.js has subscribed (sent once per load);
//! - `__error`  — args `[message, source, line, col, stack]` from window.onerror /
//!   unhandledrejection / console.error (already logged by ui::panel);
//! - `__blur`   — synthesised by ui::panel when the panel window loses activation while visible;
//! - `hidePanel` (id 0) — synthesised by ui::panel on WM_CLOSE (Alt+F4);
//! - `openExternal` (id 0, args `[url]`) — synthesised when the page tries to open a new window.

/// Receives every request the page sends (`window.sessionmux.*` invocations and `__ready`).
pub trait IpcHandler: Send + Sync {
    fn handle(&self, req: IpcRequest);
}

#[derive(Clone, Debug, PartialEq)]
pub struct IpcRequest {
    pub id: u64,
    pub name: String,
    pub args: Vec<serde_json::Value>,
}

/// Largest accepted message (bytes of JSON text).
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    name.len() <= 64 && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Parse one message from the page. Returns None for oversize or malformed input.
pub fn parse_request(json: &str) -> Option<IpcRequest> {
    if json.len() > MAX_MESSAGE_BYTES {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let obj = value.as_object()?;
    if obj.keys().any(|k| k != "id" && k != "name" && k != "args") {
        return None;
    }
    let name = obj.get("name")?.as_str()?;
    if !valid_name(name) {
        return None;
    }
    let id = match obj.get("id") {
        None | Some(serde_json::Value::Null) => 0,
        Some(v) => v.as_u64()?,
    };
    let args = match obj.get("args") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(a)) => a.clone(),
        Some(_) => return None,
    };
    Some(IpcRequest { id, name: name.to_string(), args })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_invocations() {
        let r = parse_request(r#"{"id":7,"name":"openAccount","args":["abc"]}"#).unwrap();
        assert_eq!(r, IpcRequest { id: 7, name: "openAccount".into(), args: vec![json!("abc")] });
        let r = parse_request(r#"{"name":"__ready"}"#).unwrap();
        assert_eq!(r, IpcRequest { id: 0, name: "__ready".into(), args: vec![] });
        assert!(parse_request(r#"{"id":1,"name":"setSettings","args":[{"theme":"dark"}]"#).is_none(), "truncated JSON");
        let r =
            parse_request(r#"{"id":2,"name":"transferSession","args":[{"from":"a","to":"b","mode":"copy"}]}"#).unwrap();
        assert_eq!(r.args[0]["mode"], json!("copy"));
        let r = parse_request(r#"{"id":3,"name":"reorderAccounts","args":[["a","b"]]}"#).unwrap();
        assert_eq!(r.args, vec![json!(["a", "b"])]);
        let r = parse_request(r#"{"id":null,"name":"quit","args":null}"#).unwrap();
        assert_eq!((r.id, r.args.len()), (0, 0));
    }

    #[test]
    fn rejects_bad_shapes() {
        for bad in [
            "",
            "null",
            "[]",
            "\"listAccounts\"",
            r#"{}"#,
            r#"{"id":1}"#,
            r#"{"id":1,"name":""}"#,
            r#"{"id":1,"name":5}"#,
            r#"{"id":-1,"name":"x"}"#,
            r#"{"id":1.5,"name":"x"}"#,
            r#"{"id":"1","name":"x"}"#,
            r#"{"id":1,"name":"x","args":{}}"#,
            r#"{"id":1,"name":"x","args":"a"}"#,
            r#"{"id":1,"name":"x","extra":true}"#,
            r#"{"id":1,"name":"bad name"}"#,
            r#"{"id":1,"name":"1abc"}"#,
            r#"{"id":1,"name":"a.b"}"#,
        ] {
            assert!(parse_request(bad).is_none(), "accepted {bad:?}");
        }
        let long_name = format!(r#"{{"id":1,"name":"{}"}}"#, "a".repeat(65));
        assert!(parse_request(&long_name).is_none());
    }

    #[test]
    fn rejects_oversize() {
        let big = "x".repeat(MAX_MESSAGE_BYTES);
        let msg = format!(r#"{{"id":1,"name":"addAccount","args":["{big}"]}}"#);
        assert!(msg.len() > MAX_MESSAGE_BYTES);
        assert!(parse_request(&msg).is_none());
        let ok = format!(r#"{{"id":1,"name":"addAccount","args":["{}"]}}"#, "x".repeat(1000));
        assert!(parse_request(&ok).is_some());
    }
}
