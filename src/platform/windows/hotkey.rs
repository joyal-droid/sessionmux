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

//! Global hotkey on Windows: `RegisterHotKey` on the app's message window, which receives
//! `WM_HOTKEY` with wparam = the id; MOD_NOREPEAT is always added.

use crate::platform::windows::{event_loop, win};
use crate::platform::{HotkeyHost, Os};
use crate::ui::hotkey::{self, Accel, Key};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
};

/// The modifiers of `a`, without MOD_NOREPEAT.
fn modifiers(a: &Accel) -> HOT_KEY_MODIFIERS {
    let mut mods = HOT_KEY_MODIFIERS(0);
    for (on, m) in [(a.ctrl, MOD_CONTROL), (a.shift, MOD_SHIFT), (a.alt, MOD_ALT), (a.meta, MOD_WIN)] {
        if on {
            mods |= m;
        }
    }
    mods
}

/// The virtual-key code of `key`.
fn vk(key: Key) -> u32 {
    match key {
        Key::Char(c) => c as u32,
        Key::Punct(c) => match c {
            b',' => 0xBC,  // VK_OEM_COMMA
            b'.' => 0xBE,  // VK_OEM_PERIOD
            b';' => 0xBA,  // VK_OEM_1
            b'/' => 0xBF,  // VK_OEM_2
            b'`' => 0xC0,  // VK_OEM_3
            b'[' => 0xDB,  // VK_OEM_4
            b'\\' => 0xDC, // VK_OEM_5
            b']' => 0xDD,  // VK_OEM_6
            b'\'' => 0xDE, // VK_OEM_7
            b'=' => 0xBB,  // VK_OEM_PLUS
            _ => 0xBD,     // VK_OEM_MINUS
        },
        Key::F(n) => 0x70 + n as u32 - 1, // VK_F1..VK_F24
        Key::Num(n) => 0x60 + n as u32,   // VK_NUMPAD0..9
        Key::Space => 0x20,
        Key::Enter => 0x0D,
        Key::Tab => 0x09,
        Key::Escape => 0x1B,
        Key::Backspace => 0x08,
        Key::Delete => 0x2E,
        Key::Insert => 0x2D,
        Key::Home => 0x24,
        Key::End => 0x23,
        Key::PageUp => 0x21,
        Key::PageDown => 0x22,
        Key::Up => 0x26,
        Key::Down => 0x28,
        Key::Left => 0x25,
        Key::Right => 0x27,
    }
}

/// Parse an accelerator into (modifiers without MOD_NOREPEAT, virtual-key code).
pub fn parse(spec: &str) -> Result<(HOT_KEY_MODIFIERS, u32), String> {
    let a = hotkey::parse(spec)?;
    Ok((modifiers(&a), vk(a.key)))
}

/// Register `spec` as a global hotkey delivering WM_HOTKEY(id) to `hwnd`.
pub fn register(hwnd: HWND, id: i32, spec: &str) -> Result<(), String> {
    let (mods, vk) = parse(spec)?;
    // SAFETY: plain Win32 call with integer arguments; an invalid hwnd only makes it fail.
    unsafe { RegisterHotKey(hwnd, id, mods | MOD_NOREPEAT, vk) }.map_err(|e| {
        let detail = if e.code().0 as u32 == 0x8007_0581 {
            "already in use by another app".to_string()
        } else {
            let s = e.message();
            if s.is_empty() {
                win::last_error()
            } else {
                s
            }
        };
        format!("RegisterHotKey({spec}): {detail}")
    })
}

/// Unregister; failures (never registered, window gone) are ignored.
pub fn unregister(hwnd: HWND, id: i32) {
    // SAFETY: plain Win32 call with integer arguments.
    let _ = unsafe { UnregisterHotKey(hwnd, id) };
}

impl HotkeyHost for Os {
    fn register(id: u32, spec: &str) -> Result<(), String> {
        event_loop::register_hotkey(id, spec)
    }

    fn unregister(id: u32) {
        event_loop::unregister_hotkey(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;

    #[test]
    fn parses_specs() {
        assert_eq!(parse("Ctrl+Shift+Space").unwrap(), (MOD_CONTROL | MOD_SHIFT, 0x20));
        assert_eq!(parse(" ctrl + alt + K ").unwrap(), (MOD_CONTROL | MOD_ALT, b'K' as u32));
        assert_eq!(parse("CommandOrControl+Shift+O").unwrap(), (MOD_CONTROL | MOD_SHIFT, b'O' as u32));
        assert_eq!(parse("Win+F12").unwrap(), (MOD_WIN, 0x7B));
        assert_eq!(parse("Alt+F24").unwrap(), (MOD_ALT, 0x87));
        assert_eq!(parse("Ctrl+1").unwrap(), (MOD_CONTROL, b'1' as u32));
        assert_eq!(parse("Ctrl++").unwrap(), (MOD_CONTROL, 0xBB));
        assert_eq!(parse("Ctrl+Shift+/").unwrap(), (MOD_CONTROL | MOD_SHIFT, 0xBF));
        assert_eq!(parse("Super+PageDown").unwrap(), (MOD_WIN, 0x22));
        assert_eq!(parse("Ctrl+num5").unwrap(), (MOD_CONTROL, 0x65));
        assert_eq!(parse("Ctrl+Minus").unwrap(), (MOD_CONTROL, 0xBD));
        assert_eq!(parse("Ctrl+-").unwrap(), (MOD_CONTROL, 0xBD));
        assert_eq!(parse("Ctrl+Cmd+A").unwrap_err(), "duplicate modifier 'cmd'");
    }

    #[test]
    fn punctuation_codes() {
        for (c, code) in [
            (',', 0xBC),
            ('.', 0xBE),
            (';', 0xBA),
            ('/', 0xBF),
            ('`', 0xC0),
            ('[', 0xDB),
            ('\\', 0xDC),
            (']', 0xDD),
            ('\'', 0xDE),
            ('=', 0xBB),
        ] {
            assert_eq!(parse(&format!("Ctrl+{c}")).unwrap().1, code, "{c}");
        }
    }

    #[test]
    fn register_and_unregister() {
        let hwnd = event_loop::create_msg_window(None);
        assert!(!hwnd.is_invalid());
        // An obscure combination nobody uses; released immediately.
        let spec = "Ctrl+Alt+Shift+Win+F23";
        register(hwnd, 0x3F01, spec).expect("register");
        // A second registration of the same combination fails (the OS holds it for us).
        assert!(register(hwnd, 0x3F02, spec).is_err());
        unregister(hwnd, 0x3F01);
        register(hwnd, 0x3F02, spec).expect("re-register after unregister");
        unregister(hwnd, 0x3F02);
        assert!(register(hwnd, 0x3F03, "nope").is_err());
        // SAFETY: destroying the message window this test created on this thread.
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}
