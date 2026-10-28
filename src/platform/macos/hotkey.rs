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

//! The global shortcut on macOS, through Carbon hot keys (no permission prompt).
//!
//! One handler for hot-key presses is installed on the application's event target the first
//! time a shortcut is registered; a press of a shortcut registered under `id` reaches the app as
//! `PlatformEvent::Hotkey(id)`. Registering an id again replaces its shortcut.
//!
//! Letters, digits and punctuation are looked up in the current keyboard layout (the key that
//! types the character without modifiers), so `Ctrl+Shift+Z` uses the key labelled Z on any
//! layout; a character the layout only types with a modifier falls back to the key at that place
//! on a US keyboard. Other keys use fixed key codes. `Ctrl` is the Control key (⌃), `Cmd` the
//! Command key (⌘), `Alt` and `Option` the Option key (⌥). When macOS refuses a shortcut the
//! error says so with the code it gave.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr;

use objc2::MainThreadMarker;

use super::event_loop::post_event;
use crate::platform::{HotkeyHost, Os, PlatformEvent};
use crate::ui::hotkey::{self, Accel, Key};
use crate::util::log;

type OsStatus = i32;
type EventTargetRef = *mut c_void;
type EventHandlerRef = *mut c_void;
type EventHandlerCallRef = *mut c_void;
type EventRef = *mut c_void;
type EventHotKeyRef = *mut c_void;
type EventHandlerProc = extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OsStatus;

#[repr(C)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct EventHotKeyId {
    signature: u32,
    id: u32,
}

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn InstallEventHandler(
        target: EventTargetRef,
        handler: EventHandlerProc,
        num_types: usize,
        list: *const EventTypeSpec,
        user_data: *mut c_void,
        out_ref: *mut EventHandlerRef,
    ) -> OsStatus;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        id: EventHotKeyId,
        target: EventTargetRef,
        options: u32,
        out_ref: *mut EventHotKeyRef,
    ) -> OsStatus;
    fn UnregisterEventHotKey(hot_key: EventHotKeyRef) -> OsStatus;
    fn GetEventParameter(
        event: EventRef,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        buffer_size: usize,
        actual_size: *mut usize,
        data: *mut c_void,
    ) -> OsStatus;
    fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut c_void;
    fn TISGetInputSourceProperty(source: *mut c_void, key: *const c_void) -> *mut c_void;
    fn UCKeyTranslate(
        layout: *const c_void,
        key_code: u16,
        key_action: u16,
        modifier_state: u32,
        keyboard_type: u32,
        options: u32,
        dead_key_state: *mut u32,
        max_len: usize,
        actual_len: *mut usize,
        chars: *mut u16,
    ) -> OsStatus;
    fn LMGetKbdType() -> u8;
    #[allow(non_upper_case_globals)]
    static kTISPropertyUnicodeKeyLayoutData: *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
    fn CFRelease(cf: *const c_void);
}

/// A four-character Carbon code as the number the system uses.
const fn fourcc(c: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*c)
}

/// Signature of Sessionmux's hot keys, so presses of other code's hot keys are passed on.
const SIGNATURE: u32 = fourcc(b"SMUX");
const EVENT_CLASS_KEYBOARD: u32 = fourcc(b"keyb");
const EVENT_HOT_KEY_PRESSED: u32 = 5;
const EVENT_PARAM_DIRECT_OBJECT: u32 = fourcc(b"----");
const TYPE_EVENT_HOT_KEY_ID: u32 = fourcc(b"hkid");
const NO_ERR: OsStatus = 0;
const EVENT_NOT_HANDLED_ERR: OsStatus = -9874;

/// Carbon modifier bits.
const CMD_KEY: u32 = 0x0100;
const SHIFT_KEY: u32 = 0x0200;
const OPTION_KEY: u32 = 0x0800;
const CONTROL_KEY: u32 = 0x1000;

/// `kUCKeyActionDisplay` and `kUCKeyTranslateNoDeadKeysMask`: the character a key shows, with
/// dead keys typing their own character.
const UC_KEY_ACTION_DISPLAY: u16 = 3;
const UC_NO_DEAD_KEYS: u32 = 1;
/// The typing keys (letters, digits, punctuation, the ISO section key) are key codes 0 to 0x32;
/// the keypad, whose keys also type digits and some punctuation, lies above them.
const LAST_TYPING_KEY: u16 = 0x32;

thread_local! {
    /// The press handler is installed (main thread).
    static HANDLER_INSTALLED: Cell<bool> = const { Cell::new(false) };
    /// Registered hot keys by id (main thread).
    static REGISTERED: RefCell<HashMap<u32, EventHotKeyRef>> = RefCell::new(HashMap::new());
}

/// The Carbon modifier mask of `a`.
fn modifiers(a: &Accel) -> u32 {
    [(a.ctrl, CONTROL_KEY), (a.shift, SHIFT_KEY), (a.alt, OPTION_KEY), (a.meta, CMD_KEY)]
        .into_iter()
        .filter(|(on, _)| *on)
        .fold(0, |m, (_, bit)| m | bit)
}

/// The key code of `key` on a US (ANSI) keyboard; None for keys a Mac keyboard does not have
/// (F21 to F24).
fn ansi_code(key: Key) -> Option<u32> {
    Some(match key {
        Key::Char(c) => match c {
            b'A' => 0x00,
            b'S' => 0x01,
            b'D' => 0x02,
            b'F' => 0x03,
            b'H' => 0x04,
            b'G' => 0x05,
            b'Z' => 0x06,
            b'X' => 0x07,
            b'C' => 0x08,
            b'V' => 0x09,
            b'B' => 0x0B,
            b'Q' => 0x0C,
            b'W' => 0x0D,
            b'E' => 0x0E,
            b'R' => 0x0F,
            b'Y' => 0x10,
            b'T' => 0x11,
            b'1' => 0x12,
            b'2' => 0x13,
            b'3' => 0x14,
            b'4' => 0x15,
            b'6' => 0x16,
            b'5' => 0x17,
            b'9' => 0x19,
            b'7' => 0x1A,
            b'8' => 0x1C,
            b'0' => 0x1D,
            b'O' => 0x1F,
            b'U' => 0x20,
            b'I' => 0x22,
            b'P' => 0x23,
            b'L' => 0x25,
            b'J' => 0x26,
            b'K' => 0x28,
            b'N' => 0x2D,
            b'M' => 0x2E,
            _ => return None,
        },
        Key::Punct(c) => match c {
            b'=' => 0x18,
            b'-' => 0x1B,
            b']' => 0x1E,
            b'[' => 0x21,
            b'\'' => 0x27,
            b';' => 0x29,
            b'\\' => 0x2A,
            b',' => 0x2B,
            b'/' => 0x2C,
            b'.' => 0x2F,
            b'`' => 0x32,
            _ => return None,
        },
        Key::F(n) => match n {
            1 => 0x7A,
            2 => 0x78,
            3 => 0x63,
            4 => 0x76,
            5 => 0x60,
            6 => 0x61,
            7 => 0x62,
            8 => 0x64,
            9 => 0x65,
            10 => 0x6D,
            11 => 0x67,
            12 => 0x6F,
            13 => 0x69,
            14 => 0x6B,
            15 => 0x71,
            16 => 0x6A,
            17 => 0x40,
            18 => 0x4F,
            19 => 0x50,
            20 => 0x5A,
            _ => return None,
        },
        Key::Num(n) => match n {
            0 => 0x52,
            1 => 0x53,
            2 => 0x54,
            3 => 0x55,
            4 => 0x56,
            5 => 0x57,
            6 => 0x58,
            7 => 0x59,
            8 => 0x5B,
            9 => 0x5C,
            _ => return None,
        },
        Key::Space => 0x31,
        Key::Enter => 0x24,
        Key::Tab => 0x30,
        Key::Escape => 0x35,
        Key::Backspace => 0x33,
        Key::Delete => 0x75,
        // A Mac keyboard has Help where a PC keyboard has Insert.
        Key::Insert => 0x72,
        Key::Home => 0x73,
        Key::End => 0x77,
        Key::PageUp => 0x74,
        Key::PageDown => 0x79,
        Key::Left => 0x7B,
        Key::Right => 0x7C,
        Key::Down => 0x7D,
        Key::Up => 0x7E,
    })
}

/// The character a letter, digit or punctuation key stands for; None for other keys.
fn key_char(key: Key) -> Option<char> {
    match key {
        Key::Char(c) | Key::Punct(c) => Some(char::from(c).to_ascii_lowercase()),
        _ => None,
    }
}

/// The first typing key whose unmodified character is `c`, asking `typed` for each key code's
/// character.
fn find_code(c: char, typed: impl Fn(u16) -> Option<String>) -> Option<u32> {
    (0..=LAST_TYPING_KEY).find(|&code| typed(code).is_some_and(|s| s.chars().eq(std::iter::once(c)))).map(u32::from)
}

/// The key that types `c` without modifiers in the current keyboard layout. Main thread only.
fn layout_code(c: char) -> Option<u32> {
    // SAFETY: the input source is a +1 reference released below; the layout data is a CFData
    // owned by that source and read only while it is held; UCKeyTranslate writes at most
    // `buf.len()` UTF-16 units to `buf` and the count to `len`.
    unsafe {
        let source = TISCopyCurrentKeyboardLayoutInputSource();
        if source.is_null() {
            return None;
        }
        let data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
        let found = if data.is_null() {
            None
        } else {
            let layout = CFDataGetBytePtr(data).cast::<c_void>();
            let kbd = u32::from(LMGetKbdType());
            find_code(c, |code| {
                let mut dead = 0u32;
                let mut buf = [0u16; 4];
                let mut len = 0usize;
                let st = UCKeyTranslate(
                    layout,
                    code,
                    UC_KEY_ACTION_DISPLAY,
                    0,
                    kbd,
                    UC_NO_DEAD_KEYS,
                    &mut dead,
                    buf.len(),
                    &mut len,
                    buf.as_mut_ptr(),
                );
                (st == NO_ERR && len > 0).then(|| String::from_utf16_lossy(&buf[..len.min(buf.len())]).to_lowercase())
            })
        };
        CFRelease(source);
        found
    }
}

/// The key code for `key`: from the current layout for characters, else the US position.
fn key_code(key: Key) -> Option<u32> {
    key_char(key).and_then(layout_code).or_else(|| ansi_code(key))
}

/// The message for a shortcut macOS refused with `status`.
fn rejected(status: OsStatus) -> String {
    format!("macOS rejected this shortcut (code {status}). Try one that includes ⌘ or ⌃.")
}

/// A hot-key press: delivered to the app when it is one of Sessionmux's.
extern "C" fn on_hot_key(_call: EventHandlerCallRef, event: EventRef, _user: *mut c_void) -> OsStatus {
    let mut hk = EventHotKeyId::default();
    // SAFETY: `event` is the event being handled; the parameter is copied into `hk`, whose size is
    // passed, and no type or size is asked back.
    let st = unsafe {
        GetEventParameter(
            event,
            EVENT_PARAM_DIRECT_OBJECT,
            TYPE_EVENT_HOT_KEY_ID,
            ptr::null_mut(),
            std::mem::size_of::<EventHotKeyId>(),
            ptr::null_mut(),
            (&mut hk as *mut EventHotKeyId).cast(),
        )
    };
    if st != NO_ERR || hk.signature != SIGNATURE {
        return EVENT_NOT_HANDLED_ERR;
    }
    post_event(PlatformEvent::Hotkey(hk.id));
    NO_ERR
}

/// Install the press handler once.
fn install_handler() -> Result<(), String> {
    if HANDLER_INSTALLED.get() {
        return Ok(());
    }
    let spec = EventTypeSpec { event_class: EVENT_CLASS_KEYBOARD, event_kind: EVENT_HOT_KEY_PRESSED };
    let mut handler: EventHandlerRef = ptr::null_mut();
    // SAFETY: one event type from `spec`, which outlives the call; `on_hot_key` matches the
    // handler signature and uses no user data; the handler stays installed for the process.
    let st = unsafe {
        InstallEventHandler(GetApplicationEventTarget(), on_hot_key, 1, &spec, ptr::null_mut(), &mut handler)
    };
    if st != NO_ERR {
        return Err(rejected(st));
    }
    HANDLER_INSTALLED.set(true);
    Ok(())
}

fn release(id: u32) {
    if let Some(r) = REGISTERED.with(|m| m.borrow_mut().remove(&id)) {
        // SAFETY: `r` came from RegisterEventHotKey and is unregistered once, here.
        let st = unsafe { UnregisterEventHotKey(r) };
        if st != NO_ERR {
            log::debug(&format!("hotkey: releasing {id} gave code {st}"));
        }
    }
}

impl HotkeyHost for Os {
    /// Register `spec` under `id`, replacing what `id` had. Main thread only.
    fn register(id: u32, spec: &str) -> Result<(), String> {
        let accel = hotkey::parse(spec)?;
        if MainThreadMarker::new().is_none() {
            return Err("the shortcut can only be registered on the main thread".into());
        }
        let code = key_code(accel.key).ok_or_else(|| format!("a Mac keyboard has no key for '{spec}'"))?;
        install_handler()?;
        release(id);
        let mut r: EventHotKeyRef = ptr::null_mut();
        // SAFETY: integer arguments and the application's event target; `r` receives the
        // reference, kept until it is unregistered.
        let st = unsafe {
            RegisterEventHotKey(
                code,
                modifiers(&accel),
                EventHotKeyId { signature: SIGNATURE, id },
                GetApplicationEventTarget(),
                0,
                &mut r,
            )
        };
        if st != NO_ERR || r.is_null() {
            return Err(rejected(st));
        }
        REGISTERED.with(|m| m.borrow_mut().insert(id, r));
        Ok(())
    }

    fn unregister(id: u32) {
        if MainThreadMarker::new().is_some() {
            release(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carbon_codes() {
        assert_eq!(SIGNATURE, 0x534D_5558);
        assert_eq!(EVENT_CLASS_KEYBOARD, 0x6B65_7962);
        assert_eq!(TYPE_EVENT_HOT_KEY_ID, 0x686B_6964);
        assert_eq!(EVENT_PARAM_DIRECT_OBJECT, 0x2D2D_2D2D);
    }

    #[test]
    fn modifier_masks() {
        let m = |spec: &str| modifiers(&hotkey::parse(spec).unwrap());
        assert_eq!(m("Ctrl+Shift+Space"), CONTROL_KEY | SHIFT_KEY);
        assert_eq!(m("Cmd+Option+K"), CMD_KEY | OPTION_KEY);
        assert_eq!(m("CommandOrControl+K"), CMD_KEY);
        assert_eq!(m("Ctrl+Cmd+Alt+Shift+K"), 0x1B00);
        assert_eq!(m("Super+K"), CMD_KEY);
    }

    #[test]
    fn ansi_table() {
        let code = |spec: &str| ansi_code(hotkey::parse(spec).unwrap().key);
        assert_eq!(code("Ctrl+Space"), Some(0x31));
        assert_eq!(code("Ctrl+A"), Some(0x00));
        assert_eq!(code("Ctrl+Z"), Some(0x06));
        assert_eq!(code("Ctrl+1"), Some(0x12));
        assert_eq!(code("Ctrl+0"), Some(0x1D));
        assert_eq!(code("Ctrl+/"), Some(0x2C));
        assert_eq!(code("Ctrl+`"), Some(0x32));
        assert_eq!(code("Ctrl++"), Some(0x18));
        assert_eq!(code("Ctrl+Return"), Some(0x24));
        assert_eq!(code("Ctrl+Esc"), Some(0x35));
        assert_eq!(code("Ctrl+Up"), Some(0x7E));
        assert_eq!(code("Ctrl+Left"), Some(0x7B));
        assert_eq!(code("Ctrl+F1"), Some(0x7A));
        assert_eq!(code("Ctrl+F20"), Some(0x5A));
        assert_eq!(code("Ctrl+F21"), None);
        assert_eq!(code("Ctrl+num5"), Some(0x57));

        // Every letter and digit has its own key among the typing keys.
        let mut seen = std::collections::HashSet::new();
        for c in (b'A'..=b'Z').chain(b'0'..=b'9') {
            let k = ansi_code(Key::Char(c)).unwrap_or_else(|| panic!("no key for {}", c as char));
            assert!(k <= u32::from(LAST_TYPING_KEY));
            assert!(seen.insert(k), "{} shares a key", c as char);
        }
        for c in b",.;/'[]\\=-`" {
            let k = ansi_code(Key::Punct(*c)).unwrap();
            assert!(k <= u32::from(LAST_TYPING_KEY));
            assert!(seen.insert(k), "{} shares a key", *c as char);
        }
    }

    #[test]
    fn layout_lookup_prefers_the_layout() {
        // A French layout: A and Q swapped, digits only with Shift.
        let azerty = |code: u16| -> Option<String> {
            Some(
                match code {
                    0x00 => "q",
                    0x0C => "a",
                    0x12 => "&",
                    0x2F => ":",
                    _ => return None,
                }
                .to_string(),
            )
        };
        assert_eq!(find_code('a', azerty), Some(0x0C));
        assert_eq!(find_code('q', azerty), Some(0x00));
        assert_eq!(find_code('1', azerty), None);
        let digit = Key::Char(b'1');
        assert_eq!(key_char(digit), Some('1'));
        assert_eq!(find_code('1', azerty).or_else(|| ansi_code(digit)), Some(0x12));
        assert_eq!(key_char(Key::Char(b'K')), Some('k'));
        assert_eq!(key_char(Key::Space), None);
        // Keypad keys are never taken for the typing key of the same character.
        assert_eq!(find_code('5', |c| (c == 0x57).then(|| "5".to_string())), None);
    }

    #[test]
    fn refusals_name_the_code() {
        assert_eq!(rejected(-9878), "macOS rejected this shortcut (code -9878). Try one that includes ⌘ or ⌃.");
    }

    #[test]
    fn registering_off_the_main_thread_reports_why() {
        // Tests normally run on worker threads; on the main thread there is nothing to check.
        if MainThreadMarker::new().is_some() {
            return;
        }
        assert!(Os::register(1, "Space").unwrap_err().contains("modifier"));
        assert!(Os::register(1, "Ctrl+Shift+Space").unwrap_err().contains("main thread"));
        Os::unregister(1);
    }
}
