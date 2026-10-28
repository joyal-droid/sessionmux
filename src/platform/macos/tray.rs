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

//! The menu bar icon on macOS.
//!
//! - A variable-length `NSStatusItem` in the system menu bar shows the template image (the mark in
//!   black on transparent, tinted by the system for light and dark menu bars). Its button reports
//!   left and right mouse-ups to a target object.
//! - A left click is `TrayEvent::LeftClick`. A right click or a Control-click shows the menu built
//!   from the last `set_menu` model: the menu is attached to the item for that one click and
//!   detached again, so a left click never opens it. A chosen item is the event its tag maps to.
//! - Requests waiting for approval show as a number beside the icon and in the tooltip.
//! - Events reach the app from the top of the loop, after the click or the menu has returned.
//!   Choosing an account (or all of them) makes Sessionmux the active app first, inside the menu
//!   choice, so it can pass the activation on to Claude.
//! - Dropping the Tray removes the icon.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSCellImagePosition, NSControlStateValueOn, NSEventMask, NSEventModifierFlags, NSEventType, NSImage,
    NSMenu, NSMenuItem, NSScreen, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSData, NSObject, NSObjectProtocol, NSRect, NSSize, NSString};

use super::event_loop::post_event;
use crate::brand;
use crate::platform::{PlatformEvent, Rect, TrayHost};
use crate::ui::tray::{tooltip_text, MenuItem, TrayEvent, TrayMenu};
use crate::util::log;

/// The template image, 18 x 18 pixels for standard displays and 36 x 36 for Retina displays.
const TEMPLATE_1X: &[u8] = include_bytes!("../../../assets/macos/trayTemplate.png");
const TEMPLATE_2X: &[u8] = include_bytes!("../../../assets/macos/trayTemplate@2x.png");
/// The image's size in points.
const TEMPLATE_PT: f64 = 18.0;

/// What the target object keeps: the item whose menu it attaches, the model the next menu is
/// built from, and the model of the menu shown last (its tags are mapped with that one).
struct Ivars {
    item: Retained<NSStatusItem>,
    model: RefCell<TrayMenu>,
    shown: RefCell<Option<TrayMenu>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and `Target` implements no `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SessionmuxStatusItemTarget"]
    #[ivars = Ivars]
    struct Target;

    impl Target {
        /// A mouse-up on the icon.
        #[unsafe(method(buttonClicked:))]
        fn button_clicked(&self, _sender: Option<&AnyObject>) {
            let mtm = self.mtm();
            let ev = NSApplication::sharedApplication(mtm).currentEvent();
            if ev.is_some_and(|e| wants_menu(e.r#type(), e.modifierFlags())) {
                self.show_menu(mtm);
            } else {
                post_event(PlatformEvent::Tray(TrayEvent::LeftClick));
            }
        }

        /// A menu item was chosen.
        #[unsafe(method(menuAction:))]
        fn menu_action(&self, sender: &NSMenuItem) {
            let Ok(cmd) = u32::try_from(sender.tag()) else { return };
            let ev = match self.ivars().shown.borrow().as_ref() {
                Some(m) => m.command_event(cmd),
                None => self.ivars().model.borrow().command_event(cmd),
            };
            if let Some(ev) = ev {
                if matches!(ev, TrayEvent::OpenAccount(_) | TrayEvent::OpenAll) {
                    super::claude::take_activation(self.mtm());
                }
                post_event(PlatformEvent::Tray(ev));
            }
        }
    }

    unsafe impl NSObjectProtocol for Target {}
);

impl Target {
    fn new(mtm: MainThreadMarker, item: Retained<NSStatusItem>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            item,
            model: RefCell::new(TrayMenu::default()),
            shown: RefCell::new(None),
        });
        // SAFETY: plain NSObject initialiser on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }

    /// Show the menu under the icon: attached for this click only, tracked until it closes.
    fn show_menu(&self, mtm: MainThreadMarker) {
        let model = self.ivars().model.borrow().clone();
        let menu = build_menu(mtm, &model.items(), self);
        *self.ivars().shown.borrow_mut() = Some(model);
        let item = &self.ivars().item;
        let Some(button) = item.button(mtm) else { return };
        item.setMenu(Some(&menu));
        // SAFETY: with a menu attached, a click on the item's button opens that menu and tracks it
        // until it closes; no sender is needed.
        unsafe { button.performClick(None) };
        item.setMenu(None);
    }
}

/// True when a click should open the menu: the right button, or any click with Control held.
fn wants_menu(kind: NSEventType, mods: NSEventModifierFlags) -> bool {
    kind == NSEventType::RightMouseUp
        || kind == NSEventType::RightMouseDown
        || mods.contains(NSEventModifierFlags::Control)
}

/// A menu label without control characters.
fn menu_label(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// The text beside the icon: the number of waiting requests, nothing when there are none.
fn waiting_title(n: usize) -> String {
    if n == 0 {
        String::new()
    } else {
        format!(" {n}")
    }
}

/// A menu of `items` whose commands go to `target` with their id as tag. Items are enabled as the
/// model says, not by the responder chain; a checked item shows a check mark.
fn build_menu(mtm: MainThreadMarker, items: &[MenuItem], target: &Target) -> Retained<NSMenu> {
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);
    let t: &AnyObject = target;
    for entry in items {
        match entry {
            MenuItem::Action { id, label, enabled, checked } => {
                // SAFETY: `menuAction:` is implemented by the target with one object argument.
                let item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(mtm),
                        &NSString::from_str(&menu_label(label)),
                        Some(sel!(menuAction:)),
                        &NSString::new(),
                    )
                };
                // SAFETY: the target is the tray's target object, which outlives every menu it
                // builds (menus are only shown while the tray exists).
                unsafe { item.setTarget(Some(t)) };
                item.setTag(*id as isize);
                item.setEnabled(*enabled && *id != 0);
                if *checked {
                    item.setState(NSControlStateValueOn);
                }
                menu.addItem(&item);
            }
            MenuItem::Separator => menu.addItem(&NSMenuItem::separatorItem(mtm)),
            MenuItem::Submenu { label, items } => {
                let title = NSString::from_str(&menu_label(label));
                let sub = build_menu(mtm, items, target);
                sub.setTitle(&title);
                let item = NSMenuItem::new(mtm);
                item.setTitle(&title);
                item.setSubmenu(Some(&sub));
                menu.addItem(&item);
            }
        }
    }
    menu
}

/// The template image with both resolutions, or None when the data cannot be read.
fn template_image() -> Option<Retained<NSImage>> {
    let image = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(TEMPLATE_1X))?;
    let size = NSSize::new(TEMPLATE_PT, TEMPLATE_PT);
    // SAFETY: `imageRepWithData:` returns an autoreleased bitmap image rep or nil for data it
    // cannot read; `setSize:` takes an NSSize and `addRepresentation:` an image rep, both on the
    // rep just created.
    unsafe {
        let rep: Option<Retained<AnyObject>> =
            msg_send![class!(NSBitmapImageRep), imageRepWithData: &*NSData::with_bytes(TEMPLATE_2X)];
        match rep {
            Some(rep) => {
                let _: () = msg_send![&*rep, setSize: size];
                let _: () = msg_send![&*image, addRepresentation: &*rep];
            }
            None => log::warn("tray: the Retina icon could not be read"),
        }
    }
    image.setSize(size);
    image.setTemplate(true);
    image.setAccessibilityDescription(Some(&NSString::from_str(brand::NAME)));
    Some(image)
}

/// A Cocoa rectangle (origin at the bottom-left of the primary display, y upwards) in the
/// top-left coordinates of [`Rect`]; `primary_h` is the primary display's height.
fn to_top_left(r: NSRect, primary_h: f64) -> Rect {
    Rect {
        x: r.origin.x.round() as i32,
        y: (primary_h - (r.origin.y + r.size.height)).round() as i32,
        w: r.size.width.round() as i32,
        h: r.size.height.round() as i32,
    }
}

/// The menu bar icon.
pub struct Tray {
    item: Retained<NSStatusItem>,
    target: Retained<Target>,
    waiting: Cell<usize>,
    /// The icon shows the name because its image could not be read.
    named: bool,
}

impl TrayHost for Tray {
    /// The icon at the right end of the menu bar. Main thread only.
    fn create() -> Result<Tray, String> {
        let mtm = MainThreadMarker::new().ok_or_else(|| "the menu bar icon needs the main thread".to_string())?;
        let bar = NSStatusBar::systemStatusBar();
        let item = bar.statusItemWithLength(NSVariableStatusItemLength);
        let Some(button) = item.button(mtm) else {
            bar.removeStatusItem(&item);
            return Err("the menu bar gave the icon no button".into());
        };
        // Keeps a position the user chose by ⌘-dragging the icon across starts.
        item.setAutosaveName(Some(&NSString::from_str(brand::NAME)));
        let named = match template_image() {
            Some(image) => {
                button.setImage(Some(&image));
                button.setImagePosition(NSCellImagePosition::ImageLeft);
                false
            }
            None => {
                log::warn("tray: the icon image could not be read; showing the name instead");
                button.setTitle(&NSString::from_str(brand::NAME));
                true
            }
        };
        button.setToolTip(Some(&NSString::from_str(&tooltip_text(0))));
        let target = Target::new(mtm, item.clone());
        let t: &AnyObject = &target;
        // SAFETY: the target implements `buttonClicked:` with one object argument; the button
        // does not retain it, and the Tray keeps it alive for as long as the item exists.
        unsafe {
            button.setTarget(Some(t));
            button.setAction(Some(sel!(buttonClicked:)));
        }
        button.sendActionOn(NSEventMask::LeftMouseUp | NSEventMask::RightMouseUp);
        Ok(Tray { item, target, waiting: Cell::new(0), named })
    }

    fn set_menu(&mut self, m: &TrayMenu) {
        *self.target.ivars().model.borrow_mut() = m.clone();
    }

    fn set_waiting(&self, n: usize) {
        if self.waiting.replace(n) == n {
            return;
        }
        let Some(mtm) = MainThreadMarker::new() else { return };
        let Some(button) = self.item.button(mtm) else { return };
        let title = if self.named { format!("{}{}", brand::NAME, waiting_title(n)) } else { waiting_title(n) };
        button.setTitle(&NSString::from_str(&title));
        button.setToolTip(Some(&NSString::from_str(&tooltip_text(n))));
    }

    /// The icon's rectangle on screen.
    fn anchor(&self) -> Option<Rect> {
        let mtm = MainThreadMarker::new()?;
        let frame = self.item.button(mtm)?.window()?.frame();
        let primary_h = NSScreen::screens(mtm).iter().next()?.frame().size.height;
        Some(to_top_left(frame, primary_h))
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        if let Some(button) = self.item.button(mtm) {
            // SAFETY: clearing the button's target and action so no click reaches the target
            // after the item is gone.
            unsafe {
                button.setTarget(None);
                button.setAction(None);
            }
        }
        NSStatusBar::systemStatusBar().removeStatusItem(&self.item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_foundation::NSPoint;

    /// Width and height from a PNG's header.
    fn png_size(data: &[u8]) -> (u32, u32) {
        assert_eq!(&data[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&data[12..16], b"IHDR");
        let n = |at: usize| u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
        (n(16), n(20))
    }

    #[test]
    fn template_images_have_both_resolutions() {
        assert_eq!(png_size(TEMPLATE_1X), (18, 18));
        assert_eq!(png_size(TEMPLATE_2X), (36, 36));
    }

    #[test]
    fn right_and_control_clicks_open_the_menu() {
        let none = NSEventModifierFlags::empty();
        assert!(!wants_menu(NSEventType::LeftMouseUp, none));
        assert!(wants_menu(NSEventType::RightMouseUp, none));
        assert!(wants_menu(NSEventType::RightMouseDown, none));
        assert!(wants_menu(NSEventType::LeftMouseUp, NSEventModifierFlags::Control));
        assert!(!wants_menu(NSEventType::LeftMouseUp, NSEventModifierFlags::Command));
    }

    #[test]
    fn labels_and_waiting_count() {
        assert_eq!(menu_label("R&D\u{7}\n"), "R&D");
        assert_eq!(waiting_title(0), "");
        assert_eq!(waiting_title(3), " 3");
    }

    #[test]
    fn icon_frame_flips_to_top_left() {
        let r = NSRect::new(NSPoint::new(1200.0, 876.0), NSSize::new(30.0, 24.0));
        assert_eq!(to_top_left(r, 900.0), Rect { x: 1200, y: 0, w: 30, h: 24 });
    }

    #[test]
    fn creation_needs_the_main_thread() {
        // Tests normally run on worker threads; on the main thread there is nothing to check.
        if MainThreadMarker::new().is_some() {
            return;
        }
        let e = <Tray as TrayHost>::create().err().expect("created off the main thread");
        assert!(e.contains("main thread"), "{e}");
    }
}
