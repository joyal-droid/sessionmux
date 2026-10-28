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

//! Sessionmux — switch between Claude Desktop accounts. Entry point.
//!
//! Args: `--hidden` (start without showing the panel), `--smoke` (headless self-check), `--relay <url>` /
//! `--relay=<url>` / a bare `claude://…` (forward one sign-in link and exit, no UI),
//! `--end-relay` (the uninstaller: end an active relay and restore Claude's handler, then exit),
//! `--notify-hook` / `--approve-hook` (the Claude Code hook helpers, see `hookhelper`) and
//! `--remove-hooks` (the uninstaller: take our hooks out of the user's Claude Code settings).
//! `--focus-check <plan.json>` runs the app and checks, step by step, that clicking an account
//! brings its Claude window forward (macOS; only with `SESSIONMUX_FOCUS_CHECK=1`, see
//! `focuscheck`).
//! A hook word is honoured only as the one and only argument; any other shape exits 0 silently.
//! Single instance through `platform::instance` (on Windows the named mutex `brand::MUTEX`,
//! `Local\Sessionmux.SingleInstance`); a second launch asks the first instance to show its panel
//! (on Windows by posting `WM_SESSIONMUX_SHOW_PANEL` to its message window) and exits. A refused relay
//! launch whose command line shows argument injection ([`relay_injected`]) also posts
//! `WM_SESSIONMUX_GESTURE` to it (cosmetic); other refused shapes are only logged.
//! The UI thread is prepared through `platform::misc` (on Windows: COM in an STA, per-monitor DPI
//! awareness and the taskbar identity) and then runs the platform's event loop.
#![windows_subsystem = "windows"]
// Direct2D drawing helpers take a rectangle, radius, stroke width and colour as plain values.
#![allow(clippy::too_many_arguments)]

mod app;
pub mod brand;
mod core;
#[cfg(target_os = "macos")]
mod focuscheck;
mod hookhelper;
#[cfg(test)]
mod livetest;
mod platform;
mod smoke;
mod ui;
mod util;

#[cfg(windows)]
use platform::EventLoop;
use platform::{Misc, Os, SingleInstance};
use util::{log, paths};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Args {
    hidden: bool,
    smoke: bool,
}

fn parse_args<I: IntoIterator<Item = String>>(it: I) -> Args {
    let mut a = Args::default();
    for s in it {
        match s.to_ascii_lowercase().as_str() {
            "--hidden" => a.hidden = true,
            "--smoke" => a.smoke = true,
            _ => {}
        }
    }
    a
}

/// The hook helper a launch asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookMode {
    Notify,
    Approve,
    RemoveHooks,
}

fn hook_word(s: &str) -> Option<HookMode> {
    match s {
        "--notify-hook" => Some(HookMode::Notify),
        "--approve-hook" => Some(HookMode::Approve),
        "--remove-hooks" => Some(HookMode::RemoveHooks),
        _ => None,
    }
}

/// The hook helper of a launch: `None` when no argument is a hook word, `Some(None)` when one is
/// but the command line is anything other than exactly that one argument (refused, exit 0 with no
/// output: the helpers accept no further arguments), else `Some(Some(mode))`.
fn hook_arg(args: &[String]) -> Option<Option<HookMode>> {
    if !args.iter().any(|s| hook_word(&s.trim().to_ascii_lowercase()).is_some()) {
        return None;
    }
    match args {
        [only] => Some(hook_word(only)),
        _ => Some(None),
    }
}

/// True for an argument that makes this a relay launch.
fn is_relay_word(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower == "--relay" || lower.starts_with("--relay=") || lower.trim_start().starts_with("claude://")
}

/// The link of a relay launch: exactly `--relay <url>`, `--relay=<url>`, or a
/// bare `claude://…` (Windows passes `"%1"` as one argument). `--relay` without a value gives
/// `Ok("")` (refused later by `validate_url`). Any relay-looking argument makes it a relay launch,
/// so `--smoke` / `--hidden` are never honoured next to one; other shapes are `Err`: extra
/// arguments can only come from a `"` inside `%1` (argument injection), so the launch is refused.
fn relay_arg(args: &[String]) -> Option<Result<String, String>> {
    if !args.iter().any(|s| is_relay_word(s)) {
        return None;
    }
    let first = args[0].as_str();
    let lower = first.to_ascii_lowercase();
    let (url, used) = if lower == "--relay" {
        (args.get(1).cloned().unwrap_or_default(), 2)
    } else if lower.starts_with("--relay=") {
        (first["--relay=".len()..].to_string(), 1)
    } else if lower.trim_start().starts_with("claude://") {
        (first.to_string(), 1)
    } else {
        return Some(Err("link is not the first argument".into()));
    };
    if args.len() > used {
        return Some(Err("unexpected extra arguments".into()));
    }
    Some(Ok(url))
}

/// True when a relay launch's command line carries evidence of argument injection: a quote or a
/// control character in any argument, an extra `--` argument besides the link, or a scheme other
/// than `claude:` in the link. Never true for a shape Windows itself produces from the registered
/// `"<exe>" --relay "%1"` (any `claude:` link, however long, or an empty value) nor for a launch
/// without a relay argument.
fn relay_injected(args: &[String]) -> bool {
    let Some(at) = args.iter().position(|s| is_relay_word(s)) else { return false };
    let word = args[at].as_str();
    let lower = word.to_ascii_lowercase();
    let (link, link_at) = if lower == "--relay" {
        (args.get(at + 1).map(String::as_str).unwrap_or(""), Some(at + 1))
    } else if lower.starts_with("--relay=") {
        (&word["--relay=".len()..], None)
    } else {
        (word, None)
    };
    // `"%1"` with a link ending in an odd number of backslashes: the argv rules turn the closing
    // quote into one literal trailing `"` on the last argument. Windows itself produces that shape.
    let tail_at = if lower.starts_with("--relay=") { None } else { Some(link_at.unwrap_or(at)) };
    let windows_tail = |i: usize, a: &str| {
        Some(i) == tail_at && i + 1 == args.len() && a.ends_with('"') && a.matches('"').count() == 1
    };
    let suspicious =
        |(i, a): (usize, &String)| (a.contains('"') && !windows_tail(i, a)) || a.chars().any(char::is_control);
    if args.iter().enumerate().any(suspicious) {
        return true;
    }
    let extra_flag = args.iter().enumerate().any(|(i, a)| {
        let is_word = i == at;
        let is_link = Some(i) == link_at;
        (!is_word || is_link) && a.trim_start().starts_with("--")
    });
    extra_flag || link_scheme(link).is_some_and(|sch| !sch.eq_ignore_ascii_case("claude"))
}

/// The URI scheme a link starts with (`ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ) ":"`), if any.
fn link_scheme(link: &str) -> Option<&str> {
    let t = link.trim();
    let end = t.find(':')?;
    let sch = &t[..end];
    let mut chars = sch.chars();
    let ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    ok.then_some(sch)
}

/// True while an Sessionmux app instance holds the single-instance guard.
fn sessionmux_running() -> bool {
    platform::instance::Os::running()
}

/// `Sessionmux.exe --end-relay` (run by the uninstaller before it removes the exe):
/// end an active relay so `claude://` points at Claude again. No UI; always exit 0.
fn run_end_relay() -> i32 {
    log::init(&paths::log_file());
    match crate::core::relay::Relay::open(&paths::user_data()).end() {
        Ok(()) => log::info("relay: --end-relay done"),
        Err(e) => log::warn(&format!("relay: --end-relay: {e}")),
    }
    0
}

/// `Sessionmux.exe --relay <url>`: forward one `claude://` link to the account that is signing
/// in (or to the default Claude) and exit. No UI, no hotkeys, no single-instance mutex, no COM.
/// Any failure falls back to the default Claude so the link is never swallowed; always exit 0.
/// A refused link signals the running instance only when `injected` (see [`relay_injected`]).
fn run_relay(raw: &str, injected: bool) -> i32 {
    use crate::core::{claude_app, relay};
    log::init(&paths::log_file());
    let url = match relay::validate_url(raw) {
        Ok(u) => u,
        Err(e) => {
            log::warn(&format!("relay: refused link {} ({e})", relay::redact(raw.trim())));
            if injected {
                relay::signal_bad_link();
            }
            return 0;
        }
    };
    let shown = relay::redact(&url);
    let exe = match claude_app::locate() {
        Ok(l) => l.exe,
        Err(e) => {
            log::error(&format!("relay: cannot forward {shown}: Claude not found ({e})"));
            return 0;
        }
    };
    let rl = relay::Relay::open(&paths::user_data());
    let state = rl.state();
    // A timed-out relay, or one whose Sessionmux crashed / was killed / logged off, never routes.
    let (target, stale) = relay::relay_decision(state.as_ref(), relay::now_secs(), sessionmux_running());
    if stale {
        log::warn("relay: Sessionmux is not running; ending the stale sign-in routing");
    }
    if let (Some(dir), Some(st)) = (target.as_deref(), state.as_ref()) {
        match relay::forward(&url, &exe, Some(dir)) {
            Ok(()) => {
                log::info(&format!("relay: forwarded {shown} to {}", st.account_name));
                return 0;
            }
            Err(e) => {
                log::error(&format!("relay: forwarding {shown} to {} failed ({e}); using default", st.account_name))
            }
        }
    }
    match relay::forward(&url, &exe, None) {
        Ok(()) => log::info(&format!("relay: forwarded {shown} to default")),
        Err(e) => log::error(&format!("relay: forwarding {shown} to default failed ({e})")),
    }
    if stale {
        // Heal the handler now instead of waiting for the next Claude start.
        if let Err(e) = rl.end() {
            log::warn(&format!("relay: ending the stale routing failed ({e})"));
        }
    }
    0
}

fn main() {
    std::process::exit(real_main());
}

fn real_main() -> i32 {
    // Lossy: a stray invalid-UTF-16 argument must not panic (a relay link would be swallowed).
    let raw: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    // Before parse_args, COM, DPI, the mutex and any window: a relay launch
    // never honours --smoke or --hidden (a quote inside %1 could otherwise inject them).
    match relay_arg(&raw) {
        Some(Ok(url)) => return run_relay(&url, relay_injected(&raw)),
        Some(Err(e)) => {
            log::init(&paths::log_file());
            log::warn(&format!("relay: refused launch ({e})"));
            if relay_injected(&raw) {
                crate::core::relay::signal_bad_link();
            }
            return 0;
        }
        None => {}
    }
    if raw.len() == 1 && raw[0].eq_ignore_ascii_case("--end-relay") {
        return run_end_relay();
    }
    // Hook helpers run without COM, DPI, the mutex or any window, and never fall through to the
    // app: a malformed hook launch must not open the panel inside Claude's hook runner.
    match hook_arg(&raw) {
        Some(Some(HookMode::Notify)) => return hookhelper::run_notify_hook(),
        Some(Some(HookMode::Approve)) => return hookhelper::run_approve_hook(),
        Some(Some(HookMode::RemoveHooks)) => return hookhelper::run_remove_hooks(),
        Some(None) => return 0,
        None => {}
    }
    if raw.first().is_some_and(|a| a == "--focus-check") {
        return run_focus_check(&raw[1..]);
    }
    let args = parse_args(raw.iter().cloned());

    let prepared = <Os as Misc>::init_ui_thread();

    let code = if args.smoke {
        app::SMOKE.store(true, std::sync::atomic::Ordering::Relaxed);
        smoke::run()
    } else {
        run_app(args)
    };

    if prepared {
        <Os as Misc>::uninit_ui_thread();
    }
    code
}

/// True when `SESSIONMUX_FOCUS_CHECK` is exactly `1`.
fn focus_check_enabled() -> bool {
    std::env::var("SESSIONMUX_FOCUS_CHECK").is_ok_and(|v| v == "1")
}

/// `--focus-check <plan.json>`: refused unless `SESSIONMUX_FOCUS_CHECK=1` and exactly one plan
/// file follows; exit 2 when refused.
fn run_focus_check(rest: &[String]) -> i32 {
    if !focus_check_enabled() {
        eprintln!("--focus-check runs only with SESSIONMUX_FOCUS_CHECK=1");
        return 2;
    }
    let [plan] = rest else {
        eprintln!("usage: --focus-check <plan.json>");
        return 2;
    };
    focus_check_main(plan)
}

#[cfg(target_os = "macos")]
fn focus_check_main(plan: &str) -> i32 {
    let prepared = <Os as Misc>::init_ui_thread();
    let code = focuscheck::run(std::path::Path::new(plan));
    if prepared {
        <Os as Misc>::uninit_ui_thread();
    }
    code
}

#[cfg(windows)]
fn focus_check_main(_plan: &str) -> i32 {
    eprintln!("--focus-check is available on macOS only");
    2
}

#[cfg(windows)]
fn run_app(args: Args) -> i32 {
    log::init(&paths::log_file());

    let Some(guard) = platform::instance::Os::acquire() else {
        log::info("another instance is running; handing off");
        platform::instance::Os::notify_existing();
        return 0;
    };

    log::info(&format!("{} {} starting{}", brand::NAME, brand::VERSION, if args.hidden { " (hidden)" } else { "" }));
    if let Err(e) = app::init(args.hidden) {
        log::error(&format!("startup failed: {e}"));
        drop(guard);
        return 1;
    }

    let code = <Os as EventLoop>::run();

    app::shutdown();
    drop(guard);
    log::info("exit");
    code
}

/// The app on macOS: AppKit's loop runs first and the app starts once AppKit finished launching,
/// so a link that launched Sessionmux is not lost. A start the system made as a login item is a
/// hidden start, like `--hidden`.
#[cfg(target_os = "macos")]
fn run_app(args: Args) -> i32 {
    log::init(&paths::log_file());

    let Some(guard) = platform::instance::Os::acquire() else {
        log::info("another instance is running; handing off");
        platform::instance::Os::notify_existing();
        return 0;
    };

    log::info(&format!("{} {} starting{}", brand::NAME, brand::VERSION, if args.hidden { " (hidden)" } else { "" }));
    let hidden = args.hidden;
    let launch = move |login_item: bool| {
        if login_item && !hidden {
            log::info("started as a login item");
        }
        app::init(hidden || login_item).inspect_err(|e| log::error(&format!("startup failed: {e}")))
    };
    // Also run when the system ends the process for a logout; the lock is released with the process.
    fn finish() {
        app::shutdown();
        log::info("exit");
    }
    let code = platform::event_loop::run_app(launch, finish);
    drop(guard);
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args() {
        let p = |v: &[&str]| parse_args(v.iter().map(|s| s.to_string()));
        assert_eq!(p(&[]), Args::default());
        assert_eq!(p(&["--hidden"]), Args { hidden: true, smoke: false });
        assert_eq!(p(&["--SMOKE", "x"]), Args { hidden: false, smoke: true });
    }

    #[test]
    fn relay_args() {
        let r = |v: &[&str]| relay_arg(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let ok = |s: &str| Some(Ok(s.to_string()));
        assert_eq!(r(&[]), None);
        assert_eq!(r(&["--hidden"]), None);
        assert_eq!(r(&["--smoke"]), None);
        assert_eq!(r(&["--relay", "claude://auth?x=1"]), ok("claude://auth?x=1"));
        assert_eq!(r(&["--RELAY", "claude://a"]), ok("claude://a"));
        assert_eq!(r(&["--relay=claude://a?b=c"]), ok("claude://a?b=c"));
        assert_eq!(r(&["--relay"]), ok(""));
        assert_eq!(r(&["Claude://auth/callback?code=1"]), ok("Claude://auth/callback?code=1"));
        assert_eq!(r(&["http://x"]), None);
        // A quote inside %1 splits the link: refused, and --smoke / --hidden are never honoured.
        assert!(matches!(r(&["--relay", "claude://x", "--smoke", "y"]), Some(Err(_))));
        assert!(matches!(r(&["--relay=claude://x", "--hidden"]), Some(Err(_))));
        assert!(matches!(r(&["claude://x", "--smoke"]), Some(Err(_))));
        assert!(matches!(r(&["--hidden", "claude://x"]), Some(Err(_))));
        assert!(matches!(r(&["--smoke", "--relay", "claude://x"]), Some(Err(_))));
    }

    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn hook_args() {
        let h = |v: &[&str]| hook_arg(&argv(v));
        assert_eq!(h(&[]), None);
        assert_eq!(h(&["--hidden"]), None);
        assert_eq!(h(&["--end-relay"]), None);
        assert_eq!(h(&["--notify-hook"]), Some(Some(HookMode::Notify)));
        assert_eq!(h(&["--approve-hook"]), Some(Some(HookMode::Approve)));
        assert_eq!(h(&["--remove-hooks"]), Some(Some(HookMode::RemoveHooks)));
        // Exactly the one argument, spelled exactly: anything else is refused and never opens the app.
        for v in [
            argv(&["--approve-hook", "x"]),
            argv(&["--notify-hook", "--smoke"]),
            argv(&["--hidden", "--approve-hook"]),
            argv(&["--approve-hook", "--approve-hook"]),
            argv(&["--APPROVE-HOOK"]),
            argv(&[" --notify-hook"]),
            argv(&["--remove-hooks", ""]),
        ] {
            assert_eq!(hook_arg(&v), Some(None), "{v:?}");
        }
    }

    /// The focus check never starts without its environment switch.
    #[test]
    fn focus_check_needs_its_switch() {
        if std::env::var_os("SESSIONMUX_FOCUS_CHECK").is_none() {
            assert_eq!(run_focus_check(&argv(&["plan.json"])), 2);
            assert_eq!(run_focus_check(&argv(&[])), 2);
        }
    }

    /// Shapes normal use (Windows, a browser, a second launch) can produce never fire the bad-link
    /// trigger, whether or not the link itself is refused.
    #[test]
    fn bad_link_trigger_ignores_normal_shapes() {
        let long = format!("claude://{}", "a".repeat(crate::core::relay::MAX_URL_LEN));
        assert!(crate::core::relay::validate_url(&long).is_err(), "over-long link is refused");
        for v in [
            argv(&["--relay"]),
            argv(&["--relay", ""]),
            argv(&["--relay="]),
            argv(&["--relay", &long]),
            argv(&["--relay", "claude://auth/callback?code=1&state=2"]),
            argv(&["--relay=claude://a?b=c"]),
            argv(&["Claude://auth/callback?code=1"]),
            argv(&["--relay", "claude:"]),
            argv(&["--relay", "claude://x --smoke"]),
            // `"%1"` with %1 ending in a backslash: the closing quote becomes a literal one.
            argv(&["--relay", "claude://auth/callback\""]),
            argv(&["claude://auth/callback\""]),
            // Percent-encoded and other legal characters stay inside the one quoted argument.
            argv(&["--relay", "claude://a/b%22c%20d?e=%E2%9C%93&f=g h#i"]),
            // Normal launches and second-instance hand-offs carry no relay argument.
            argv(&[]),
            argv(&["--hidden"]),
            argv(&["--smoke"]),
            argv(&["--end-relay"]),
        ] {
            assert!(!relay_injected(&v), "{v:?}");
        }
    }

    /// Shapes that only a crafted command line produces fire the bad-link trigger.
    #[test]
    fn bad_link_trigger_fires_on_injection() {
        for v in [
            argv(&["--relay", "claude://a\"b"]),
            argv(&["--relay", "claude://a\"b\""]),
            argv(&["--relay=claude://a\""]),
            argv(&["--relay", "claude://a\"", "x"]),
            argv(&["--relay=claude://a\" --smoke"]),
            argv(&["--relay", "claude://a\nb"]),
            argv(&["--relay", "claude://a\u{0}b"]),
            argv(&["claude://a\tb"]),
            argv(&["--relay", "claude://x", "--x"]),
            argv(&["--relay", "claude://x", "--smoke", "y"]),
            argv(&["claude://x", "--hidden"]),
            argv(&["--hidden", "claude://x"]),
            argv(&["--relay", "--smoke"]),
            argv(&["--relay", "https://claude.ai/"]),
            argv(&["--relay=https://claude.ai/"]),
            argv(&["--relay", "file:///C:/x"]),
        ] {
            assert!(relay_injected(&v), "{v:?}");
        }
    }
}
