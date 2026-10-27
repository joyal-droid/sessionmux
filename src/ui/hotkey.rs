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

//! Global hotkey.
//!
//! API:
//! ```ignore
//! pub fn register(hwnd:HWND, id:i32, spec:&str /* "Ctrl+Shift+Space" */) -> Result<(),String>
//! pub fn unregister(hwnd:HWND, id:i32)
//! ```
//!
//! `spec` uses accelerator syntax (case-insensitive, `+`-separated, spaces tolerated):
//! modifiers `Ctrl|Control|CommandOrControl|CmdOrCtrl|Command|Cmd`, `Shift`, `Alt|Option|AltGr`,
//! `Win|Super|Meta`; exactly one key: `A`–`Z`, `0`–`9`, `F1`–`F24`, `Space`, `Enter|Return`, `Tab`,
//! `Esc|Escape`, `Backspace`, `Delete|Del`, `Insert|Ins`, `Home`, `End`, `PageUp`, `PageDown`,
//! `Up|Down|Left|Right`, `Plus`, `Minus`, `num0`–`num9`, or one of `` ,.;/'[]\=-` ``.
//! At least one modifier is required (a bare global key would swallow normal typing).
//! WM_HOTKEY is posted to `hwnd` with wparam = `id`; MOD_NOREPEAT is always added.

use crate::util::win;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
};

/// Parse an accelerator into (modifiers without MOD_NOREPEAT, virtual-key code).
pub fn parse(spec: &str) -> Result<(HOT_KEY_MODIFIERS, u32), String> {
    let spec = spec.trim();
    if spec.is_empty() || spec.len() > 64 {
        return Err("empty or too long hotkey".into());
    }
    // "Ctrl++" means Ctrl + Plus: split on '+' but keep a trailing literal '+'.
    let (body, plus_key) = match spec.strip_suffix("++") {
        Some(rest) => (rest, true),
        None => (spec, false),
    };
    let mut parts: Vec<String> = body.split('+').map(|p| p.trim().to_ascii_lowercase()).collect();
    if plus_key {
        parts.push("plus".into());
    }
    let mut mods = HOT_KEY_MODIFIERS(0);
    let mut key: Option<u32> = None;
    for p in &parts {
        let m = match p.as_str() {
            "ctrl" | "control" | "commandorcontrol" | "cmdorctrl" | "command" | "cmd" => Some(MOD_CONTROL),
            "shift" => Some(MOD_SHIFT),
            "alt" | "option" | "altgr" => Some(MOD_ALT),
            "win" | "super" | "meta" | "windows" => Some(MOD_WIN),
            _ => None,
        };
        if let Some(m) = m {
            if mods.0 & m.0 != 0 {
                return Err(format!("duplicate modifier '{p}'"));
            }
            mods |= m;
            continue;
        }
        if key.is_some() {
            return Err(format!("more than one key in '{spec}'"));
        }
        key = Some(vk_for(p).ok_or_else(|| format!("unknown key '{p}'"))?);
    }
    let key = key.ok_or_else(|| format!("no key in '{spec}'"))?;
    if mods.0 == 0 {
        return Err(format!("'{spec}' needs at least one modifier"));
    }
    Ok((mods, key))
}

fn vk_for(name: &str) -> Option<u32> {
    let b = name.as_bytes();
    if b.len() == 1 {
        let c = b[0];
        return match c {
            b'a'..=b'z' => Some((c - b'a' + b'A') as u32),
            b'0'..=b'9' => Some(c as u32),
            b',' => Some(0xBC),  // VK_OEM_COMMA
            b'.' => Some(0xBE),  // VK_OEM_PERIOD
            b';' => Some(0xBA),  // VK_OEM_1
            b'/' => Some(0xBF),  // VK_OEM_2
            b'`' => Some(0xC0),  // VK_OEM_3
            b'[' => Some(0xDB),  // VK_OEM_4
            b'\\' => Some(0xDC), // VK_OEM_5
            b']' => Some(0xDD),  // VK_OEM_6
            b'\'' => Some(0xDE), // VK_OEM_7
            b'=' => Some(0xBB),  // VK_OEM_PLUS
            b'-' => Some(0xBD),  // VK_OEM_MINUS
            _ => None,
        };
    }
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=24).contains(&n).then_some(0x70 + n - 1); // VK_F1..VK_F24
    }
    if let Some(n) = name.strip_prefix("num").and_then(|n| n.parse::<u32>().ok()) {
        return (n <= 9).then_some(0x60 + n); // VK_NUMPAD0..9
    }
    Some(match name {
        "space" => 0x20,
        "enter" | "return" => 0x0D,
        "tab" => 0x09,
        "esc" | "escape" => 0x1B,
        "backspace" => 0x08,
        "delete" | "del" => 0x2E,
        "insert" | "ins" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "up" => 0x26,
        "down" => 0x28,
        "left" => 0x25,
        "right" => 0x27,
        "plus" => 0xBB,
        "minus" => 0xBD,
        _ => return None,
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::ui_thread;
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
    }

    #[test]
    fn rejects_bad_specs() {
        for bad in
            ["", "Space", "Ctrl", "Ctrl+Shift", "Ctrl+A+B", "Ctrl+Ctrl+A", "Ctrl+F25", "Ctrl+Foo", "Ctrl+F0", "Hyper+A"]
        {
            assert!(parse(bad).is_err(), "accepted {bad:?}");
        }
        assert!(parse(&format!("Ctrl+{}", "A".repeat(80))).is_err());
    }

    #[test]
    fn register_and_unregister() {
        let hwnd = ui_thread::create_msg_window(None);
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
