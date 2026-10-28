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

//! Processes. Only the liveness check works yet; listing Claude and reading other processes'
//! details report nothing.

use crate::core::claude_app::{Located, Proc};
use crate::platform::{Os, Pid, Processes};

impl Processes for Os {
    fn list_claude(_l: Option<&Located>) -> Vec<Proc> {
        Vec::new()
    }

    /// `kill(pid, 0)`: it exists when the call works or is refused for lack of permission.
    fn alive(pid: Pid) -> bool {
        let Ok(p) = libc::pid_t::try_from(pid) else { return false };
        if p <= 0 {
            return false;
        }
        // SAFETY: signal 0 only checks whether the process exists; nothing is delivered.
        if unsafe { libc::kill(p, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    fn start_time(_pid: Pid) -> Option<u64> {
        None
    }

    fn parent(_pid: Pid) -> Option<Pid> {
        None
    }

    fn exe(_pid: Pid) -> Option<String> {
        None
    }

    fn argv(_pid: Pid) -> Option<Vec<String>> {
        None
    }

    fn processes_under(_dir: &str) -> Vec<(Pid, String)> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alive_rules() {
        assert!(Os::alive(std::process::id()));
        assert!(Os::alive(1)); // launchd: refused, but it exists
        assert!(!Os::alive(0));
        assert!(!Os::alive(u32::MAX));
    }

    #[test]
    fn details_read_as_unknown() {
        let me = std::process::id();
        assert!(Os::list_claude(None).is_empty());
        assert_eq!(Os::start_time(me), None);
        assert_eq!(Os::parent(me), None);
        assert_eq!(Os::exe(me), None);
        assert_eq!(Os::argv(me), None);
        assert!(Os::processes_under("/").is_empty());
    }
}
