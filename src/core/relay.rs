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

//! Sign-in routing for extra accounts.
//!
//! While an account Sessionmux opened is waiting to sign in, Sessionmux temporarily becomes the
//! per-user `claude://` handler, forwards each link to that account's instance and restores the
//! saved handler afterwards. On Windows the handler is `HKCU\Software\Classes\claude\shell\open\
//! command` and a link arrives as `Sessionmux.exe --relay "%1"`. The state lives in
//! `<user_data>\relay.json` so the short-lived `--relay` process can read it.
//!
//! Handler changes go through the platform's [`UrlHandler`]. A change can also stay unanswered
//! (`Pending`: the relay keeps reading the handler and treats a later switch as done) or be
//! refused by the user (`Declined`: never asked again in that relay). A restore the user refused
//! sets `restoreDeclined`, which stops automatic restores until the user asks for one.
//!
//! Contract:
//! ```ignore
//! pub const PROTOCOL: &str = "claude"; pub const TIMEOUT_SECS: u64 = 900; pub const POLL_MS: u64 = 2000;
//! pub const FILE: &str = "relay.json";
//! pub struct RelayState { active, account_id, account_name, data_dir, since, original_command,
//!                         handler_pending, handler_declined, retakes, restore_pending, restore_declined }
//! impl Relay { open(user_data), open_with(user_data, Option<protocol>), state(), begin(id, name, dir),
//!              ensure() -> Result<bool, String>, end(), defer_restore(), exit_defers_restore(),
//!              restore_settled(), restore_now(), expired(now_secs) }
//! pub fn validate_url(url) -> Result<String, String>; pub fn relay_target(Option<&RelayState>) -> Option<String>;
//! pub fn forward(url, exe, data_dir) -> Result<(), ForwardError>; pub fn forward_to(l, data_dir, pid, url);
//! pub fn redact(url) -> String; pub fn now_secs() -> u64; pub fn signal_bad_link();
//! ```
//! Unit tests (`cfg(test)`) never touch the real `claude` handler: `Relay::open` then manages no
//! handler at all, and the handler tests use a fake [`UrlHandler`] or a throw-away
//! `sessionmux-test-<uuid>` scheme.

use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use crate::brand;
use crate::core::claude_app::{self, Located};
use crate::platform::{ForwardError, HandlerChange, HandlerId, Os, Pid, UrlHandler};
use crate::util::{date, json, log};

pub const PROTOCOL: &str = "claude";
pub const TIMEOUT_SECS: u64 = 15 * 60;
pub const POLL_MS: u64 = 2000;
/// `<user_data>\relay.json`.
pub const FILE: &str = brand::RELAY_FILE;
/// Longest link we forward (characters).
pub const MAX_URL_LEN: usize = 8192;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RelayState {
    pub active: bool,
    #[serde(rename = "accountId")]
    pub account_id: String,
    #[serde(rename = "accountName")]
    pub account_name: String,
    /// The profile waiting to sign in (never Main).
    #[serde(rename = "dataDir")]
    pub data_dir: String,
    /// ISO-8601 UTC.
    pub since: String,
    /// The handler that was there before us (None: there was none).
    #[serde(rename = "originalCommand")]
    pub original_command: Option<String>,
    /// Taking the handler is still waiting for the user's answer.
    #[serde(rename = "handlerPending", default, skip_serializing_if = "is_false")]
    pub handler_pending: bool,
    /// The user kept Claude as the handler: links are not routed in this relay.
    #[serde(rename = "handlerDeclined", default, skip_serializing_if = "is_false")]
    pub handler_declined: bool,
    /// How often this relay took the handler back after Claude claimed it (counted only where
    /// that is limited).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub retakes: u32,
    /// The relay ended but giving the handler back still waits for the user's answer.
    #[serde(rename = "restorePending", default, skip_serializing_if = "is_false")]
    pub restore_pending: bool,
    /// The user chose to keep Sessionmux as the handler: no automatic restore until asked.
    #[serde(rename = "restoreDeclined", default, skip_serializing_if = "is_false")]
    pub restore_declined: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl RelayState {
    fn new(account_id: &str, account_name: &str, data_dir: &str, original_command: Option<String>) -> RelayState {
        RelayState {
            active: true,
            account_id: account_id.to_string(),
            account_name: account_name.to_string(),
            data_dir: data_dir.to_string(),
            since: date::iso_now(),
            original_command,
            handler_pending: false,
            handler_declined: false,
            retakes: 0,
            restore_pending: false,
            restore_declined: false,
        }
    }
}

// ───────────────────────────────────────────────────────────── state ──

/// The relay state of one Sessionmux user-data folder, managing the handler through `H`.
pub struct Relay<H: UrlHandler = Os> {
    user_data: PathBuf,
    /// Scheme whose handler we manage; None = no handler at all (unit tests of other modules).
    protocol: Option<String>,
    handler: PhantomData<H>,
}

impl<H: UrlHandler> Clone for Relay<H> {
    fn clone(&self) -> Self {
        Relay { user_data: self.user_data.clone(), protocol: self.protocol.clone(), handler: PhantomData }
    }
}

impl Relay<Os> {
    /// Relay for `user_data` managing the real `claude` handler (none under `cfg(test)`).
    pub fn open(user_data: &Path) -> Relay {
        Self::open_with(user_data, if cfg!(test) { None } else { Some(PROTOCOL) })
    }

    /// Full control: `protocol` None disables every handler access (relay.json only).
    pub fn open_with(user_data: &Path, protocol: Option<&str>) -> Relay {
        Relay::using(user_data, protocol)
    }
}

impl<H: UrlHandler> Relay<H> {
    /// A relay whose handler changes go through `H`.
    pub fn using(user_data: &Path, protocol: Option<&str>) -> Relay<H> {
        Relay { user_data: user_data.to_path_buf(), protocol: protocol.map(str::to_string), handler: PhantomData }
    }

    fn file(&self) -> PathBuf {
        self.user_data.join(FILE)
    }

    /// From relay.json; None when missing or unreadable (an unreadable file is kept as
    /// `relay.json.corrupt`).
    pub fn state(&self) -> Option<RelayState> {
        let file = self.file();
        if !file.exists() {
            return None;
        }
        match json::read::<RelayState>(&file) {
            Some(s) => Some(s),
            None => {
                let mut bad = file.clone().into_os_string();
                bad.push(".corrupt");
                let _ = std::fs::remove_file(&bad);
                let moved = std::fs::rename(&file, &bad).is_ok();
                log::warn(&format!("relay: {FILE} is unreadable (kept as .corrupt: {moved})"));
                None
            }
        }
    }

    fn persist(&self, st: &RelayState) -> Result<(), String> {
        json::write_atomic(&self.file(), st).map_err(|e| format!("could not write {FILE}: {e}"))
    }

    fn remove_file(&self) -> Result<(), String> {
        match std::fs::remove_file(self.file()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("could not delete {FILE}: {e}")),
        }
    }

    /// True while the handler of our scheme is Sessionmux.
    fn handler_is_us(&self, protocol: &str) -> bool {
        H::current(protocol).is_some_and(|h| H::is_us(&h))
    }

    /// Save the current handler (unless it already is us), persist state, take the handler.
    /// A take that waits for the user's answer or that the user refused still starts the relay
    /// (`handlerPending` / `handlerDeclined`); only a failed take is an error.
    pub fn begin(&self, account_id: &str, account_name: &str, data_dir: &str) -> Result<RelayState, String> {
        if data_dir.trim().is_empty() {
            return Err("relay: dataDir required".into());
        }
        let current = self.protocol.as_deref().and_then(H::current);
        let original_command = match &current {
            // Already ours (a previous relay or a crash): keep whatever was saved before.
            Some(h) if H::is_us(h) => self.state().and_then(|s| s.original_command),
            other => other.as_ref().map(|h| h.0.clone()),
        };
        let mut st = RelayState::new(account_id, account_name, data_dir, original_command);
        self.persist(&st)?;
        if let Some(p) = self.protocol.as_deref() {
            match H::take(p) {
                HandlerChange::Done => {}
                HandlerChange::Pending => {
                    st.handler_pending = true;
                    self.persist(&st)?;
                    log::info("relay: waiting for the answer on taking sign-in links");
                }
                HandlerChange::Declined => {
                    st.handler_declined = true;
                    self.persist(&st)?;
                    log::info("relay: sign-in links stay with Claude (declined)");
                    return Ok(st);
                }
                HandlerChange::Failed(e) => {
                    let _ = self.remove_file();
                    return Err(format!("relay: {e}"));
                }
            }
        }
        log::info(&format!("relay: routing sign-in links to {}", st.account_name));
        Ok(st)
    }

    /// Called every POLL_MS while active. A pending take becomes active once the handler reads
    /// as us. If Claude took the handler back, the value Claude wrote becomes the one restored by
    /// [`Relay::end`] and the handler is taken again, as often as the platform allows; past that
    /// (and at once where changes ask the user) routing stops for this relay. Ok(true) when the
    /// handler was taken again.
    pub fn ensure(&self) -> Result<bool, String> {
        let Some(mut st) = self.state().filter(|s| s.active) else {
            return Ok(false);
        };
        let Some(p) = self.protocol.as_deref() else {
            return Ok(false);
        };
        if st.handler_declined {
            return Ok(false);
        }
        let current = H::current(p);
        if current.as_ref().is_some_and(H::is_us) {
            if st.handler_pending {
                st.handler_pending = false;
                self.persist(&st)?;
                log::info("relay: sign-in links now open through Sessionmux");
            }
            return Ok(false);
        }
        if st.handler_pending {
            return Ok(false); // still waiting for the answer
        }
        let mut dirty = false;
        if let Some(c) = current.map(|h| h.0) {
            if Some(&c) != st.original_command.as_ref() {
                st.original_command = Some(c);
                dirty = true;
            }
        }
        let limit = if H::changes_need_consent() { Some(0) } else { H::retake_limit() };
        if let Some(n) = limit {
            if st.retakes >= n {
                st.handler_declined = true;
                self.persist(&st)?;
                log::info("relay: Claude took the sign-in links back; routing stops for this sign-in");
                return Ok(false);
            }
            st.retakes += 1;
            dirty = true;
        }
        if dirty {
            self.persist(&st)?;
        }
        match H::take(p) {
            HandlerChange::Done => {}
            HandlerChange::Pending => {
                st.handler_pending = true;
                self.persist(&st)?;
            }
            HandlerChange::Declined => {
                st.handler_declined = true;
                self.persist(&st)?;
                log::info("relay: sign-in links stay with Claude (declined)");
                return Ok(false);
            }
            HandlerChange::Failed(e) => return Err(format!("relay: {e}")),
        }
        log::info("relay: handler was re-registered; re-applied");
        Ok(true)
    }

    /// Restore the saved handler (only if the handler still is us), delete relay.json. With
    /// nothing usable saved, Claude's own handler is restored when Claude can be located, else
    /// ours is removed (Claude registers itself again on its next start). A restore waiting for
    /// the user's answer keeps relay.json (`restorePending`, healed by a later call); one the user
    /// refused keeps it too (`restoreDeclined`), and later calls then leave the handler alone.
    pub fn end(&self) -> Result<(), String> {
        let st = self.state();
        if st.as_ref().is_some_and(|s| !s.active && s.restore_declined) {
            return Ok(());
        }
        self.restore_with(st)
    }

    /// End the relay without changing the handler, leaving the restore to the next start
    /// (`restorePending`): used at quit where every change asks the user, so quitting never waits
    /// for an answer. A restore the user already refused stays refused. Asks nothing.
    pub fn defer_restore(&self) -> Result<(), String> {
        if self.protocol.is_none() {
            return self.end(); // no handler to give back: only relay.json goes
        }
        let Some(mut st) = self.state() else { return Ok(()) };
        if !st.active && (st.restore_pending || st.restore_declined) {
            return Ok(());
        }
        st.active = false;
        st.handler_pending = false;
        st.restore_pending = true;
        self.persist(&st)?;
        log::info("relay: sign-in routing ended; sign-in links go back to Claude at the next start");
        Ok(())
    }

    /// True when quitting must leave the handler alone and only mark the restore for the next
    /// start ([`Relay::defer_restore`]); false when quitting gives it back now ([`Relay::end`]).
    pub fn exit_defers_restore() -> bool {
        !H::restore_at_exit() || H::changes_need_consent()
    }

    /// Give the links back because the user asked: also after an earlier refusal.
    #[allow(dead_code)]
    pub fn restore_now(&self) -> Result<(), String> {
        self.restore_with(self.state())
    }

    /// While a restore waits for an answer: true once the handler no longer is us, and the
    /// leftover relay.json is then removed. Asks nothing.
    #[allow(dead_code)]
    pub fn restore_settled(&self) -> bool {
        let Some(st) = self.state().filter(|s| !s.active && s.restore_pending) else {
            return true;
        };
        if self.protocol.as_deref().is_some_and(|p| self.handler_is_us(p)) {
            return false;
        }
        let _ = self.remove_file();
        log::info(&format!("relay: sign-in links are back with Claude ({})", st.account_name));
        true
    }

    fn restore_with(&self, st: Option<RelayState>) -> Result<(), String> {
        let mut result = Ok(());
        let mut keep: Option<RelayState> = None;
        if let Some(p) = self.protocol.as_deref() {
            if self.handler_is_us(p) {
                let prev = st.as_ref().and_then(|s| s.original_command.clone()).map(HandlerId);
                let located = claude_app::locate().ok();
                let finished = |pending: bool, declined: bool| {
                    let mut s = st.clone().unwrap_or_else(|| RelayState::new("", "", "", None));
                    s.active = false;
                    s.handler_pending = false;
                    s.restore_pending = pending;
                    s.restore_declined = declined;
                    s
                };
                match H::restore(p, prev.as_ref(), located.as_ref()) {
                    HandlerChange::Done => {}
                    HandlerChange::Pending => keep = Some(finished(true, false)),
                    HandlerChange::Declined => keep = Some(finished(false, true)),
                    HandlerChange::Failed(e) => result = Err(format!("relay: {e}")),
                }
            }
        }
        let saved = match &keep {
            Some(s) => self.persist(s),
            None => self.remove_file(),
        };
        if st.as_ref().is_some_and(|s| s.active) {
            log::info("relay: sign-in routing ended");
        }
        if let Some(s) = &keep {
            let what = if s.restore_pending { "waits for an answer" } else { "was declined" };
            log::info(&format!("relay: giving sign-in links back to Claude {what}"));
        }
        result.and(saved)
    }

    /// True when the active relay started `TIMEOUT_SECS` or more before `now_secs` (Unix
    /// seconds). An unreadable `since` counts as expired; no/inactive state never expires.
    pub fn expired(&self, now_secs: u64) -> bool {
        self.state().is_some_and(|s| state_expired(&s, now_secs))
    }
}

/// [`Relay::expired`] for one state.
fn state_expired(s: &RelayState, now_secs: u64) -> bool {
    s.active
        && match iso_to_secs(&s.since) {
            Some(t) => now_secs >= t.saturating_add(TIMEOUT_SECS),
            None => true,
        }
}

/// Unix seconds now (for [`Relay::expired`]).
pub fn now_secs() -> u64 {
    log::now_millis() / 1000
}

/// `YYYY-MM-DDTHH:MM:SS[.mmm]Z` → Unix seconds.
fn iso_to_secs(s: &str) -> Option<u64> {
    if !date::is_iso_utc(s) || s.len() < 20 {
        return None;
    }
    let (y, m, d) = date::parse_ymd(&s[..10])?;
    let b = s.as_bytes();
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s[r].parse::<u64>().ok();
    let (hh, mm, ss) = (n(11..13)?, n(14..16)?, n(17..19)?);
    if hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    let days = u64::try_from(date::days_from_civil(y, m, d)).ok()?;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

// ───────────────────────────────────────────────────────────── --relay ──

/// A link we may forward: starts with `claude://` (any case), ≤ 8192 chars, no control chars,
/// no `"`. Returns it trimmed of surrounding whitespace.
pub fn validate_url(url: &str) -> Result<String, String> {
    let u = url.trim();
    if u.is_empty() {
        return Err("empty link".into());
    }
    let prefix = format!("{PROTOCOL}://");
    if u.len() < prefix.len() || !u.is_char_boundary(prefix.len()) || !u[..prefix.len()].eq_ignore_ascii_case(&prefix) {
        return Err("not a claude:// link".into());
    }
    if u.chars().count() > MAX_URL_LEN {
        return Err("link too long".into());
    }
    if u.chars().any(char::is_control) {
        return Err("link contains control characters".into());
    }
    if u.contains('"') {
        return Err("link contains quotes".into());
    }
    Ok(u.to_string())
}

/// A refused `--relay` launch whose command line shows argument injection tells the running
/// Sessionmux, if any, to play its tamper gesture. The relay process itself shows no UI; never blocks.
pub fn signal_bad_link() {
    <Os as UrlHandler>::signal_bad_link();
}

/// `Some(data_dir)` while a relay is active (and names a profile).
pub fn relay_target(state: Option<&RelayState>) -> Option<String> {
    state.filter(|s| s.active && !s.data_dir.trim().is_empty()).map(|s| s.data_dir.clone())
}

/// The `--relay` decision: `(target, stale)`. A relay that timed out, or whose Sessionmux is no
/// longer running (crash, kill, logoff), never routes: the link goes to the default Claude.
/// `stale` (active state but Sessionmux gone) tells the caller to `end()` it so the handler heals
/// now; an expired relay of a running Sessionmux is left to its own timer (it shows the toast).
pub fn relay_decision(state: Option<&RelayState>, now_secs: u64, sessionmux_running: bool) -> (Option<String>, bool) {
    let Some(s) = state.filter(|s| s.active) else {
        return (None, false);
    };
    if !sessionmux_running {
        return (None, true);
    }
    if state_expired(s, now_secs) {
        return (None, false);
    }
    (relay_target(Some(s)), false)
}

/// Deliver a link to the instance of `data_dir` (None = Main) through the platform; `pid` is
/// that instance's main process when known. The url is validated first.
pub fn forward_to(l: &Located, data_dir: Option<&str>, pid: Option<Pid>, url: &str) -> Result<(), ForwardError> {
    let url = validate_url(url)?;
    <Os as UrlHandler>::forward(l, data_dir, pid, &url)
}

/// [`forward_to`] for a caller that knows only Claude's exe (the `--relay` process).
pub fn forward(url: &str, exe: &str, data_dir: Option<&str>) -> Result<(), ForwardError> {
    let l = Located { exe: exe.to_string(), ..Located::default() };
    forward_to(&l, data_dir, None, url)
}

/// `scheme://host/path` of a link for the log: no query, no fragment, at most 200 chars.
pub fn redact(url: &str) -> String {
    let cut = url.find(['?', '#']).unwrap_or(url.len());
    url[..cut].chars().filter(|c| !c.is_control()).take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    fn st(active: bool, dir: &str) -> RelayState {
        RelayState {
            active,
            account_id: "a1".into(),
            account_name: "Work".into(),
            data_dir: dir.into(),
            since: "2026-10-26T08:00:00.000Z".into(),
            original_command: None,
            handler_pending: false,
            handler_declined: false,
            retakes: 0,
            restore_pending: false,
            restore_declined: false,
        }
    }

    // ── a scripted handler, one per test thread ──

    const US: &str = "sessionmux";
    const CLAUDE_APP: &str = "claude-app";

    #[derive(Default)]
    struct FakeState {
        current: Option<String>,
        takes: VecDeque<HandlerChange>,
        restores: VecDeque<HandlerChange>,
        consent: bool,
        keep_at_exit: bool,
        limit: Option<u32>,
        take_calls: u32,
        restore_calls: u32,
    }

    thread_local! {
        static FAKE: RefCell<FakeState> = RefCell::new(FakeState::default());
    }

    fn fake<R>(f: impl FnOnce(&mut FakeState) -> R) -> R {
        FAKE.with(|s| f(&mut s.borrow_mut()))
    }

    struct Fake;

    impl UrlHandler for Fake {
        fn current(_scheme: &str) -> Option<HandlerId> {
            fake(|s| s.current.clone().map(HandlerId))
        }
        fn take(_scheme: &str) -> HandlerChange {
            fake(|s| {
                s.take_calls += 1;
                let r = s.takes.pop_front().unwrap_or(HandlerChange::Done);
                if r == HandlerChange::Done {
                    s.current = Some(US.into());
                }
                r
            })
        }
        fn restore(_scheme: &str, prev: Option<&HandlerId>, _l: Option<&Located>) -> HandlerChange {
            fake(|s| {
                s.restore_calls += 1;
                let r = s.restores.pop_front().unwrap_or(HandlerChange::Done);
                if r == HandlerChange::Done {
                    s.current = Some(prev.map(|h| h.0.clone()).unwrap_or_else(|| CLAUDE_APP.into()));
                }
                r
            })
        }
        fn is_us(h: &HandlerId) -> bool {
            h.0 == US
        }
        fn changes_need_consent() -> bool {
            fake(|s| s.consent)
        }
        fn restore_at_exit() -> bool {
            fake(|s| !s.keep_at_exit)
        }
        fn retake_limit() -> Option<u32> {
            fake(|s| s.limit)
        }
        fn unavailable() -> Option<String> {
            None
        }
        fn forward(_l: &Located, _d: Option<&str>, _p: Option<Pid>, _u: &str) -> Result<(), ForwardError> {
            Ok(())
        }
        fn signal_bad_link() {}
    }

    fn fake_relay(d: &Path) -> Relay<Fake> {
        fake(|s| {
            *s = FakeState::default();
            s.current = Some(CLAUDE_APP.into());
        });
        Relay::<Fake>::using(d, Some("claude"))
    }

    const DIR: &str = "/profiles/Claude-Work";

    #[test]
    fn pending_take_then_done_activates_routing() {
        let d = tempfile::tempdir().unwrap();
        let r = fake_relay(d.path());
        fake(|s| s.takes.push_back(HandlerChange::Pending));
        let s = r.begin("a1", "Work", DIR).unwrap();
        assert!(s.active && s.handler_pending);
        assert_eq!(s.original_command.as_deref(), Some(CLAUDE_APP));
        // No answer yet: nothing is asked again, routing waits.
        assert_eq!(r.ensure(), Ok(false));
        assert!(r.state().unwrap().handler_pending);
        assert_eq!(fake(|s| s.take_calls), 1);
        // The user answered Use: the next tick sees us and routing is active.
        fake(|s| s.current = Some(US.into()));
        assert_eq!(r.ensure(), Ok(false));
        let s = r.state().unwrap();
        assert!(s.active && !s.handler_pending && !s.handler_declined);
        assert_eq!(fake(|s| s.take_calls), 1);
        assert_eq!(relay_target(Some(&s)).as_deref(), Some(DIR));
    }

    #[test]
    fn declined_take_ends_routing_without_asking_again() {
        let d = tempfile::tempdir().unwrap();
        let r = fake_relay(d.path());
        fake(|s| s.takes.push_back(HandlerChange::Declined));
        let s = r.begin("a1", "Work", DIR).unwrap();
        assert!(s.active && s.handler_declined);
        for _ in 0..3 {
            assert_eq!(r.ensure(), Ok(false));
        }
        assert_eq!(fake(|s| s.take_calls), 1);
        // The handler never was ours: ending asks nothing and removes relay.json.
        r.end().unwrap();
        assert_eq!(fake(|s| s.restore_calls), 0);
        assert_eq!(r.state(), None);
    }

    #[test]
    fn failed_take_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        let r = fake_relay(d.path());
        fake(|s| s.takes.push_back(HandlerChange::Failed("nope".into())));
        assert!(r.begin("a1", "Work", DIR).is_err());
        assert_eq!(r.state(), None);
    }

    #[test]
    fn with_consent_a_taken_back_handler_is_never_retaken() {
        let d = tempfile::tempdir().unwrap();
        let r = fake_relay(d.path());
        fake(|s| s.consent = true);
        r.begin("a1", "Work", DIR).unwrap();
        fake(|s| s.current = Some("claude-new".into()));
        assert_eq!(r.ensure(), Ok(false));
        let s = r.state().unwrap();
        assert!(s.handler_declined);
        assert_eq!(s.original_command.as_deref(), Some("claude-new"));
        assert_eq!(r.ensure(), Ok(false));
        assert_eq!(fake(|s| s.take_calls), 1);
    }

    #[test]
    fn retakes_stop_at_the_limit() {
        let d = tempfile::tempdir().unwrap();
        let r = fake_relay(d.path());
        fake(|s| s.limit = Some(1));
        r.begin("a1", "Work", DIR).unwrap();
        fake(|s| s.current = Some(CLAUDE_APP.into()));
        assert_eq!(r.ensure(), Ok(true));
        assert_eq!(r.state().unwrap().retakes, 1);
        fake(|s| s.current = Some(CLAUDE_APP.into()));
        assert_eq!(r.ensure(), Ok(false));
        assert!(r.state().unwrap().handler_declined);
        assert_eq!(fake(|s| s.take_calls), 2);
    }

    #[test]
    fn unlimited_retakes_keep_the_file_shape() {
        let d = tempfile::tempdir().unwrap();
        let r = fake_relay(d.path());
        r.begin("a1", "Work", DIR).unwrap();
        for _ in 0..3 {
            fake(|s| s.current = Some(CLAUDE_APP.into()));
            assert_eq!(r.ensure(), Ok(true));
        }
        let raw: serde_json::Value = json::read(&d.path().join(FILE)).unwrap();
        let keys: Vec<&String> = raw.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["accountId", "accountName", "active", "dataDir", "originalCommand", "since"]);
    }

    #[test]
    fn pending_restore_is_kept_and_heals_later() {
        let d = tempfile::tempdir().unwrap();
        let r = fake_relay(d.path());
        r.begin("a1", "Work", DIR).unwrap();
        fake(|s| s.restores.push_back(HandlerChange::Pending));
        r.end().unwrap();
        let s = r.state().unwrap();
        assert!(!s.active && s.restore_pending && !s.restore_declined);
        assert_eq!(relay_decision(Some(&s), now_secs(), true), (None, false));
        // Still ours: the check asks nothing and keeps waiting.
        assert!(!r.restore_settled());
        assert_eq!(fake(|s| s.restore_calls), 1);
        // The user answered: the next check removes the leftover state.
        fake(|s| s.current = Some(CLAUDE_APP.into()));
        assert!(r.restore_settled());
        assert_eq!(r.state(), None);
    }

    #[test]
    fn pending_restore_is_asked_again_at_the_next_start() {
        let d = tempfile::tempdir().unwrap();
        let r = fake_relay(d.path());
        r.begin("a1", "Work", DIR).unwrap();
        fake(|s| s.restores.push_back(HandlerChange::Pending));
        r.end().unwrap();
        // The next start ends the leftover relay: one more restore, which completes.
        r.end().unwrap();
        assert_eq!(fake(|s| s.restore_calls), 2);
        assert_eq!(fake(|s| s.current.clone()).as_deref(), Some(CLAUDE_APP));
        assert_eq!(r.state(), None);
    }

    #[test]
    fn a_deferred_restore_asks_nothing_and_heals_at_the_next_start() {
        let d = tempfile::tempdir().unwrap();
        let r = fake_relay(d.path());
        r.begin("a1", "Work", DIR).unwrap();
        r.defer_restore().unwrap();
        let s = r.state().unwrap();
        assert!(!s.active && s.restore_pending && !s.restore_declined);
        assert_eq!(s.original_command.as_deref(), Some(CLAUDE_APP));
        assert_eq!(fake(|s| s.restore_calls), 0);
        assert_eq!(fake(|s| s.current.clone()).as_deref(), Some(US));
        // A link arriving in between is not routed.
        assert_eq!(relay_decision(Some(&s), now_secs(), true), (None, false));
        // The next start ends the leftover relay with the one restore.
        r.end().unwrap();
        assert_eq!(fake(|s| s.restore_calls), 1);
        assert_eq!(fake(|s| s.current.clone()).as_deref(), Some(CLAUDE_APP));
        assert_eq!(r.state(), None);
        // Nothing saved: nothing to defer.
        r.defer_restore().unwrap();
        assert_eq!(r.state(), None);
    }

    /// What quitting does with the relay, as the app's quit and logout paths run it.
    fn end_at_exit(r: &Relay<Fake>) {
        if Relay::<Fake>::exit_defers_restore() {
            r.defer_restore().unwrap();
        } else {
            r.end().unwrap();
        }
    }

    #[test]
    fn the_quit_path_never_changes_the_handler_where_exit_keeps_it() {
        let d = tempfile::tempdir().unwrap();
        let r = fake_relay(d.path());
        fake(|s| s.keep_at_exit = true);
        r.begin("a1", "Work", DIR).unwrap();
        let (takes, restores) = fake(|s| (s.take_calls, s.restore_calls));
        end_at_exit(&r);
        assert_eq!(fake(|s| (s.take_calls, s.restore_calls)), (takes, restores));
        assert_eq!(fake(|s| s.current.clone()).as_deref(), Some(US));
        assert!(r.state().unwrap().restore_pending);
        // Quitting again with the restore still pending changes nothing either.
        end_at_exit(&r);
        assert_eq!(fake(|s| (s.take_calls, s.restore_calls)), (takes, restores));
    }

    #[test]
    fn the_quit_path_gives_the_handler_back_where_exit_restores() {
        let d = tempfile::tempdir().unwrap();
        let r = fake_relay(d.path());
        r.begin("a1", "Work", DIR).unwrap();
        end_at_exit(&r);
        assert_eq!(fake(|s| s.restore_calls), 1);
        assert_eq!(fake(|s| s.current.clone()).as_deref(), Some(CLAUDE_APP));
        assert_eq!(r.state(), None);
        // Where each change asks the user, quitting never restores either.
        r.begin("a1", "Work", DIR).unwrap();
        fake(|s| s.consent = true);
        end_at_exit(&r);
        assert_eq!(fake(|s| s.restore_calls), 1);
        assert!(r.state().unwrap().restore_pending);
    }

    #[test]
    fn a_deferred_restore_keeps_an_earlier_refusal() {
        let d = tempfile::tempdir().unwrap();
        let r = fake_relay(d.path());
        r.begin("a1", "Work", DIR).unwrap();
        fake(|s| s.restores.push_back(HandlerChange::Declined));
        r.end().unwrap();
        r.defer_restore().unwrap();
        let s = r.state().unwrap();
        assert!(s.restore_declined && !s.restore_pending);
    }

    #[test]
    fn declined_restore_is_never_retried_automatically() {
        let d = tempfile::tempdir().unwrap();
        let r = fake_relay(d.path());
        r.begin("a1", "Work", DIR).unwrap();
        fake(|s| s.restores.push_back(HandlerChange::Declined));
        r.end().unwrap();
        let s = r.state().unwrap();
        assert!(!s.active && s.restore_declined);
        assert_eq!(s.original_command.as_deref(), Some(CLAUDE_APP));
        for _ in 0..3 {
            r.end().unwrap();
        }
        assert_eq!(fake(|s| s.restore_calls), 1);
        assert!(r.state().unwrap().restore_declined);
        // The user asks: one restore, and the flag is gone once it is done.
        r.restore_now().unwrap();
        assert_eq!(fake(|s| s.restore_calls), 2);
        assert_eq!(fake(|s| s.current.clone()).as_deref(), Some(CLAUDE_APP));
        assert_eq!(r.state(), None);
    }

    #[test]
    fn a_new_sign_in_after_a_declined_restore_keeps_the_saved_handler() {
        let d = tempfile::tempdir().unwrap();
        let r = fake_relay(d.path());
        r.begin("a1", "Work", DIR).unwrap();
        fake(|s| s.restores.push_back(HandlerChange::Declined));
        r.end().unwrap();
        let s = r.begin("a2", "Home", "/profiles/Claude-Home").unwrap();
        assert!(s.active && !s.restore_declined);
        assert_eq!(s.original_command.as_deref(), Some(CLAUDE_APP));
        r.end().unwrap();
        assert_eq!(fake(|s| s.current.clone()).as_deref(), Some(CLAUDE_APP));
        assert_eq!(r.state(), None);
    }

    #[test]
    fn end_leaves_a_handler_someone_else_set() {
        let d = tempfile::tempdir().unwrap();
        let r = fake_relay(d.path());
        r.begin("a1", "Work", DIR).unwrap();
        fake(|s| s.current = Some("other-app".into()));
        r.end().unwrap();
        assert_eq!(fake(|s| s.restore_calls), 0);
        assert_eq!(fake(|s| s.current.clone()).as_deref(), Some("other-app"));
        assert_eq!(r.state(), None);
    }

    #[test]
    fn validate_url_rules() {
        assert_eq!(validate_url("claude://auth?x=1").as_deref(), Ok("claude://auth?x=1"));
        assert_eq!(validate_url("  CLAUDE://auth/callback?code=1 ").as_deref(), Ok("CLAUDE://auth/callback?code=1"));
        assert!(validate_url("http://claude.ai/").is_err());
        assert!(validate_url("https://claude://x").is_err());
        assert!(validate_url("claude:").is_err());
        assert!(validate_url("").is_err());
        assert!(validate_url("claude://a\"b").is_err());
        assert!(validate_url("claude://a\" --inject \"").is_err());
        assert!(validate_url("claude://a\nb").is_err());
        assert!(validate_url("claude://a\u{0}b").is_err());
        assert!(validate_url("claude://a\tb").is_err());
        let max = format!("claude://{}", "a".repeat(MAX_URL_LEN - 9));
        assert!(validate_url(&max).is_ok());
        assert!(validate_url(&format!("{max}a")).is_err());
        assert!(validate_url("ö").is_err());
    }

    #[test]
    fn relay_target_rules() {
        assert_eq!(relay_target(None), None);
        assert_eq!(relay_target(Some(&st(false, r"C:\D\Claude-Work"))), None);
        assert_eq!(relay_target(Some(&st(true, r"C:\D\Claude-Work"))).as_deref(), Some(r"C:\D\Claude-Work"));
        assert_eq!(relay_target(Some(&st(true, "  "))), None);
    }

    #[test]
    fn relay_decision_rules() {
        let dir = r"C:\D\Claude-Work";
        let s = st(true, dir);
        let t0 = iso_to_secs(&s.since).unwrap();
        assert_eq!(relay_decision(None, t0, true), (None, false));
        assert_eq!(relay_decision(Some(&st(false, dir)), t0, false), (None, false));
        assert_eq!(relay_decision(Some(&s), t0 + 60, true), (Some(dir.to_string()), false));
        // Timed out: default target; the running app's timer ends it.
        assert_eq!(relay_decision(Some(&s), t0 + TIMEOUT_SECS, true), (None, false));
        // Sessionmux gone (crash, kill, logoff): default target and heal now.
        assert_eq!(relay_decision(Some(&s), t0 + 60, false), (None, true));
        assert_eq!(relay_decision(Some(&s), t0 + TIMEOUT_SECS * 10, false), (None, true));
    }

    #[test]
    fn forward_refuses_invalid_links() {
        // Refused before anything is started.
        assert!(forward("http://x", "Claude", None).is_err());
        assert!(forward("claude://a\"b", "Claude", Some("/x")).is_err());
    }

    #[test]
    fn redact_drops_query_and_fragment() {
        assert_eq!(redact("claude://auth/callback?code=secret&state=1"), "claude://auth/callback");
        assert_eq!(redact("claude://host/path#frag"), "claude://host/path");
        assert_eq!(redact("claude://host/path"), "claude://host/path");
    }

    #[test]
    fn older_files_read_with_the_new_fields_off() {
        let raw = r#"{"active":true,"accountId":"a1","accountName":"Work","dataDir":"D","since":"2026-10-26T08:00:00.000Z","originalCommand":null}"#;
        let s: RelayState = serde_json::from_str(raw).unwrap();
        assert!(!s.handler_pending && !s.handler_declined && !s.restore_pending && !s.restore_declined);
        assert_eq!(s.retakes, 0);
        assert_eq!(serde_json::to_string(&s).unwrap(), raw);
    }

    #[test]
    fn no_handler_mode_and_corrupt_file() {
        let d = tempfile::tempdir().unwrap();
        let r = Relay::open(d.path()); // cfg(test): no handler
        assert!(r.protocol.is_none());
        r.begin("a1", "Work", r"C:\D\Claude-Work").unwrap();
        assert!(r.state().unwrap().active);
        assert_eq!(r.ensure(), Ok(false));
        r.end().unwrap();
        assert_eq!(r.state(), None);

        std::fs::write(d.path().join(FILE), b"{nope").unwrap();
        assert_eq!(r.state(), None);
        assert!(d.path().join("relay.json.corrupt").exists());
        assert!(!r.expired(u64::MAX));
    }

    #[test]
    fn expired_boundary() {
        let d = tempfile::tempdir().unwrap();
        let r = Relay::open_with(d.path(), None);
        assert!(!r.expired(u64::MAX), "no state never expires");
        let mut s = st(true, r"C:\D\Claude-Work");
        s.since = "2026-10-26T08:00:00.000Z".into();
        json::write_atomic(&d.path().join(FILE), &s).unwrap();
        let t0 = iso_to_secs(&s.since).unwrap();
        assert_eq!(log::iso_from_millis(t0 * 1000), "2026-10-26T08:00:00.000Z");
        assert_eq!(iso_to_secs("1970-01-02T00:00:01Z"), Some(86_401));
        assert!(!r.expired(t0));
        assert!(!r.expired(t0 + TIMEOUT_SECS - 1));
        assert!(r.expired(t0 + TIMEOUT_SECS));
        assert!(r.expired(t0 + TIMEOUT_SECS + 1));

        s.active = false;
        json::write_atomic(&d.path().join(FILE), &s).unwrap();
        assert!(!r.expired(t0 + TIMEOUT_SECS * 10));

        s.active = true;
        s.since = "garbage".into();
        json::write_atomic(&d.path().join(FILE), &s).unwrap();
        assert!(r.expired(0), "an unreadable start time ends the relay");
        assert!(now_secs() > t0 - 86_400 * 365);
    }

    /// The whole relay against the real Windows handler of a throw-away scheme.
    #[cfg(windows)]
    mod registry_handler {
        use super::*;
        use crate::platform::windows::registry;

        /// Throw-away scheme `sessionmux-test-<uuid>`; its whole `HKCU\Software\Classes\<s>` tree
        /// is deleted on drop (also when the test panics).
        struct TestScheme(String);

        impl TestScheme {
            fn new() -> TestScheme {
                TestScheme(format!("sessionmux-test-{}", uuid::Uuid::new_v4()))
            }
            fn command_key(&self) -> String {
                format!("Software\\Classes\\{}\\shell\\open\\command", self.0)
            }
            fn write(&self, command: &str) {
                registry::write_default(&self.command_key(), command).unwrap();
            }
            fn read(&self) -> Option<String> {
                registry::read_default(&self.command_key())
            }
        }

        impl Drop for TestScheme {
            fn drop(&mut self) {
                assert!(self.0.starts_with("sessionmux-test-"));
                registry::delete_tree(&format!("Software\\Classes\\{}", self.0));
            }
        }

        fn is_us(c: Option<String>) -> bool {
            c.is_some_and(|c| <Os as UrlHandler>::is_us(&HandlerId(c)))
        }

        fn default_command(exe: &str) -> String {
            format!("\"{}\" \"%1\"", exe)
        }

        #[test]
        fn begin_ensure_end() {
            let tp = TestScheme::new();
            let d = tempfile::tempdir().unwrap();
            let r = Relay::open_with(d.path(), Some(&tp.0));
            assert_eq!(r.state(), None);
            assert_eq!(r.ensure(), Ok(false));
            // end() only restores a saved command whose exe exists: use stand-in files.
            let fake = |v: &str| {
                let dir = d.path().join(v);
                std::fs::create_dir_all(&dir).unwrap();
                let exe = dir.join("Claude.exe");
                std::fs::write(&exe, b"").unwrap();
                default_command(&exe.to_string_lossy())
            };
            let (claude, claude2) = (fake("v1"), fake("v2"));
            let (c1, c2) = (claude.as_str(), claude2.as_str());

            // Fresh begin saves Claude's handler and writes ours.
            tp.write(c1);
            let s = r.begin("a1", "Work", r"C:\D\Claude-Work").unwrap();
            assert!(s.active);
            assert_eq!(s.original_command.as_deref(), Some(c1));
            assert!(date::is_iso_utc(&s.since));
            assert_eq!(r.state(), Some(s.clone()));
            assert!(is_us(tp.read()));
            let raw: serde_json::Value = json::read(&d.path().join(FILE)).unwrap();
            assert_eq!(raw["accountId"], "a1");
            assert_eq!(raw["accountName"], "Work");
            assert_eq!(raw["dataDir"], r"C:\D\Claude-Work");
            assert_eq!(raw["originalCommand"], c1);
            assert_eq!(r.ensure(), Ok(false));

            // begin again while the handler already points at us: original_command is kept.
            let s2 = r.begin("a2", "Home", r"C:\D\Claude-Home").unwrap();
            assert_eq!(s2.original_command.as_deref(), Some(c1));
            assert_eq!(relay_target(r.state().as_ref()).as_deref(), Some(r"C:\D\Claude-Home"));

            // Claude re-registered (new version) in between: ensure re-writes and remembers it.
            tp.write(c2);
            assert_eq!(r.ensure(), Ok(true));
            assert!(is_us(tp.read()));
            assert_eq!(r.state().unwrap().original_command.as_deref(), Some(c2));
            assert_eq!(r.ensure(), Ok(false));

            // end restores the saved handler and deletes relay.json.
            r.end().unwrap();
            assert_eq!(tp.read().as_deref(), Some(c2));
            assert_eq!(r.state(), None);
            assert!(!d.path().join(FILE).exists());
            assert_eq!(r.ensure(), Ok(false));
            r.end().unwrap(); // idempotent

            // end when someone else changed the handler: leave it alone, still delete relay.json.
            r.begin("a1", "Work", r"C:\D\Claude-Work").unwrap();
            let other = r#""C:\Other\Thing.exe" "%1""#;
            tp.write(other);
            r.end().unwrap();
            assert_eq!(tp.read().as_deref(), Some(other));
            assert!(!d.path().join(FILE).exists());

            // The saved Claude folder vanished (Store update): never restore a dead path.
            tp.write(c1);
            r.begin("a1", "Work", r"C:\D\Claude-Work").unwrap();
            std::fs::remove_file(d.path().join("v1").join("Claude.exe")).unwrap();
            r.end().unwrap();
            let after = tp.read();
            assert_ne!(after.as_deref(), Some(c1));
            assert!(!is_us(after));
        }

        #[test]
        fn begin_without_a_previous_handler() {
            let tp = TestScheme::new();
            let d = tempfile::tempdir().unwrap();
            let r = Relay::open_with(d.path(), Some(&tp.0));
            let s = r.begin("a1", "Work", r"C:\D\Claude-Work").unwrap();
            assert_eq!(s.original_command, None);
            assert!(is_us(tp.read()));
            r.end().unwrap();
            // Restored to Claude's default command (when Claude is installed) or removed; never ours.
            assert!(!is_us(tp.read()));
            assert_eq!(r.state(), None);
            assert!(r.begin("a1", "Work", "  ").is_err());
        }

        #[test]
        fn forward_needs_an_existing_exe() {
            assert!(forward("claude://x", r"C:\definitely\missing\Claude.exe", None).is_err());
        }
    }
}
