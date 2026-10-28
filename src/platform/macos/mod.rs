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

/// The message for a feature this build does not offer on macOS yet.
pub(crate) fn unsupported(what: &str) -> String {
    format!("{what} is not available on macOS in this version")
}

/// The platform-neutral UI logic the macOS windows are laid out and drawn with: bubble layout,
/// hit-testing and scenes, gesture frames and placement, the tray menu, shortcut parsing, panel
/// placement and PNG encoding. Referenced here so all of it is checked on macOS while those
/// windows report themselves unavailable.
#[allow(dead_code)]
fn shared_ui() {
    use crate::ui::bubble::{model, scene, BubbleEvent};
    use crate::ui::{gesture, hotkey, panel, tray};

    let _ = (model::effective_mode, model::dip_size, model::px_size, model::hit_test, model::Hit::clickable);
    let _ = (model::clamp_to_area, model::snap_rect, model::anchored_rect, model::scene);
    let _ = model::Visual::default;
    let _ = (model::EDGE_MARGIN_DIP, model::DEFAULT_MARGIN_DIP, model::DRAG_THRESHOLD_DIP);
    let _ = |s: &scene::Scene| {
        for p in &s.prims {
            match p {
                scene::Prim::Circle { cx, cy, r, color } => {
                    let _ = (cx, cy, r, color.channels(), color.a);
                }
                scene::Prim::Ring { cx, cy, r, w, color } => {
                    let _ = (cx, cy, r, w, color.channels(), color.a);
                }
                scene::Prim::RoundRect { l, t, r, b, radius, color } => {
                    let _ = (l, t, r, b, radius, color.channels(), color.a);
                }
                scene::Prim::RoundRectStroke { l, t, r, b, radius, w, color } => {
                    let _ = (l, t, r, b, radius, w, color.channels(), color.a);
                }
                scene::Prim::Text { text, size, cx, cy, color, shadow } => {
                    let _ = (text, size, cx, cy, color.channels(), shadow.map(|c| c.a));
                }
            }
        }
        (s.w, s.h, scene::LETTER_SPACING_EM, scene::TEXT_SHADOW_DY)
    };
    let _ = [
        BubbleEvent::Click,
        BubbleEvent::RightClick,
        BubbleEvent::AvatarClick(String::new()),
        BubbleEvent::MoreClick,
        BubbleEvent::DragEnd(0, 0),
    ];

    let _ = (gesture::timeline, gesture::pixel_scale, gesture::render, gesture::place);
    let _ = gesture::GAP_PX;
    let _ = |s: &gesture::Step| (s.frame, s.alpha, s.ms);
    let _ = [gesture::Reason::ForgedMessage, gesture::Reason::BadLink];

    let _ = (tray::TrayMenu::items, tray::TrayMenu::command_event, tray::tooltip_text);
    let _ = tray::TrayEvent::LeftClick;
    let _ = |m: &tray::MenuItem| match m {
        tray::MenuItem::Action { id, label, enabled, checked } => {
            let _ = (id, label, enabled, checked);
        }
        tray::MenuItem::Separator => {}
        tray::MenuItem::Submenu { label, items } => {
            let _ = (label, items);
        }
    };

    let _ = hotkey::parse;
    let _ = |a: &hotkey::Accel| (a.ctrl, a.alt, a.shift, a.meta, a.key);

    let _ = (panel::rect_beside, panel::rect_near_point, panel::scaled, panel::encode_png);
    let _ = (panel::PANEL_W, panel::PANEL_H, panel::PANEL_GAP);

    let _ = crate::util::paths::local_app_data;
}

#[cfg(test)]
mod tests {
    #[test]
    fn unsupported_names_the_feature() {
        assert_eq!(super::unsupported("X"), "X is not available on macOS in this version");
    }
}
