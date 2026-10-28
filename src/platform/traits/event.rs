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

//! The UI thread's event loop: what reaches the app from the system (timers, the hotkey, the tray
//! icon, the bubble, second launches, links, quit requests), running work on the UI thread, and
//! the desktop geometry the panel's placement needs.
//!
//! The app keeps one handler, `fn(PlatformEvent)`, given to [`EventLoop::init`]; the backend turns
//! its native messages into these events and calls the handler on the UI thread, from the
//! top-level loop.

// Some variants and functions are part of the contract before every caller uses them.
#![allow(dead_code)]

use crate::platform::{Rect, WindowId};

/// The app's timers. Each one is either running or not; setting a running timer restarts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimerId {
    /// The hidden panel is destroyed when it fires.
    DestroyPanel,
    /// A pinned panel stops ignoring the loss of focus.
    Pin,
    /// One memory trim shortly after start.
    StartupTrim,
    /// Sign-in routing check, every `relay::POLL_MS` while a relay is active.
    SignIn,
}

/// Everything the system tells the app, delivered on the UI thread.
pub enum PlatformEvent {
    Timer(TimerId),
    /// The global hotkey registered under this id was pressed.
    Hotkey(u32),
    /// A choice in the tray / menu bar icon.
    Tray(crate::ui::tray::TrayEvent),
    /// A tray icon notification, read by the tray itself (it may open the menu).
    #[cfg(windows)]
    TrayNotify(crate::platform::windows::event_loop::TrayNotify),
    Bubble(crate::ui::bubble::BubbleEvent),
    /// Show the panel: a second launch, a re-open from the Dock or Finder, a socket `show`.
    ShowPanel,
    /// A `claude://` link handed to Sessionmux by the system (macOS); Windows links arrive through
    /// `--relay` launches instead.
    OpenUrl(String),
    /// Play the tamper gesture.
    Gesture(crate::ui::gesture::Reason),
    /// The panel lost focus while visible.
    PanelBlur,
    /// Displays were added, removed or rearranged.
    DisplayChanged,
    /// The window or app in front changed to one of this process (never Sessionmux itself, nor
    /// on Windows the taskbar or its tray flyout).
    FrontChanged(crate::platform::Pid),
    /// Quit cleanly: the tray's Quit, a close request from the installer, logout.
    QuitRequested,
}

/// The UI thread's loop. Every function except [`EventLoop::post`] is called on the UI thread.
pub trait EventLoop {
    /// Set up the loop on the calling (UI) thread; `on_event` receives every [`PlatformEvent`]
    /// from then on.
    fn init(on_event: fn(PlatformEvent)) -> Result<(), String>;
    /// Run until [`EventLoop::quit`]; the process exit code.
    fn run() -> i32;
    /// Queue `f` to run on the UI thread, from any thread. Never blocks; when the loop is gone
    /// the closure is dropped (logged).
    fn post(f: Box<dyn FnOnce() + Send>);
    /// Deliver `PlatformEvent::Timer(id)` every `ms` milliseconds until it is killed.
    fn set_timer(id: TimerId, ms: u32);
    fn kill_timer(id: TimerId);
    /// End [`EventLoop::run`] after the current event.
    fn quit();
    /// Release what `init` set up (idempotent). Nothing is delivered afterwards.
    fn shutdown();
}

/// The desktop's geometry, in the units of [`Rect`].
pub trait Desktop {
    /// The usable area (without taskbar, Dock or menu bar) of the display nearest to `r`.
    fn work_area(r: &Rect) -> Rect;
    /// Desktop units per device-independent pixel on the display nearest to `r` (Windows: its
    /// DPI / 96; macOS: 1.0, points already are).
    fn units_per_dip(r: &Rect) -> f32;
    /// True when `w` is the window the user works in.
    fn is_foreground(w: WindowId) -> bool;
}
