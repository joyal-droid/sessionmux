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

//! The operating-system layer. The types shared by both systems live here; the contract of each
//! concern is a trait in `traits/`, implemented by the backend of the target system (`windows/`
//! or `macos/`). The traits and the backend's modules are re-exported here, so callers write
//! `platform::process::...` on either system.

#[cfg(not(any(windows, target_os = "macos")))]
compile_error!("Sessionmux builds for Windows and macOS only");

pub mod traits {
    pub mod core;
    pub mod event;
    pub mod ipc;
    pub mod ui;
}

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod windows;

#[cfg(target_os = "macos")]
#[allow(unused_imports)]
pub use self::macos::{
    autostart, bubble, capture, claude, event_loop, gesture, hotkey, instance, ipc, misc, panel, paths, process, shell,
    tray, url_handler, window,
};
#[cfg(windows)]
#[allow(unused_imports)]
pub use self::windows::{
    autostart, bubble, capture, claude, event_loop, gesture, hotkey, instance, ipc, misc, panel, paths, process, shell,
    tray, url_handler, window,
};
#[allow(unused_imports)]
pub use traits::{core::*, event::*, ipc::*, ui::*};

/// Process id.
#[allow(dead_code)]
pub type Pid = u32;

/// Screen rectangle in the platform's desktop units: Windows physical pixels, macOS points.
/// Origin at the top-left of the primary display, y grows downwards on both platforms.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Opaque native window id (HWND on Windows, CGWindowID for other apps' windows on macOS).
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowId(pub usize);

/// Name of the running system, sent to the panel by `getAppInfo`.
#[allow(dead_code)]
#[cfg(windows)]
pub const OS_NAME: &str = "windows";
/// Name of the running system, sent to the panel by `getAppInfo`.
#[allow(dead_code)]
#[cfg(target_os = "macos")]
pub const OS_NAME: &str = "macos";
