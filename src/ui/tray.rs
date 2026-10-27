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

//! The tray / menu bar icon's menu, its events and its tooltip. The icon itself is
//! `platform::tray`.
//!
//! Menu: one item per account (radio bullet when running) or a disabled "No accounts",
//! separator, "Open all", "Show bubble"/"Hide bubble", "Floating button" ▸ (Icon | Mini tray,
//! radio), "Settings…", separator, "Quit Sessionmux". A left click on the icon is
//! [`TrayEvent::LeftClick`]; a chosen item is the event [`TrayMenu::command_event`] maps its id to.

use crate::brand;

#[cfg(windows)]
pub use crate::platform::windows::tray::{Tray, WM_TRAY_CALLBACK, WM_TRAY_RECREATE};

pub const ID_OPEN_ALL: u32 = 1;
pub const ID_TOGGLE_BUBBLE: u32 = 2;
pub const ID_MODE_ICON: u32 = 3;
pub const ID_MODE_TRAY: u32 = 4;
pub const ID_SETTINGS: u32 = 5;
pub const ID_QUIT: u32 = 6;
/// Account items are `ID_ACCOUNT_BASE + index`.
pub const ID_ACCOUNT_BASE: u32 = 1000;

#[derive(Clone, Debug, PartialEq)]
pub enum TrayEvent {
    LeftClick,
    /// Account id.
    OpenAccount(String),
    OpenAll,
    ToggleBubble,
    /// "icon" | "tray"
    BubbleMode(String),
    OpenSettings,
    Quit,
}

/// One entry of the menu.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuItem {
    /// A command; `id` 0 never produces an event.
    Action {
        id: u32,
        label: String,
        enabled: bool,
        checked: bool,
    },
    Separator,
    Submenu {
        label: String,
        items: Vec<MenuItem>,
    },
}

/// What the menu shows.
#[derive(Clone, Debug, PartialEq)]
pub struct TrayMenu {
    /// (id, name, running)
    pub accounts: Vec<(String, String, bool)>,
    pub bubble_shown: bool,
    /// "icon" | "tray"
    pub bubble_mode: String,
}

impl Default for TrayMenu {
    fn default() -> Self {
        TrayMenu { accounts: Vec::new(), bubble_shown: true, bubble_mode: "icon".into() }
    }
}

fn action(id: u32, label: &str) -> MenuItem {
    MenuItem::Action { id, label: label.to_string(), enabled: true, checked: false }
}

impl TrayMenu {
    /// `accounts` = (id, name, running); any mode but "tray" is "icon".
    pub fn new(accounts: &[(String, String, bool)], bubble_shown: bool, bubble_mode: &str) -> TrayMenu {
        TrayMenu {
            accounts: accounts.to_vec(),
            bubble_shown,
            bubble_mode: if bubble_mode == "tray" { "tray".into() } else { "icon".into() },
        }
    }

    /// The menu's entries, top to bottom.
    pub fn items(&self) -> Vec<MenuItem> {
        let mut items = Vec::new();
        if self.accounts.is_empty() {
            items.push(MenuItem::Action { id: 0, label: "No accounts".into(), enabled: false, checked: false });
        } else {
            for (i, (_, name, running)) in self.accounts.iter().enumerate() {
                items.push(MenuItem::Action {
                    id: ID_ACCOUNT_BASE + i as u32,
                    label: name.clone(),
                    enabled: true,
                    checked: *running,
                });
            }
        }
        items.push(MenuItem::Separator);
        items.push(action(ID_OPEN_ALL, "Open all"));
        items.push(action(ID_TOGGLE_BUBBLE, if self.bubble_shown { "Hide bubble" } else { "Show bubble" }));
        let tray = self.bubble_mode == "tray";
        items.push(MenuItem::Submenu {
            label: "Floating button".into(),
            items: vec![
                MenuItem::Action { id: ID_MODE_ICON, label: "Icon".into(), enabled: true, checked: !tray },
                MenuItem::Action { id: ID_MODE_TRAY, label: "Mini tray".into(), enabled: true, checked: tray },
            ],
        });
        items.push(action(ID_SETTINGS, "Settings\u{2026}"));
        items.push(MenuItem::Separator);
        items.push(action(ID_QUIT, &format!("Quit {}", brand::NAME)));
        items
    }

    /// Map a menu command id to its event.
    pub fn command_event(&self, cmd: u32) -> Option<TrayEvent> {
        match cmd {
            0 => None,
            ID_OPEN_ALL => Some(TrayEvent::OpenAll),
            ID_TOGGLE_BUBBLE => Some(TrayEvent::ToggleBubble),
            ID_MODE_ICON => Some(TrayEvent::BubbleMode("icon".into())),
            ID_MODE_TRAY => Some(TrayEvent::BubbleMode("tray".into())),
            ID_SETTINGS => Some(TrayEvent::OpenSettings),
            ID_QUIT => Some(TrayEvent::Quit),
            n if n >= ID_ACCOUNT_BASE => {
                self.accounts.get((n - ID_ACCOUNT_BASE) as usize).map(|a| TrayEvent::OpenAccount(a.0.clone()))
            }
            _ => None,
        }
    }
}

/// The tooltip, with the number of requests waiting for approval when there are any.
pub fn tooltip_text(waiting: usize) -> String {
    match waiting {
        0 => brand::TRAY_TOOLTIP.to_string(),
        1 => format!("{} · 1 request waiting for your approval", brand::NAME),
        n => format!("{} · {n} requests waiting for your approval", brand::NAME),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu(accounts: &[(&str, &str, bool)]) -> TrayMenu {
        let a: Vec<(String, String, bool)> =
            accounts.iter().map(|(a, b, c)| (a.to_string(), b.to_string(), *c)).collect();
        TrayMenu::new(&a, true, "icon")
    }

    #[test]
    fn tooltip_counts_waiting_requests() {
        assert_eq!(tooltip_text(0), brand::TRAY_TOOLTIP);
        assert_eq!(tooltip_text(1), "Sessionmux · 1 request waiting for your approval");
        assert_eq!(tooltip_text(3), "Sessionmux · 3 requests waiting for your approval");
        assert!(tooltip_text(1000).encode_utf16().count() < 128);
    }

    #[test]
    fn maps_commands() {
        let t = menu(&[("main", "Main", true), ("w", "Work", false)]);
        assert_eq!(t.command_event(0), None);
        assert_eq!(t.command_event(ID_OPEN_ALL), Some(TrayEvent::OpenAll));
        assert_eq!(t.command_event(ID_TOGGLE_BUBBLE), Some(TrayEvent::ToggleBubble));
        assert_eq!(t.command_event(ID_MODE_ICON), Some(TrayEvent::BubbleMode("icon".into())));
        assert_eq!(t.command_event(ID_MODE_TRAY), Some(TrayEvent::BubbleMode("tray".into())));
        assert_eq!(t.command_event(ID_SETTINGS), Some(TrayEvent::OpenSettings));
        assert_eq!(t.command_event(ID_QUIT), Some(TrayEvent::Quit));
        assert_eq!(t.command_event(ID_ACCOUNT_BASE + 1), Some(TrayEvent::OpenAccount("w".into())));
        assert_eq!(t.command_event(ID_ACCOUNT_BASE + 2), None);
        assert_eq!(t.command_event(7), None);
    }

    #[test]
    fn menu_items() {
        let t = menu(&[("a", "R&D", true), ("b", "Work", false)]);
        let items = t.items();
        assert_eq!(items.len(), 9);
        assert_eq!(
            items[0],
            MenuItem::Action { id: ID_ACCOUNT_BASE, label: "R&D".into(), enabled: true, checked: true }
        );
        assert_eq!(
            items[1],
            MenuItem::Action { id: ID_ACCOUNT_BASE + 1, label: "Work".into(), enabled: true, checked: false }
        );
        assert_eq!(items[2], MenuItem::Separator);
        assert_eq!(items[4], action(ID_TOGGLE_BUBBLE, "Hide bubble"));
        let MenuItem::Submenu { label, items: sub } = &items[5] else { panic!("no submenu: {:?}", items[5]) };
        assert_eq!(label, "Floating button");
        assert!(matches!(sub[0], MenuItem::Action { id: ID_MODE_ICON, checked: true, .. }));
        assert!(matches!(sub[1], MenuItem::Action { id: ID_MODE_TRAY, checked: false, .. }));
        assert_eq!(items[8], action(ID_QUIT, "Quit Sessionmux"));

        let t = TrayMenu::new(&[], false, "tray");
        let items = t.items();
        assert_eq!(items[0], MenuItem::Action { id: 0, label: "No accounts".into(), enabled: false, checked: false });
        assert_eq!(items[3], action(ID_TOGGLE_BUBBLE, "Show bubble"));
        let MenuItem::Submenu { items: sub, .. } = &items[4] else { panic!("no submenu") };
        assert!(matches!(sub[1], MenuItem::Action { id: ID_MODE_TRAY, checked: true, .. }));
        assert_eq!(TrayMenu::new(&[], true, "bogus").bubble_mode, "icon");
        assert_eq!(TrayMenu::default(), TrayMenu::new(&[], true, "icon"));
    }
}
