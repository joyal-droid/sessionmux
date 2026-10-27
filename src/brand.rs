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

//! Every product identifier in one place: names, files, window classes, registry keys and links.
//! The panel's literal strings live in web/panel.html and the installer's in installer/sessionmux.nsi.

/// The repository; every public link derives from it.
macro_rules! repo_url {
    () => {
        "https://github.com/joyal-droid/sessionmux"
    };
}

pub const NAME: &str = "Sessionmux";
pub const TAGLINE: &str = "Switch between multiple Claude Desktop accounts and move your Code sessions with you";
pub const EXE: &str = "Sessionmux.exe";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DATA_DIR_NAME: &str = "Sessionmux"; // %APPDATA%\Sessionmux
pub const STORE_FILE: &str = "sessionmux.json";
pub const LOG_FILE: &str = "sessionmux.log";
pub const RELAY_FILE: &str = "relay.json"; // sign-in routing state
pub const WEBVIEW_DIR: &str = "WebView2";
pub const MUTEX: &str = "Local\\Sessionmux.SingleInstance";
pub const CLASS_MSG: &str = "SessionmuxMsgWindow";
pub const CLASS_BUBBLE: &str = "SessionmuxBubble";
pub const CLASS_PANEL: &str = "SessionmuxPanel";
pub const CLASS_TRAY_OWNER: &str = "SessionmuxTrayOwner";
pub const AUMID: &str = "Sessionmux.App"; // taskbar / notification identity
pub const RUN_VALUE: &str = "Sessionmux"; // HKCU\...\Run
pub const URL_SCHEME_HOST: &str = "sessionmux://app"; // panel asset scheme
pub const TRAY_TOOLTIP: &str = "Sessionmux — switch between Claude Desktop accounts";

pub const REPO_URL: &str = repo_url!();
pub const URL_HOME: &str = REPO_URL;
pub const URL_SUPPORT: &str = concat!(repo_url!(), "/issues");
pub const URL_RELEASES: &str = concat!(repo_url!(), "/releases");
pub const URL_LICENSE: &str = concat!(repo_url!(), "/blob/main/LICENSE");
/// Sponsor link; empty hides "Support the project" in the About card.
pub const URL_SPONSOR: &str = "";

/// SPDX id of the license (Common Public Attribution License 1.0).
pub const LICENSE_NAME: &str = "CPAL-1.0";

pub const DISCLAIMER: &str = "Sessionmux is an independent, unofficial open-source project. It is not affiliated with, endorsed by or sponsored by Anthropic, PBC. Claude is a trademark of Anthropic, PBC.";

pub fn build_date() -> &'static str {
    env!("SESSIONMUX_BUILD_DATE")
} // YYYY-MM-DD, set by build.rs
