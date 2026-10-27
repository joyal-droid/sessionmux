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

//! Global hotkey: the accelerator grammar. [`parse`] turns a spec into an [`Accel`]; the system
//! backend (`platform::hotkey`) maps it to its own key codes and registers it.
//!
//! `spec` uses accelerator syntax (case-insensitive, `+`-separated, spaces tolerated):
//! modifiers `Ctrl|Control|CommandOrControl|CmdOrCtrl|Command|Cmd`, `Shift`, `Alt|Option|AltGr`,
//! `Win|Super|Meta`; exactly one key: `A`–`Z`, `0`–`9`, `F1`–`F24`, `Space`, `Enter|Return`, `Tab`,
//! `Esc|Escape`, `Backspace`, `Delete|Del`, `Insert|Ins`, `Home`, `End`, `PageUp`, `PageDown`,
//! `Up|Down|Left|Right`, `Plus`, `Minus`, `num0`–`num9`, or one of `` ,.;/'[]\=-` ``.
//! At least one modifier is required (a bare global key would swallow normal typing).
//! `CommandOrControl`, `CmdOrCtrl`, `Command` and `Cmd` mean Ctrl on Windows and ⌘ on macOS;
//! `Win`, `Super` and `Meta` mean the Windows key or ⌘.

#[cfg(windows)]
pub use crate::platform::windows::hotkey::{register, unregister};

/// The key of an accelerator, independent of any keyboard layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// A letter (`b'A'`–`b'Z'`) or a digit (`b'0'`–`b'9'`).
    Char(u8),
    /// A punctuation key, by the character it types on a US layout: one of `` ,.;/'[]\=-` ``.
    /// `Plus` is the `=` key and `Minus` the `-` key.
    Punct(u8),
    /// `F1`–`F24`.
    F(u8),
    /// Numeric keypad `0`–`9`.
    Num(u8),
    Space,
    Enter,
    Tab,
    Escape,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
}

/// A parsed accelerator: its modifiers and its one key. `meta` is the Windows key or ⌘.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Accel {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
    pub key: Key,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Modifier {
    Ctrl,
    Alt,
    Shift,
    Meta,
}

/// `Command` and its spellings: Ctrl on Windows, ⌘ on macOS.
#[cfg(not(target_os = "macos"))]
const COMMAND: Modifier = Modifier::Ctrl;
#[cfg(target_os = "macos")]
const COMMAND: Modifier = Modifier::Meta;

fn modifier(name: &str) -> Option<Modifier> {
    Some(match name {
        "ctrl" | "control" => Modifier::Ctrl,
        "commandorcontrol" | "cmdorctrl" | "command" | "cmd" => COMMAND,
        "shift" => Modifier::Shift,
        "alt" | "option" | "altgr" => Modifier::Alt,
        "win" | "super" | "meta" | "windows" => Modifier::Meta,
        _ => return None,
    })
}

/// Parse an accelerator ("Ctrl+Shift+Space").
pub fn parse(spec: &str) -> Result<Accel, String> {
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
    let mut mods: Vec<Modifier> = Vec::new();
    let mut key: Option<Key> = None;
    for p in &parts {
        if let Some(m) = modifier(p) {
            if mods.contains(&m) {
                return Err(format!("duplicate modifier '{p}'"));
            }
            mods.push(m);
            continue;
        }
        if key.is_some() {
            return Err(format!("more than one key in '{spec}'"));
        }
        key = Some(key_for(p).ok_or_else(|| format!("unknown key '{p}'"))?);
    }
    let key = key.ok_or_else(|| format!("no key in '{spec}'"))?;
    if mods.is_empty() {
        return Err(format!("'{spec}' needs at least one modifier"));
    }
    Ok(Accel {
        ctrl: mods.contains(&Modifier::Ctrl),
        alt: mods.contains(&Modifier::Alt),
        shift: mods.contains(&Modifier::Shift),
        meta: mods.contains(&Modifier::Meta),
        key,
    })
}

fn key_for(name: &str) -> Option<Key> {
    let b = name.as_bytes();
    if b.len() == 1 {
        let c = b[0];
        return match c {
            b'a'..=b'z' => Some(Key::Char(c - b'a' + b'A')),
            b'0'..=b'9' => Some(Key::Char(c)),
            b',' | b'.' | b';' | b'/' | b'`' | b'[' | b'\\' | b']' | b'\'' | b'=' | b'-' => Some(Key::Punct(c)),
            _ => None,
        };
    }
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        return (1..=24).contains(&n).then_some(Key::F(n));
    }
    if let Some(n) = name.strip_prefix("num").and_then(|n| n.parse::<u8>().ok()) {
        return (n <= 9).then_some(Key::Num(n));
    }
    Some(match name {
        "space" => Key::Space,
        "enter" | "return" => Key::Enter,
        "tab" => Key::Tab,
        "esc" | "escape" => Key::Escape,
        "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "insert" | "ins" => Key::Insert,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "plus" => Key::Punct(b'='),
        "minus" => Key::Punct(b'-'),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accel(ctrl: bool, alt: bool, shift: bool, meta: bool, key: Key) -> Accel {
        Accel { ctrl, alt, shift, meta, key }
    }

    #[test]
    fn parses_specs() {
        assert_eq!(parse("Ctrl+Shift+Space").unwrap(), accel(true, false, true, false, Key::Space));
        assert_eq!(parse(" ctrl + alt + K ").unwrap(), accel(true, true, false, false, Key::Char(b'K')));
        assert_eq!(parse("Win+F12").unwrap(), accel(false, false, false, true, Key::F(12)));
        assert_eq!(parse("Super+PageDown").unwrap(), accel(false, false, false, true, Key::PageDown));
        assert_eq!(parse("Option+F24").unwrap(), accel(false, true, false, false, Key::F(24)));
        assert_eq!(parse("Ctrl++").unwrap(), accel(true, false, false, false, Key::Punct(b'=')));
        assert_eq!(parse("Ctrl+Minus").unwrap(), parse("Ctrl+-").unwrap());
        assert_eq!(parse("Ctrl+Shift+/").unwrap(), accel(true, false, true, false, Key::Punct(b'/')));
        assert_eq!(parse("Ctrl+num5").unwrap(), accel(true, false, false, false, Key::Num(5)));
        assert_eq!(parse("Alt+Return").unwrap().key, Key::Enter);
    }

    #[test]
    fn command_follows_the_system() {
        let a = parse("CommandOrControl+Shift+O").unwrap();
        assert_eq!(a.key, Key::Char(b'O'));
        assert!(a.shift && !a.alt);
        let mac = cfg!(target_os = "macos");
        assert_eq!((a.ctrl, a.meta), (!mac, mac));
        assert_eq!(parse("Cmd+A").unwrap(), parse("CmdOrCtrl+A").unwrap());
        // Command and Ctrl are the same modifier on Windows, and two on macOS.
        assert_eq!(parse("Ctrl+Cmd+A").is_ok(), mac);
    }

    #[test]
    fn rejects_bad_specs() {
        for bad in
            ["", "Space", "Ctrl", "Ctrl+Shift", "Ctrl+A+B", "Ctrl+Ctrl+A", "Ctrl+F25", "Ctrl+Foo", "Ctrl+F0", "Hyper+A"]
        {
            assert!(parse(bad).is_err(), "accepted {bad:?}");
        }
        assert!(parse("Ctrl+num10").is_err());
        assert!(parse(&format!("Ctrl+{}", "A".repeat(80))).is_err());
    }
}
