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
//! second start asks the first one to show its panel over the hook socket. The hook helpers never
//! take the lock.

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

/// The lock file of the data folder `dir`.
fn lock_path_in(dir: &Path) -> PathBuf {
    dir.join(brand::LOCK_FILE)
}

/// The lock file at `path` opened and locked; `Ok(None)` while another holder has it. With
/// `create`, the folder is set up and checked as for the socket and the file is created (0600).
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

/// [`SingleInstance::acquire`] for the lock file at `path`.
fn acquire_at(path: &Path) -> Option<Guard> {
    match try_lock(path, true) {
        Ok(Some(f)) => Some(Guard { _file: Some(f) }),
        Ok(None) => None,
        Err(e) => {
            log::warn(&format!("single-instance lock: {e}"));
            Some(Guard { _file: None })
        }
    }
}

/// [`SingleInstance::notify_existing`] for the instance serving `endpoint`.
fn notify_at(endpoint: &str) -> bool {
    match pipe::client_show_at(endpoint, Instant::now() + SHOW_DEADLINE) {
        Ok(()) => true,
        Err(e) => {
            log::warn(&format!("single-instance: the running instance did not take the show request ({e})"));
            false
        }
    }
}

/// [`SingleInstance::running`] for the lock file at `path`.
fn running_at(path: &Path) -> bool {
    matches!(try_lock(path, false), Ok(None))
}

impl SingleInstance for Os {
    type Guard = Guard;

    fn acquire() -> Option<Guard> {
        acquire_at(&lock_path_in(&paths::user_data()))
    }

    fn notify_existing() -> bool {
        notify_at(&pipe::endpoint())
    }

    /// The lock is held by some process; the file is never created here.
    fn running() -> bool {
        running_at(&lock_path_in(&paths::user_data()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::macos::ipc;
    use crate::platform::IpcTransport;
    use std::fs;
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::thread;

    /// Set for the child process of [`a_second_process_hands_off_to_the_first`]: the data folder
    /// it starts on.
    const CHILD_DIR: &str = "SESSIONMUX_TEST_SECOND_INSTANCE_DIR";
    /// The child's test, by its full name.
    const CHILD_TEST: &str = "platform::macos::instance::tests::second_process";

    #[test]
    fn the_lock_is_exclusive_while_held() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("sub").join(brand::LOCK_FILE);
        assert!(try_lock(&path, false).is_err(), "never created without asking");
        assert!(!running_at(&path));
        let held = try_lock(&path, true).unwrap().expect("a fresh lock is free");
        assert!(matches!(try_lock(&path, true), Ok(None)));
        assert!(matches!(try_lock(&path, false), Ok(None)));
        assert!(running_at(&path));
        assert!(acquire_at(&path).is_none());
        drop(held);
        assert!(!running_at(&path));
        assert!(matches!(try_lock(&path, false), Ok(Some(_))));
        let mode = fs::metadata(d.path().join("sub")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn a_lock_in_a_folder_that_is_not_private_runs_unguarded() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let guard = acquire_at(&lock_path_in(&link)).expect("the app still starts");
        assert!(guard._file.is_none());
        assert!(!real.join(brand::LOCK_FILE).exists(), "nothing is created through the link");
    }

    #[test]
    fn nobody_running_takes_no_show_request() {
        let d = tempfile::tempdir().unwrap();
        assert!(!notify_at(&ipc::endpoint_in(d.path())));
        assert!(!notify_at(""));
    }

    /// The second process of [`a_second_process_hands_off_to_the_first`]; does nothing in a
    /// normal test run. Exits 0 when the lock was taken by the first process and the show request
    /// was handed over, 3 when the hand-off failed and 4 when the lock was free.
    #[test]
    fn second_process() {
        let Some(dir) = std::env::var_os(CHILD_DIR) else { return };
        let dir = PathBuf::from(dir);
        let code = match acquire_at(&lock_path_in(&dir)) {
            None if notify_at(&ipc::endpoint_in(&dir)) => 0,
            None => 3,
            Some(_) => 4,
        };
        std::process::exit(code);
    }

    #[test]
    fn a_second_process_hands_off_to_the_first() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join(brand::DATA_DIR_NAME);
        let guard = acquire_at(&lock_path_in(&dir)).expect("the first start takes the lock");
        assert!(guard._file.is_some());
        let mut listener = <ipc::Os as IpcTransport>::listen(&ipc::endpoint_in(&dir)).unwrap();

        // The first instance takes one connection and reports the line it carried.
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let Ok(Some(mut conn)) = <ipc::Os as IpcTransport>::accept(&mut listener) else { return };
            let mut line = Vec::new();
            let mut byte = [0u8; 1];
            while matches!(conn.read(&mut byte), Ok(1)) {
                line.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            let _ = tx.send(line);
        });

        let child = Command::new(std::env::current_exe().unwrap())
            .args([CHILD_TEST, "--exact", "--nocapture", "--test-threads=1"])
            .env(CHILD_DIR, &dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let line = rx.recv_timeout(Duration::from_secs(10)).expect("the second process connected");
        assert_eq!(line, b"{\"kind\":\"show\"}\n");
        assert_eq!(
            out.status.code(),
            Some(0),
            "second process: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        drop(guard);
    }

    /// Set by the two-user CI job: a folder both users can write, where each run leaves its
    /// socket path and a done marker for the other.
    const TWO_USER_DIR: &str = "SESSIONMUX_TWO_USER_DIR";

    /// Wait up to two minutes for `f` to give a value.
    fn wait_for<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
        let until = Instant::now() + Duration::from_secs(120);
        loop {
            if let Some(v) = f() {
                return v;
            }
            assert!(Instant::now() < until, "gave up waiting for {what}");
            thread::sleep(Duration::from_millis(200));
        }
    }

    /// The other user's file in `shared` ending in `suffix`.
    fn other_users_file(shared: &Path, me: &str, suffix: &str) -> Option<PathBuf> {
        fs::read_dir(shared).ok()?.filter_map(|e| e.ok().map(|e| e.path())).find(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            name.ends_with(suffix) && name != format!("{me}{suffix}")
        })
    }

    #[test]
    #[ignore = "needs a second macOS user running this test at the same time (the two-user CI job)"]
    fn two_users_keep_their_own_lock_and_socket() {
        use crate::core::pipe::Server;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let shared = PathBuf::from(std::env::var_os(TWO_USER_DIR).expect("SESSIONMUX_TWO_USER_DIR is set"));
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() }.to_string();

        let guard = Os::acquire().expect("this user's lock is free");
        assert!(guard._file.is_some(), "this user's lock is set up");
        let shows = Arc::new(AtomicUsize::new(0));
        let counted = shows.clone();
        let server = Server::start_with_show(
            Box::new(|_, _| {}),
            Box::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .expect("this user's socket is served");
        let mine = pipe::endpoint();
        fs::write(shared.join(format!("{me}.endpoint")), &mine).unwrap();

        let theirs = wait_for("the other user's socket", || other_users_file(&shared, &me, ".endpoint"));
        let theirs = fs::read_to_string(theirs).unwrap();
        assert_ne!(theirs, mine);
        let soon = Instant::now() + Duration::from_secs(2);
        assert!(<ipc::Os as IpcTransport>::connect(&theirs, soon).is_err(), "the other user's socket is out of reach");
        assert!(std::os::unix::net::UnixStream::connect(&theirs).is_err());
        assert!(!notify_at(&theirs));

        assert!(Os::notify_existing(), "this user's show reaches this user's instance");
        wait_for("the show request", || (shows.load(Ordering::SeqCst) >= 1).then_some(()));

        fs::write(shared.join(format!("{me}.done")), b"").unwrap();
        wait_for("the other user to finish", || other_users_file(&shared, &me, ".done"));
        thread::sleep(Duration::from_secs(1));
        assert_eq!(shows.load(Ordering::SeqCst), 1, "only this user's show arrived");
        server.stop();
        drop(guard);
    }
}
