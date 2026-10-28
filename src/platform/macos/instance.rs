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

//! Single instance: an exclusive `flock` on `instance.lock` in Sessionmux's data folder, held for
//! the life of the process (the system releases it when the process ends, also on a crash). A
//! second start asks the first one to show its panel over the hook socket.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::brand;
use crate::core::pipe;
use crate::platform::SingleInstance;
use crate::util::{log, paths};

/// How long a second start waits for the running instance to take the show request.
const SHOW_DEADLINE: Duration = Duration::from_secs(2);

/// The lock file backend.
pub struct Os;

/// The held lock; empty when the lock file could not be set up (the app then runs unguarded).
pub struct Guard {
    _file: Option<File>,
}

fn lock_path() -> PathBuf {
    paths::user_data().join(brand::LOCK_FILE)
}

/// The lock file at `path` opened and locked; `Ok(None)` while another holder has it.
fn try_lock(path: &Path, create: bool) -> io::Result<Option<File>> {
    if create {
        if let Some(dir) = path.parent() {
            super::ipc::secure_dir(dir)?;
        }
    }
    let file = OpenOptions::new().read(true).write(true).create(create).truncate(false).mode(0o600).open(path)?;
    // SAFETY: the descriptor belongs to `file`, which is alive; flock does not touch memory.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(Some(file));
    }
    let e = io::Error::last_os_error();
    if e.raw_os_error() == Some(libc::EWOULDBLOCK) {
        return Ok(None);
    }
    Err(e)
}

impl SingleInstance for Os {
    type Guard = Guard;

    fn acquire() -> Option<Guard> {
        match try_lock(&lock_path(), true) {
            Ok(Some(f)) => Some(Guard { _file: Some(f) }),
            Ok(None) => None,
            Err(e) => {
                log::warn(&format!("single-instance lock: {e}"));
                Some(Guard { _file: None })
            }
        }
    }

    fn notify_existing() -> bool {
        pipe::client_show(Instant::now() + SHOW_DEADLINE).is_ok()
    }

    /// The lock is held by some process; the file is never created here.
    fn running() -> bool {
        matches!(try_lock(&lock_path(), false), Ok(None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_lock_is_exclusive_while_held() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("sub").join(brand::LOCK_FILE);
        assert!(try_lock(&path, false).is_err(), "never created without asking");
        let held = try_lock(&path, true).unwrap().expect("a fresh lock is free");
        assert!(matches!(try_lock(&path, true), Ok(None)));
        assert!(matches!(try_lock(&path, false), Ok(None)));
        drop(held);
        assert!(matches!(try_lock(&path, false), Ok(Some(_))));
        let mode = fs::metadata(d.path().join("sub")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
}
