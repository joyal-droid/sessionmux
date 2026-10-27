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

//! Floating bubble: what it shows and how it reacts, the same on both systems. [`model`] holds the
//! layout, hit-testing, edge snapping and the look as a [`scene::Scene`]; the window that draws
//! it and follows the pointer is `platform::bubble`.
//!
//! - Icon mode: the active account's circle (or "C"), its status dot and the waiting-count badge.
//! - Tray mode ("mini tray"): a pill with a grip, up to 3 account avatars and a "⋯" button.
//! - A press that moves more than [`model::DRAG_THRESHOLD_DIP`] drags the bubble; on release it
//!   snaps to the nearest left/right edge of the work area ([`BubbleEvent::DragEnd`]). Otherwise
//!   the release is a click on what was pressed.
//! - Positions passed in / out (`create`, `set_position`, `DragEnd`) are desktop units.

pub mod model;
pub mod scene;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BubbleMode {
    Icon,
    Tray,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BubbleAccount {
    pub id: String,
    pub name: String,
    /// 0xRRGGBB
    pub color: u32,
    pub initials: String,
    pub running: bool,
    pub active: bool,
    /// Running but not signed in yet: the status dot is `--warn` amber instead of green.
    pub needs_sign_in: bool,
    /// Hit its plan limit: the status dot is `--danger` red, shown even while not running.
    pub limited: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BubbleState {
    pub mode: BubbleMode,
    pub active: Option<BubbleAccount>,
    /// At most 3.
    pub accounts: Vec<BubbleAccount>,
    /// Requests waiting for the user's approval: a count badge on the bubble / the "⋯" button.
    pub waiting: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BubbleEvent {
    Click,
    RightClick,
    /// Account id.
    AvatarClick(String),
    MoreClick,
    /// The bubble's new top-left after a drag, snapped to an edge.
    DragEnd(i32, i32),
}
