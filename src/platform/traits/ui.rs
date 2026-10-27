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

//! The native user interface: the global hotkey, the tray / menu bar icon, the floating bubble,
//! the tamper gesture, the panel's window and the smoke run's captures.
//!
//! What they show is decided by the neutral code in `ui/`: the accelerator grammar
//! (`ui::hotkey`), the menu model (`ui::tray`), the bubble's layout, hit-testing and scene
//! (`ui::bubble`), the gesture's frames and timeline (`ui::gesture`) and the panel's page, protocol
//! and placement (`ui::panel`). The hosts here own the windows and draw.

// Some functions are part of the contract before every caller uses them.
#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;

use raw_window_handle::HasWindowHandle;
use wry::{WebView, WebViewBuilder};

use crate::platform::{Rect, WindowId};
use crate::ui::bubble::{BubbleEvent, BubbleState};
use crate::ui::ipc::IpcHandler;
use crate::ui::tray::TrayMenu;

/// The global hotkey; a press reaches the app as `PlatformEvent::Hotkey(id)`.
pub trait HotkeyHost {
    /// Register `spec` (see `ui::hotkey`) under `id`; the error says why the system refused it.
    fn register(id: u32, spec: &str) -> Result<(), String>;
    /// Release `id`; nothing happens when it is not registered.
    fn unregister(id: u32);
}

/// The tray (Windows) or menu bar (macOS) icon. A left click and the menu's choices reach the
/// app as `PlatformEvent::Tray`. Dropping it removes the icon.
pub trait TrayHost: Sized {
    fn create() -> Result<Self, String>;
    /// The menu shown from now on.
    fn set_menu(&mut self, m: &TrayMenu);
    /// Show `n` requests waiting for approval (0 = none).
    fn set_waiting(&self, n: usize);
    /// Where the icon is on screen, for placing the panel; None places it at the cursor.
    fn anchor(&self) -> Option<Rect>;
}

/// The floating bubble. The host draws the scene `ui::bubble::model::scene` builds for the
/// state and the pointer, and resolves presses through `ui::bubble::model::hit_test`.
pub trait BubbleHost: Sized {
    /// Create and show it at `pos` (desktop units), or in the bottom-right corner of the primary
    /// display. `on_event` runs on the UI thread with no borrow of the bubble held.
    fn create(pos: Option<(i32, i32)>, on_event: Box<dyn Fn(BubbleEvent)>) -> Result<Self, String>;
    /// Show `s`: resized to the new layout, keeping the screen edge it sits on.
    fn set_state(&mut self, s: BubbleState);
    fn show(&self, visible: bool);
    fn is_visible(&self) -> bool;
    /// The window's rectangle in desktop units.
    fn rect(&self) -> Rect;
    fn set_position(&self, x: i32, y: i32);
    /// Write the last drawn frame as PNG (straight alpha).
    fn render_to_png(&self, path: &Path) -> Result<(), String>;
}

/// The tamper gesture's window: click-through, never activated.
pub trait GestureHost {
    /// Play `ui::gesture::timeline` beside `anchor` (the bubble's rectangle) or in the
    /// bottom-right corner of the primary display; playing again while visible restarts it.
    fn play(anchor: Option<Rect>);
    /// Close the window now if it is showing.
    fn close();
    /// Write the held frame at 100 % as PNG (straight alpha); no window is shown.
    fn render_to_png(path: &Path) -> Result<(), String>;
}

/// The panel's native window, which the web view is built into.
pub trait PanelHost: Sized + HasWindowHandle {
    /// Name of the web engine, for error messages.
    const ENGINE: &'static str;
    /// Create the window hidden; with `near` (an anchor and the preferred side, see
    /// [`PanelHost::show_near`]) it is shown there at once without activation. A lost focus
    /// while visible reaches `ipc` as `__blur`, a close request as `hidePanel`.
    fn create(ipc: Arc<dyn IpcHandler>, near: Option<(Rect, &str)>) -> Result<Self, String>;
    /// The builder settings only this system has.
    fn configure(builder: WebViewBuilder<'_>) -> WebViewBuilder<'_>;
    /// Adjust the web view once it is built.
    fn after_build(&self, webview: &WebView);
    /// Place and show the window: beside a non-empty `anchor` rect (the bubble) on `side`
    /// ("left", "right", anything else = right if it fits, else left); above an empty rect's
    /// point (the tray icon), or the cursor's when that point is (0, 0).
    fn show_near(&self, anchor: Rect, side: &str);
    /// Show without activation at (x, y), typically off-screen (smoke captures).
    fn show_inactive_at(&self, x: i32, y: i32);
    /// Make the window the one the user types into.
    fn activate(&self);
    fn hide(&self);
    fn is_visible(&self) -> bool;
    fn window(&self) -> WindowId;
    /// Destroy the window; the web view has been dropped already.
    fn destroy(self);
}

/// Memory use, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Footprint {
    /// Resident (working set) size.
    pub resident: u64,
    /// Private, committed size.
    pub private: u64,
    /// Number of web engine processes counted in.
    pub helpers: usize,
}

/// What the smoke run needs from the system.
pub trait Capture {
    /// Print to the console that started the process, if there is one.
    fn attach_console();
    /// Run UI events for `ms` milliseconds (returns early once the loop was asked to quit).
    fn pump(ms: u64);
    /// Write the panel window `w` as PNG; returns the image size. It must be shown (it may be
    /// off-screen).
    fn panel_png(w: WindowId, path: &Path) -> Result<(u32, u32), String>;
    /// This process alone, or with `helpers` its web engine processes too.
    fn footprint(helpers: bool) -> Footprint;
}
