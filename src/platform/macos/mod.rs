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

//! macOS backend: one module per platform concern. Each module implements its traits from
//! `platform::traits`; a concern that is not built yet reports itself as unavailable instead of
//! failing in an unexpected way.

pub mod autostart;
pub mod bubble;
pub mod capture;
pub mod claude;
pub mod event_loop;
pub mod gesture;
pub mod hotkey;
pub mod instance;
pub mod ipc;
pub mod misc;
pub mod panel;
pub mod paths;
pub mod process;
pub mod shell;
pub mod tray;
pub mod url_handler;
pub mod window;

/// Platform-neutral helpers the macOS build does not call yet: the bubble's pixel size, the
/// gesture's trigger reasons, panel scaling and the per-user local data root. Referenced here so
/// they stay checked on macOS.
#[allow(dead_code)]
fn shared_ui() {
    use crate::ui::bubble::model;
    use crate::ui::{gesture, panel};

    let _ = (model::px_size, panel::scaled, crate::util::paths::local_app_data);
    let _ = [gesture::Reason::ForgedMessage, gesture::Reason::BadLink];
}
