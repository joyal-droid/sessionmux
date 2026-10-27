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

//! JSON file I/O.
//!
//! API:
//! `read<T: DeserializeOwned>(p) -> Option<T>` (missing/unparseable → None; tolerates a UTF-8 BOM);
//! `write_atomic<T: Serialize>(p, &T) -> io::Result<()>` (pretty, 2-space indent like
//! `JSON.stringify(o, null, 2)`; temp file + fsync + rename with retries).

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Read and parse a JSON file; `None` when missing or unparseable.
pub fn read<T: DeserializeOwned>(p: &Path) -> Option<T> {
    let bytes = std::fs::read(p).ok()?;
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(&bytes);
    serde_json::from_slice(body).ok()
}

/// Write `value` as pretty JSON atomically: readers never observe a half-written file.
pub fn write_atomic<T: Serialize>(p: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let data = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    let mut tmp = p.as_os_str().to_owned();
    let rnd = uuid::Uuid::new_v4().simple().to_string();
    tmp.push(format!(".tmp-{}-{}", std::process::id(), &rnd[..8]));
    let tmp = PathBuf::from(tmp);
    let result = (|| {
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(&data)?;
        f.sync_all()?;
        drop(f);
        rename_with_retry(&tmp, p)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Rename over `dst`, retrying briefly: AV scanners / indexers often hold the target open
/// (ERROR_ACCESS_DENIED / SHARING_VIOLATION / LOCK_VIOLATION).
pub fn rename_with_retry(src: &Path, dst: &Path) -> io::Result<()> {
    let mut attempt = 0u64;
    loop {
        match std::fs::rename(src, dst) {
            Ok(()) => return Ok(()),
            Err(e) => {
                let transient = e.kind() == io::ErrorKind::PermissionDenied
                    || matches!(e.raw_os_error(), Some(5) | Some(32) | Some(33));
                if !transient || attempt >= 5 {
                    return Err(e);
                }
                attempt += 1;
                std::thread::sleep(Duration::from_millis(20 * attempt));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn roundtrip_pretty_and_bom() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub").join("a.json");
        write_atomic(&p, &json!({"a": 1, "b": [true]})).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(text, "{\n  \"a\": 1,\n  \"b\": [\n    true\n  ]\n}");
        assert_eq!(read::<Value>(&p).unwrap()["a"], 1);
        write_atomic(&p, &json!({"a": 2})).unwrap();
        assert_eq!(read::<Value>(&p).unwrap()["a"], 2);
        assert_eq!(std::fs::read_dir(p.parent().unwrap()).unwrap().count(), 1, "no temp leftovers");

        let bom = d.path().join("bom.json");
        std::fs::write(&bom, b"\xEF\xBB\xBF{\"x\":3}").unwrap();
        assert_eq!(read::<Value>(&bom).unwrap()["x"], 3);
        assert!(read::<Value>(&d.path().join("missing.json")).is_none());
        std::fs::write(&bom, b"{nope").unwrap();
        assert!(read::<Value>(&bom).is_none());
    }
}
