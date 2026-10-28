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

//! Opening links and showing files. Not built yet: each call reports an error the panel shows.

use std::path::Path;

use super::unsupported;
use crate::platform::{Os, PrivacyPane, Shell};

impl Shell for Os {
    fn open_url(_url: &str) -> Result<(), String> {
        Err(unsupported("Opening links"))
    }

    fn reveal(_path: &Path) -> Result<(), String> {
        Err(unsupported("Show in Finder"))
    }

    fn open_privacy_settings(_pane: PrivacyPane) -> Result<(), String> {
        Err(unsupported("Opening System Settings"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_report_an_error() {
        assert!(Os::open_url("https://example.com/").is_err());
        assert!(Os::reveal(Path::new("/tmp")).is_err());
        assert!(Os::open_privacy_settings(PrivacyPane::Automation).is_err());
    }
}
