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

//! The `claude://` link handler. Sign-in routing is not built yet: it reports itself as
//! unavailable, the handler is never read or changed, and no link is forwarded.

use super::unsupported;
use crate::core::claude_app::Located;
use crate::platform::{ForwardError, HandlerChange, HandlerId, Os, Pid, UrlHandler};

impl UrlHandler for Os {
    fn current(_scheme: &str) -> Option<HandlerId> {
        None
    }

    fn take(_scheme: &str) -> HandlerChange {
        HandlerChange::Failed(unsupported("Sign-in routing"))
    }

    fn restore(_scheme: &str, _prev: Option<&HandlerId>, _l: Option<&Located>) -> HandlerChange {
        HandlerChange::Failed(unsupported("Sign-in routing"))
    }

    fn is_us(_h: &HandlerId) -> bool {
        false
    }

    /// Treated like a system that asks for every change, so routing stays off by default.
    fn changes_need_consent() -> bool {
        true
    }

    fn retake_limit() -> Option<u32> {
        Some(0)
    }

    fn unavailable() -> Option<String> {
        Some(unsupported("Sign-in routing"))
    }

    fn forward(_l: &Located, _data_dir: Option<&str>, _pid: Option<Pid>, _url: &str) -> Result<(), ForwardError> {
        Err(ForwardError::Failed(unsupported("Sending links to Claude")))
    }

    fn signal_bad_link() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_reports_itself_unavailable() {
        assert!(Os::current("claude").is_none());
        assert!(matches!(Os::take("claude"), HandlerChange::Failed(_)));
        assert!(matches!(Os::restore("claude", None, None), HandlerChange::Failed(_)));
        assert!(!Os::is_us(&HandlerId("/Applications/Sessionmux.app".into())));
        assert!(Os::changes_need_consent());
        assert_eq!(Os::retake_limit(), Some(0));
        assert!(Os::unavailable().is_some());
        assert!(Os::forward(&Located::default(), None, None, "claude://x").is_err());
        Os::signal_bad_link();
    }
}
