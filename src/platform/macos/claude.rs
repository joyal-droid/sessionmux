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

//! Finding, starting, focusing and closing Claude Desktop. Not built yet: Claude is reported as
//! not found, and every action fails with a message instead of touching any process.

use super::unsupported;
use crate::core::claude_app::Located;
use crate::platform::{ClaudeInstall, Closer, Focus, FocusResult, Launcher, Os, Pid};

impl ClaudeInstall for Os {
    fn locate() -> Result<Located, String> {
        Err(unsupported("Finding Claude Desktop"))
    }
}

impl Focus for Os {
    fn focus(_pid: Pid) -> FocusResult {
        FocusResult::Failed
    }

    fn accessibility_trusted(_prompt: bool) -> Option<bool> {
        None
    }
}

impl Launcher for Os {
    fn launch_main(_l: &Located) -> Result<Option<Pid>, String> {
        Err(unsupported("Opening Claude Desktop"))
    }

    fn launch(_l: &Located, _data_dir: &str) -> Result<Pid, String> {
        Err(unsupported("Opening Claude Desktop"))
    }

    fn launch_url(_l: &Located, _data_dir: Option<&str>, _url: &str) -> Result<(), String> {
        Err(unsupported("Opening links in Claude Desktop"))
    }
}

impl Closer for Os {
    fn close(_pids: &[Pid]) -> Result<(), String> {
        Err(unsupported("Closing Claude Desktop"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_fails_without_touching_processes() {
        let l = Located::default();
        assert!(<Os as ClaudeInstall>::locate().is_err());
        assert_eq!(Os::focus(1), FocusResult::Failed);
        assert_eq!(Os::accessibility_trusted(false), None);
        assert!(Os::launch_main(&l).is_err());
        assert!(Os::launch(&l, "/tmp/x").is_err());
        assert!(Os::launch_url(&l, None, "claude://x").is_err());
        assert!(Os::close(&[1]).is_err());
    }
}
