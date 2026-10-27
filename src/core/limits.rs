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

//! Plan limits: recognising the local signs that an account has hit its usage limit.
//!
//! API:
//! ```ignore
//! pub enum LimitKind { Session, Weekly, Model }
//! pub enum LimitSource { HookStopFailure, Transcript, Notification }
//! pub struct LimitHit { kind, reset_text: Option<String>, until: Option<u64>, source }
//! pub fn classify(text: &str) -> Option<LimitHit>
//! pub fn warning_percent(text: &str) -> Option<u8>
//! pub fn scan_transcript_tail(path: &Path) -> Option<LimitHit>
//! pub struct Limited { kind, since, until: Option<u64>, reset_text: Option<String>, source }
//! impl Limited { pub fn from_hit(&LimitHit, now) -> Limited; pub fn expired(&self, now) -> bool }
//! ```
//!
//! Every signal is local and credential-free: the rendered error text of a turn that ended on a
//! limit (hook `StopFailure`), the tail of the session transcript, and Claude Code's quota
//! notifications. A throttle ("not your usage limit", a client-side rate limit) is never a limit
//! hit, and a "You've used N% of your ... limit" line is only a warning ([`warning_percent`]).
//! All patterns live in one table ([`RULES`]). Times are unix seconds.
//!
//! The rendered texts vary between Claude Code versions, so matching is loose: case-insensitive,
//! typographic apostrophes read as `'`, and the separators `·`, `∙` and `|` are interchangeable.
//! Reset times are turned into an expiry only when that can be done honestly: an epoch, a clock
//! time without a zone (local time), or a clock time in UTC or a fixed `Etc/` zone. A clock time
//! in any other named zone keeps its text for display and expires after the fallback window.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::OnceLock;

/// How much of a transcript's end is read when looking for a limit line.
pub const TAIL_BYTES: u64 = 64 * 1024;
/// How many complete lines at the end of that tail are looked at.
pub const TAIL_LINES: usize = 20;
/// The limited state of a session or model limit without a usable reset time ends after this.
pub const FALLBACK_SESSION_SECS: u64 = 5 * 60 * 60;
/// The limited state of a weekly limit without a usable reset time ends after this.
pub const FALLBACK_WEEKLY_SECS: u64 = 7 * 24 * 60 * 60;

/// Which limit was hit: "weekly" in the text is weekly, a model name is a model limit, anything
/// else is the session (5-hour) limit.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LimitKind {
    Session,
    Weekly,
    Model,
}

/// Where a limit hit was seen.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LimitSource {
    /// Hook `StopFailure` with matcher `rate_limit`.
    HookStopFailure,
    /// An API-error line at the end of the session transcript.
    Transcript,
    /// Hook `Notification` with a `quota_auto_resume_*` type.
    Notification,
}

/// One recognised limit hit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LimitHit {
    pub kind: LimitKind,
    /// The reset part of the message as written ("3:45pm", "Mon 12:00am (UTC)"), for display.
    pub reset_text: Option<String>,
    /// When the limit resets, when the text allows computing it honestly.
    pub until: Option<u64>,
    pub source: LimitSource,
}

/// What a pattern of [`RULES`] stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// A rate limit that is not the plan limit; checked first, it rules out a hit.
    Throttle,
    /// The plan limit was hit.
    Quota,
    /// "You've used N% ..." (group 1 = the percentage); never a hit.
    Warning,
    /// A clock time; the numbers are capture groups (0 = absent): weekday, time, zone.
    ResetClock { day: usize, time: usize, zone: usize },
    /// Epoch seconds in group 1.
    ResetEpoch,
    /// Epoch seconds, or milliseconds from 1e11 up, in group 1.
    ResetEpochOrMillis,
}

/// Every pattern, in the order they are tried within a role (the first reset match wins).
/// Case-insensitive; matched against the trimmed text with `·`/`∙` read as `|`.
const RULES: &[(Role, &str)] = &[
    (Role::Throttle, r"not your usage limit"),
    (Role::Throttle, r"temporarily limiting requests"),
    (Role::Throttle, r"^API Error: Rate limit reached$"),
    (Role::Warning, r"You've used (\d{1,3})% of your (session|weekly) limit"),
    (Role::Quota, r"You've hit your (session|weekly|Opus|Sonnet|[A-Za-z0-9 .\-]{1,24}) limit"),
    (Role::Quota, r"(5-hour|weekly|session) limit reached"),
    (Role::Quota, r"Usage limit reached"),
    (Role::Quota, r"Claude AI usage limit reached\|(\d{9,11})"),
    (Role::Quota, r"Claude usage limit reached"),
    (
        Role::ResetClock { day: 1, time: 2, zone: 3 },
        r"resets?\s+(?:at\s+)?((?:Mon|Tue|Wed|Thu|Fri|Sat|Sun)\s+)?(\d{1,2}(?::\d{2})?\s*(?:am|pm))(?:\s*\(([^)]+)\))?",
    ),
    (Role::ResetClock { day: 0, time: 1, zone: 0 }, r"continuing automatically at (\d{1,2}(?::\d{2})?\s*(?:am|pm))"),
    (
        Role::ResetClock { day: 0, time: 1, zone: 2 },
        r"will reset at (\d{1,2}(?::\d{2})?\s*(?:am|pm))(?:\s*\(([^)]+)\))?",
    ),
    (Role::ResetEpoch, r"\|(\d{9,11})\s*$"),
    (Role::ResetEpochOrMillis, r#""resetsAt"\s*:\s*(\d{9,13})"#),
];

/// The compiled [`RULES`].
fn rules() -> &'static [(Role, rx::Regex)] {
    static COMPILED: OnceLock<Vec<(Role, rx::Regex)>> = OnceLock::new();
    COMPILED.get_or_init(|| RULES.iter().map(|&(role, pat)| (role, rx::Regex::new(pat))).collect())
}

/// A message prepared for matching: the original characters (for display slices) and the same
/// characters with apostrophes and separators unified (same length, same positions).
struct Prepared {
    orig: Vec<char>,
    norm: Vec<char>,
}

impl Prepared {
    fn new(text: &str) -> Prepared {
        let orig: Vec<char> = text.trim().chars().collect();
        let norm = orig
            .iter()
            .map(|&c| match c {
                '\u{2019}' | '\u{2018}' => '\'',
                '\u{00B7}' | '\u{2219}' => '|',
                c => c,
            })
            .collect();
        Prepared { orig, norm }
    }

    fn slice(&self, span: (usize, usize)) -> String {
        self.orig[span.0..span.1].iter().collect()
    }

    fn any(&self, role: Role) -> bool {
        rules().iter().any(|(r, re)| *r == role && re.captures(&self.norm).is_some())
    }
}

/// Classify one rendered message. `Some` only for a quota hit; a throttle, a warning or any other
/// text is `None`. The returned source is [`LimitSource::HookStopFailure`]; callers that read the
/// text elsewhere set their own.
pub fn classify(text: &str) -> Option<LimitHit> {
    let now = now_secs();
    classify_at(text, now, local_offset_secs(now))
}

/// [`classify`] at a given time, with the machine's offset from UTC in seconds.
fn classify_at(text: &str, now: u64, offset: i64) -> Option<LimitHit> {
    let p = Prepared::new(text);
    if p.any(Role::Throttle) || !p.any(Role::Quota) {
        return None;
    }
    Some(hit_from(&p, None, now, offset))
}

/// The hit a quota text describes; `extra` is searched for a reset when the text has none.
fn hit_from(p: &Prepared, extra: Option<&Prepared>, now: u64, offset: i64) -> LimitHit {
    let kind = kind_of(&p.norm);
    let reset = parse_reset(p, now, offset).or_else(|| extra.and_then(|e| parse_reset(e, now, offset)));
    let (reset_text, until) = reset.map(|r| (r.text, r.until)).unwrap_or((None, None));
    LimitHit { kind, reset_text, until, source: LimitSource::HookStopFailure }
}

fn kind_of(norm: &[char]) -> LimitKind {
    let lower: String = norm.iter().collect::<String>().to_ascii_lowercase();
    if lower.contains("weekly") {
        LimitKind::Weekly
    } else if lower.contains("opus") || lower.contains("sonnet") {
        LimitKind::Model
    } else {
        LimitKind::Session
    }
}

/// The reset part of a message.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Reset {
    /// As written, for display (`None` for a bare epoch).
    text: Option<String>,
    /// The zone in parentheses after a clock time.
    zone: Option<String>,
    until: Option<u64>,
}

fn parse_reset(p: &Prepared, now: u64, offset: i64) -> Option<Reset> {
    for (role, re) in rules() {
        let Some(caps) = re.captures(&p.norm) else { continue };
        let group = |g: usize| if g == 0 { None } else { caps.get(g).copied().flatten() };
        match *role {
            Role::ResetClock { day, time, zone } => {
                let Some(t) = group(time) else { continue };
                let d = group(day);
                let z = group(zone);
                let start = d.map_or(t.0, |d| d.0);
                let end = match z {
                    Some(_) => caps[0].map_or(t.1, |m| m.1),
                    None => t.1,
                };
                let text = p.slice((start, end));
                let zone = z.map(|z| p.slice(z).trim().to_string());
                let weekday = d.and_then(|d| weekday_index(p.slice(d).trim()));
                let until = clock_until(&p.slice(t), weekday, zone.as_deref(), now, offset);
                return Some(Reset { text: Some(text), zone, until });
            }
            Role::ResetEpoch | Role::ResetEpochOrMillis => {
                let Some(n) = group(1).and_then(|g| p.slice(g).parse::<u64>().ok()) else { continue };
                let secs = if *role == Role::ResetEpochOrMillis && n >= 100_000_000_000 { n / 1000 } else { n };
                return Some(Reset { text: None, zone: None, until: Some(secs) });
            }
            _ => {}
        }
    }
    None
}

/// Monday = 0 .. Sunday = 6.
fn weekday_index(s: &str) -> Option<u32> {
    let s = s.to_ascii_lowercase();
    ["mon", "tue", "wed", "thu", "fri", "sat", "sun"].iter().position(|d| s.starts_with(d)).map(|i| i as u32)
}

/// The offset from UTC of a zone whose offset is fixed: UTC/GMT and the `Etc/` zones (where
/// `Etc/GMT+5` is five hours behind UTC). Any other zone is unknown here.
fn fixed_zone_offset(zone: &str) -> Option<i64> {
    let z = zone.trim().to_ascii_lowercase();
    if matches!(z.as_str(), "utc" | "gmt" | "z" | "zulu") {
        return Some(0);
    }
    let rest = z.strip_prefix("etc/")?;
    if matches!(rest, "utc" | "uct" | "gmt" | "gmt0" | "greenwich" | "universal" | "zulu") {
        return Some(0);
    }
    let n = rest.strip_prefix("gmt")?;
    let (sign, digits) = match n.as_bytes().first()? {
        b'+' => (-1, &n[1..]),
        b'-' => (1, &n[1..]),
        _ => return None,
    };
    let hours: i64 = digits.parse().ok().filter(|h| (0..=14).contains(h))?;
    Some(sign * hours * 3600)
}

/// "3:45pm" in a zone (none = local time) → the next such moment after `now`; with a weekday,
/// the next such weekday. `None` when the time is malformed or the zone's offset is unknown.
fn clock_until(time: &str, weekday: Option<u32>, zone: Option<&str>, now: u64, offset: i64) -> Option<u64> {
    let (hour, minute) = parse_clock(time)?;
    let offset = match zone {
        Some(z) => fixed_zone_offset(z)?,
        None => offset,
    };
    let local_now = now as i64 + offset;
    let today = local_now.div_euclid(86_400);
    let tod = i64::from(hour * 3600 + minute * 60);
    let day = match weekday {
        Some(w) => {
            let current = (today + 3).rem_euclid(7);
            let mut delta = (i64::from(w) - current).rem_euclid(7);
            if delta == 0 && today * 86_400 + tod <= local_now {
                delta = 7;
            }
            today + delta
        }
        None if today * 86_400 + tod <= local_now => today + 1,
        None => today,
    };
    u64::try_from(day * 86_400 + tod - offset).ok()
}

/// "3:45pm" / "11 AM" → (15, 45) / (11, 0).
fn parse_clock(s: &str) -> Option<(u32, u32)> {
    let s = s.trim().to_ascii_lowercase();
    let (num, pm) =
        if let Some(n) = s.strip_suffix("pm") { (n.trim(), true) } else { (s.strip_suffix("am")?.trim(), false) };
    let (h, m) = match num.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (num.parse::<u32>().ok()?, 0),
    };
    if !(1..=12).contains(&h) || m > 59 {
        return None;
    }
    Some((h % 12 + if pm { 12 } else { 0 }, m))
}

/// The percentage of a "You've used N% of your session|weekly limit" warning line, if `text` is
/// one. A warning never marks an account limited.
pub fn warning_percent(text: &str) -> Option<u8> {
    let p = Prepared::new(text);
    rules().iter().filter(|(r, _)| *r == Role::Warning).find_map(|(_, re)| {
        let caps = re.captures(&p.norm)?;
        let n: u32 = p.slice(caps.get(1).copied().flatten()?).parse().ok()?;
        u8::try_from(n).ok().filter(|n| *n <= 100)
    })
}

/// Look for a limit hit in the last [`TAIL_LINES`] complete lines of the last [`TAIL_BYTES`] of a
/// session transcript. A missing or unreadable file is `None`.
///
/// The newest assistant line decides: when it is a quota error the account is limited; when it is
/// an ordinary reply the session has moved on and older errors no longer count. A torn last line
/// (still being written) is skipped.
pub fn scan_transcript_tail(path: &Path) -> Option<LimitHit> {
    let now = now_secs();
    scan_tail_at(path, now, local_offset_secs(now))
}

fn scan_tail_at(path: &Path, now: u64, offset: i64) -> Option<LimitHit> {
    let tail = read_tail(path)?;
    let mut lines: Vec<&[u8]> = tail.split(|&b| b == b'\n').collect();
    // The piece after the last newline is incomplete (or empty).
    lines.pop();
    lines.retain(|l| !l.trim_ascii().is_empty());
    let start = lines.len().saturating_sub(TAIL_LINES);
    for line in lines[start..].iter().rev() {
        let Ok(v) = serde_json::from_slice::<Value>(line.trim_ascii()) else { continue };
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        return transcript_hit(&v, line, now, offset);
    }
    None
}

/// The last [`TAIL_BYTES`] of a file, starting after the first newline when the read begins
/// mid-file.
fn read_tail(path: &Path) -> Option<Vec<u8>> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let from = len.saturating_sub(TAIL_BYTES);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = Vec::with_capacity((len - from) as usize);
    f.take(TAIL_BYTES).read_to_end(&mut buf).ok()?;
    if from > 0 {
        let cut = buf.iter().position(|&b| b == b'\n').map_or(buf.len(), |i| i + 1);
        buf.drain(..cut);
    }
    Some(buf)
}

/// The transcript rule: an assistant API-error line that is `usage_limit_reached`, or a
/// `rate_limit` error whose text is a quota hit and not a throttle.
fn transcript_hit(v: &Value, raw: &[u8], now: u64, offset: i64) -> Option<LimitHit> {
    if v.get("isApiErrorMessage").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let text = message_text(v);
    let p = Prepared::new(&text);
    let usage = v.get("apiError").and_then(Value::as_str) == Some("usage_limit_reached");
    let rate = v.get("error").and_then(Value::as_str) == Some("rate_limit");
    if !(usage || (rate && !p.any(Role::Throttle) && p.any(Role::Quota))) {
        return None;
    }
    let raw = Prepared::new(&String::from_utf8_lossy(raw));
    let mut hit = hit_from(&p, Some(&raw), now, offset);
    hit.source = LimitSource::Transcript;
    Some(hit)
}

/// The text of a transcript entry's message: a string, or its text blocks joined by newlines.
fn message_text(v: &Value) -> String {
    match v.pointer("/message/content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// An account's limited state, kept until the limit resets.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Limited {
    pub kind: LimitKind,
    /// When the limit was first seen (unix seconds).
    pub since: u64,
    /// When it resets, if known (unix seconds).
    pub until: Option<u64>,
    #[serde(rename = "resetText")]
    pub reset_text: Option<String>,
    pub source: LimitSource,
}

impl Limited {
    /// The limited state a hit seen at `now` starts.
    pub fn from_hit(hit: &LimitHit, now: u64) -> Limited {
        Limited { kind: hit.kind, since: now, until: hit.until, reset_text: hit.reset_text.clone(), source: hit.source }
    }

    /// True once the limit has reset: `until` has passed, or, when it is unknown, the fallback
    /// window for its kind (5 h for a session or model limit, 7 days for a weekly one) has.
    pub fn expired(&self, now: u64) -> bool {
        match self.until {
            Some(until) => now >= until,
            None => {
                let window = match self.kind {
                    LimitKind::Weekly => FALLBACK_WEEKLY_SECS,
                    LimitKind::Session | LimitKind::Model => FALLBACK_SESSION_SECS,
                };
                now >= self.since.saturating_add(window)
            }
        }
    }
}

fn now_secs() -> u64 {
    crate::util::log::now_millis() / 1000
}

/// The machine's current offset from UTC in seconds (local = UTC + offset); 0 when unknown.
fn local_offset_secs(now: u64) -> i64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
    const EPOCH_DIFF_SECS: u64 = 11_644_473_600;
    let ticks = (now + EPOCH_DIFF_SECS) * 10_000_000;
    let utc = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let mut local = FILETIME::default();
    // SAFETY: both pointers refer to live FILETIME values for the duration of the call.
    if unsafe { FileTimeToLocalFileTime(&utc, &mut local) }.is_err() {
        return 0;
    }
    let local_ticks = (u64::from(local.dwHighDateTime) << 32) | u64::from(local.dwLowDateTime);
    (local_ticks as i64 - ticks as i64) / 10_000_000
}

/// A small backtracking matcher for the patterns in [`RULES`]: literals, `.`-free character
/// classes (`[a-z]`, `[^)]`), `\d`, `\s`, escaped literals, groups (`(...)`, `(?:...)`) with
/// alternation, the quantifiers `?`, `*`, `+`, `{n}`, `{m,n}`, and the anchors `^` and `$`.
/// Matching is ASCII case-insensitive and leftmost, with greedy quantifiers.
mod rx {
    const UNBOUNDED: usize = usize::MAX;

    /// Capture spans in characters; index 0 is the whole match.
    pub type Caps = Vec<Option<(usize, usize)>>;

    #[derive(Debug)]
    enum Node {
        Lit(char),
        Class(Vec<(char, char)>, bool),
        Digit,
        Space,
        Start,
        End,
        Group(Vec<Vec<Piece>>, Option<usize>),
    }

    #[derive(Debug)]
    struct Piece {
        node: Node,
        min: usize,
        max: usize,
    }

    #[derive(Debug)]
    pub struct Regex {
        alts: Vec<Vec<Piece>>,
        groups: usize,
    }

    impl Regex {
        /// Compile a pattern. The patterns are constants of this module's caller, so a malformed
        /// one is a programming error.
        pub fn new(pat: &str) -> Regex {
            let chars: Vec<char> = pat.chars().collect();
            let mut p = Parser { c: &chars, i: 0, groups: 0 };
            let alts = p.alts();
            assert!(p.i == chars.len(), "unbalanced pattern: {pat}");
            Regex { alts, groups: p.groups }
        }

        /// The leftmost match in `text`, with its capture groups.
        pub fn captures(&self, text: &[char]) -> Option<Caps> {
            let m = Matcher { t: text };
            for start in 0..=text.len() {
                let mut caps: Caps = vec![None; self.groups + 1];
                let mut end = 0;
                if m.alts(&self.alts, start, &mut caps, &mut |e, _| {
                    end = e;
                    true
                }) {
                    caps[0] = Some((start, end));
                    return Some(caps);
                }
            }
            None
        }
    }

    struct Parser<'a> {
        c: &'a [char],
        i: usize,
        groups: usize,
    }

    impl Parser<'_> {
        fn peek(&self) -> Option<char> {
            self.c.get(self.i).copied()
        }

        fn next(&mut self) -> char {
            let c = self.c[self.i];
            self.i += 1;
            c
        }

        fn alts(&mut self) -> Vec<Vec<Piece>> {
            let mut alts = vec![self.seq()];
            while self.peek() == Some('|') {
                self.i += 1;
                alts.push(self.seq());
            }
            alts
        }

        fn seq(&mut self) -> Vec<Piece> {
            let mut seq = Vec::new();
            while let Some(c) = self.peek() {
                if c == '|' || c == ')' {
                    break;
                }
                let node = self.atom();
                let (min, max) = self.quant();
                seq.push(Piece { node, min, max });
            }
            seq
        }

        fn atom(&mut self) -> Node {
            match self.next() {
                '^' => Node::Start,
                '$' => Node::End,
                '(' => {
                    let cap = if self.c[self.i..].starts_with(&['?', ':']) {
                        self.i += 2;
                        None
                    } else {
                        self.groups += 1;
                        Some(self.groups)
                    };
                    let alts = self.alts();
                    assert_eq!(self.peek(), Some(')'), "unclosed group");
                    self.i += 1;
                    Node::Group(alts, cap)
                }
                '[' => self.class(),
                '\\' => match self.next() {
                    'd' => Node::Digit,
                    's' => Node::Space,
                    c => Node::Lit(c),
                },
                c => Node::Lit(c),
            }
        }

        fn class(&mut self) -> Node {
            let neg = self.peek() == Some('^');
            if neg {
                self.i += 1;
            }
            let mut ranges = Vec::new();
            loop {
                let mut lo = self.next();
                if lo == ']' {
                    break;
                }
                if lo == '\\' {
                    lo = self.next();
                }
                if self.peek() == Some('-') && self.c.get(self.i + 1).is_some_and(|&n| n != ']') {
                    self.i += 1;
                    let mut hi = self.next();
                    if hi == '\\' {
                        hi = self.next();
                    }
                    ranges.push((lo, hi));
                } else {
                    ranges.push((lo, lo));
                }
            }
            Node::Class(ranges, neg)
        }

        fn number(&mut self) -> usize {
            let mut n = 0;
            while let Some(d) = self.peek().and_then(|c| c.to_digit(10)) {
                n = n * 10 + d as usize;
                self.i += 1;
            }
            n
        }

        fn quant(&mut self) -> (usize, usize) {
            let q = match self.peek() {
                Some('?') => (0, 1),
                Some('*') => (0, UNBOUNDED),
                Some('+') => (1, UNBOUNDED),
                Some('{') => {
                    self.i += 1;
                    let min = self.number();
                    let max = if self.peek() == Some(',') {
                        self.i += 1;
                        if self.peek() == Some('}') {
                            UNBOUNDED
                        } else {
                            self.number()
                        }
                    } else {
                        min
                    };
                    assert_eq!(self.peek(), Some('}'), "unclosed quantifier");
                    (min, max)
                }
                _ => return (1, 1),
            };
            self.i += 1;
            q
        }
    }

    type Cont<'k> = &'k mut dyn FnMut(usize, &mut Caps) -> bool;

    struct Matcher<'a> {
        t: &'a [char],
    }

    impl Matcher<'_> {
        fn alts(&self, alts: &[Vec<Piece>], pos: usize, caps: &mut Caps, k: Cont<'_>) -> bool {
            alts.iter().any(|seq| self.seq(seq, pos, caps, &mut *k))
        }

        fn seq(&self, seq: &[Piece], pos: usize, caps: &mut Caps, k: Cont<'_>) -> bool {
            match seq.split_first() {
                None => k(pos, caps),
                Some((p, rest)) => self.piece(p, 0, pos, caps, &mut |np, caps| self.seq(rest, np, caps, &mut *k)),
            }
        }

        fn piece(&self, p: &Piece, count: usize, pos: usize, caps: &mut Caps, k: Cont<'_>) -> bool {
            if count < p.max {
                let more = self.node(&p.node, pos, caps, &mut |np, caps| {
                    // An empty repetition beyond the minimum would loop forever.
                    if np == pos && count >= p.min {
                        return false;
                    }
                    self.piece(p, count + 1, np, caps, &mut *k)
                });
                if more {
                    return true;
                }
            }
            count >= p.min && k(pos, caps)
        }

        fn node(&self, n: &Node, pos: usize, caps: &mut Caps, k: Cont<'_>) -> bool {
            match n {
                Node::Start => pos == 0 && k(pos, caps),
                Node::End => pos == self.t.len() && k(pos, caps),
                Node::Group(alts, None) => self.alts(alts, pos, caps, k),
                Node::Group(alts, Some(g)) => {
                    let g = *g;
                    self.alts(alts, pos, caps, &mut |np, caps| {
                        let old = caps[g];
                        caps[g] = Some((pos, np));
                        if k(np, caps) {
                            return true;
                        }
                        caps[g] = old;
                        false
                    })
                }
                single => match self.t.get(pos) {
                    Some(&c) if accepts(single, c) => k(pos + 1, caps),
                    _ => false,
                },
            }
        }
    }

    fn accepts(n: &Node, c: char) -> bool {
        match n {
            Node::Lit(l) => l.eq_ignore_ascii_case(&c),
            Node::Digit => c.is_ascii_digit(),
            Node::Space => c.is_whitespace(),
            Node::Class(ranges, neg) => {
                let hit = [c, c.to_ascii_lowercase(), c.to_ascii_uppercase()]
                    .iter()
                    .any(|x| ranges.iter().any(|&(lo, hi)| lo <= *x && *x <= hi));
                hit != *neg
            }
            _ => false,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn find(pat: &str, text: &str) -> Option<Vec<Option<String>>> {
            let t: Vec<char> = text.chars().collect();
            Regex::new(pat)
                .captures(&t)
                .map(|caps| caps.iter().map(|c| c.map(|(a, b)| t[a..b].iter().collect())).collect())
        }

        #[test]
        fn engine_basics() {
            assert_eq!(find("b+", "aabbbc").unwrap()[0].as_deref(), Some("bbb"));
            assert_eq!(find("^abc$", "ABC").unwrap()[0].as_deref(), Some("ABC"));
            assert!(find("^abc$", "xabc").is_none());
            assert_eq!(find(r"(\d{2,3})x", "1234x").unwrap()[1].as_deref(), Some("234"));
            assert_eq!(find(r"a(?:b|c)?d", "ad").unwrap()[0].as_deref(), Some("ad"));
            assert_eq!(find(r"\(([^)]+)\)", "x (a/b) (c)").unwrap()[1].as_deref(), Some("a/b"));
            assert_eq!(find(r"[A-Z0-9 .\-]{1,5} z", "q-1. z").unwrap()[0].as_deref(), Some("q-1. z"));
            assert_eq!(find(r"(x)?y", "y").unwrap()[1], None);
            assert!(find(r"a{3}", "aa").is_none());
            assert_eq!(find(r"a\s*b", "a \t b").unwrap()[0].as_deref(), Some("a \t b"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-27 10:00:00 UTC, a Tuesday.
    const NOW: u64 = 1_793_095_200;
    /// A machine two hours ahead of UTC.
    const OFFSET: i64 = 2 * 3600;

    fn hit(text: &str) -> Option<LimitHit> {
        classify_at(text, NOW, OFFSET)
    }

    fn limited(text: &str) -> LimitHit {
        hit(text).unwrap_or_else(|| panic!("not recognised as a limit: {text}"))
    }

    fn zone(text: &str) -> Option<String> {
        let p = Prepared::new(text);
        parse_reset(&p, NOW, OFFSET).and_then(|r| r.zone)
    }

    #[test]
    fn now_constant_is_a_tuesday_at_ten_utc() {
        assert_eq!(NOW % 86_400, 10 * 3600);
        assert_eq!((NOW / 86_400 + 3) % 7, 1);
    }

    #[test]
    fn session_limit_with_clock() {
        let h = limited("You've hit your session limit · resets 3:45pm");
        assert_eq!((h.kind, h.reset_text.as_deref()), (LimitKind::Session, Some("3:45pm")));
        // 15:45 local (UTC+2) today is 13:45 UTC.
        assert_eq!(h.until, Some(NOW + 3 * 3600 + 45 * 60));
        assert_eq!(h.source, LimitSource::HookStopFailure);
    }

    #[test]
    fn weekly_limit_with_weekday() {
        let h = limited("You've hit your weekly limit · resets Mon 12:00am");
        assert_eq!((h.kind, h.reset_text.as_deref()), (LimitKind::Weekly, Some("Mon 12:00am")));
        // Next Monday 00:00 local = Sunday 22:00 UTC, 5 days and 12 hours away.
        assert_eq!(h.until, Some(NOW + 5 * 86_400 + 12 * 3600));
    }

    #[test]
    fn model_limit() {
        assert_eq!(limited("You've hit your Opus limit · resets 3:45pm").kind, LimitKind::Model);
    }

    #[test]
    fn named_zone_keeps_text_and_falls_back() {
        let text = "You've hit your session limit · resets 5:20pm (Asia/Singapore)";
        let h = limited(text);
        assert_eq!(zone(text).as_deref(), Some("Asia/Singapore"));
        assert_eq!(h.reset_text.as_deref(), Some("5:20pm (Asia/Singapore)"));
        assert_eq!(h.until, None);
    }

    #[test]
    fn weekly_limit_in_utc() {
        let text = "You've hit your weekly limit · resets 11am (UTC)";
        let h = limited(text);
        assert_eq!(h.kind, LimitKind::Weekly);
        assert_eq!(zone(text).as_deref(), Some("UTC"));
        assert_eq!(h.until, Some(NOW + 3600));
    }

    #[test]
    fn five_hour_limit_with_bullet_operator() {
        let h = limited("5-hour limit reached ∙ resets 9pm");
        assert_eq!((h.kind, h.reset_text.as_deref()), (LimitKind::Session, Some("9pm")));
    }

    #[test]
    fn five_hour_limit_with_upgrade_hint() {
        let h = limited(
            "5-hour limit reached · resets 3pm (Europe/Stockholm) · /upgrade to Max 20x or turn on /extra-usage",
        );
        assert_eq!(h.kind, LimitKind::Session);
    }

    #[test]
    fn oldest_form_with_epoch() {
        let h = limited("Claude AI usage limit reached|1749924000");
        assert_eq!((h.until, h.reset_text), (Some(1_749_924_000), None));
    }

    #[test]
    fn will_reset_at_fixed_zone() {
        let text = "Claude usage limit reached. Your limit will reset at 1pm (Etc/GMT+5).";
        let h = limited(text);
        assert_eq!(h.reset_text.as_deref(), Some("1pm (Etc/GMT+5)"));
        assert_eq!(zone(text).as_deref(), Some("Etc/GMT+5"));
        // 13:00 at UTC-5 is 18:00 UTC.
        assert_eq!(h.until, Some(NOW + 8 * 3600));
    }

    #[test]
    fn continuing_automatically() {
        let h = limited("Usage limit reached · continuing automatically at 5:50pm · esc to cancel");
        assert_eq!(h.reset_text.as_deref(), Some("5:50pm"));
    }

    #[test]
    fn agent_terminated_early() {
        let h = limited("Agent terminated early due to an API error: You've hit your session limit · resets 1pm (Europe/Berlin) (error type rate_limit, HTTP 429)");
        assert_eq!(h.kind, LimitKind::Session);
    }

    #[test]
    fn warning_is_not_a_limit() {
        let text = "You've used 85% of your session limit · resets 3:45pm";
        assert_eq!(warning_percent(text), Some(85));
        assert_eq!(hit(text), None);
    }

    #[test]
    fn server_throttle_is_not_a_limit() {
        assert_eq!(
            hit("API Error: Server is temporarily limiting requests (not your usage limit) · Rate limited"),
            None
        );
    }

    #[test]
    fn client_rate_limit_is_not_a_limit() {
        assert_eq!(hit("API Error: Rate limit reached"), None);
    }

    #[test]
    fn loose_matching() {
        assert!(hit("YOU\u{2019}VE HIT YOUR SESSION LIMIT | RESETS 3:45PM").is_some());
        assert_eq!(limited("You've hit your session limit | resets 3:45 PM").reset_text.as_deref(), Some("3:45 PM"));
        assert_eq!(hit("Everything is fine"), None);
        assert_eq!(warning_percent("Everything is fine"), None);
        assert_eq!(warning_percent("You've used 250% of your weekly limit"), None);
        let h = limited(r#"Usage limit reached {"resetsAt": 1749924000123}"#);
        assert_eq!(h.until, Some(1_749_924_000));
    }

    #[test]
    fn clock_arithmetic() {
        assert_eq!(parse_clock("12am"), Some((0, 0)));
        assert_eq!(parse_clock("12:30pm"), Some((12, 30)));
        assert_eq!(parse_clock("13pm"), None);
        assert_eq!(parse_clock("3:75am"), None);
        // A time already past today is tomorrow.
        assert_eq!(clock_until("9am", None, Some("UTC"), NOW, 0), Some(NOW + 23 * 3600));
        // Today's weekday with the time still ahead is today.
        assert_eq!(clock_until("11am", Some(1), Some("UTC"), NOW, 0), Some(NOW + 3600));
        assert_eq!(clock_until("9am", Some(1), Some("UTC"), NOW, 0), Some(NOW + 7 * 86_400 - 3600));
        assert_eq!(fixed_zone_offset("Etc/GMT-3"), Some(3 * 3600));
        assert_eq!(fixed_zone_offset("Etc/UTC"), Some(0));
        assert_eq!(fixed_zone_offset("Europe/Berlin"), None);
        assert_eq!(clock_until("1pm", None, Some("Europe/Berlin"), NOW, 0), None);
    }

    #[test]
    fn expiry() {
        let mut l = Limited::from_hit(&limited("You've hit your session limit · resets 3:45pm"), NOW);
        let until = l.until.unwrap();
        assert!(!l.expired(until - 1));
        assert!(l.expired(until));
        l.until = None;
        assert!(!l.expired(NOW + FALLBACK_SESSION_SECS - 1));
        assert!(l.expired(NOW + FALLBACK_SESSION_SECS));
        l.kind = LimitKind::Weekly;
        assert!(!l.expired(NOW + FALLBACK_SESSION_SECS));
        assert!(l.expired(NOW + FALLBACK_WEEKLY_SECS));
        let json = serde_json::to_value(&l).unwrap();
        assert_eq!(json["kind"], "weekly");
        assert_eq!(json["source"], "hookStopFailure");
        assert!(json.get("resetText").is_some());
    }

    fn assistant_error(text: &str, api_error: Option<&str>) -> String {
        let mut v = serde_json::json!({
            "type": "assistant",
            "message": {"model": "<synthetic>", "role": "assistant", "stop_reason": "stop_sequence",
                        "content": [{"type": "text", "text": text}]},
            "error": "rate_limit", "isApiErrorMessage": true, "apiErrorStatus": 429
        });
        if let Some(e) = api_error {
            v["apiError"] = e.into();
        }
        v.to_string()
    }

    fn write_transcript(lines: &[String], torn: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let mut body = String::new();
        for l in lines {
            body.push_str(l);
            body.push('\n');
        }
        body.push_str(torn);
        std::fs::write(d.path().join("t.jsonl"), body).unwrap();
        d
    }

    #[test]
    fn transcript_tail() {
        let user = r#"{"type":"user","message":{"role":"user","content":"hi"}}"#.to_string();
        let reply = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Done."}]}}"#
            .to_string();
        let quota = assistant_error(
            "You've hit your session limit · resets 5:20pm (Asia/Singapore)",
            Some("usage_limit_reached"),
        );
        let throttle = assistant_error(
            "API Error: Server is temporarily limiting requests (not your usage limit) · Rate limited",
            None,
        );
        let rate_quota = assistant_error("5-hour limit reached · resets 9pm", None);

        let d = write_transcript(&[user.clone(), quota.clone()], r#"{"type":"assist"#);
        let h = scan_tail_at(&d.path().join("t.jsonl"), NOW, OFFSET).unwrap();
        assert_eq!(h.source, LimitSource::Transcript);
        assert_eq!(h.reset_text.as_deref(), Some("5:20pm (Asia/Singapore)"));

        let d = write_transcript(&[user.clone(), rate_quota], "");
        assert_eq!(scan_tail_at(&d.path().join("t.jsonl"), NOW, OFFSET).unwrap().reset_text.as_deref(), Some("9pm"));

        let d = write_transcript(&[user.clone(), throttle], "");
        assert_eq!(scan_tail_at(&d.path().join("t.jsonl"), NOW, OFFSET), None);

        // The session moved on after the limit.
        let d = write_transcript(&[quota.clone(), user.clone(), reply], "");
        assert_eq!(scan_tail_at(&d.path().join("t.jsonl"), NOW, OFFSET), None);

        // Only the last lines count.
        let mut many = vec![quota.clone()];
        many.extend(std::iter::repeat_n(user.clone(), TAIL_LINES));
        let d = write_transcript(&many, "");
        assert_eq!(scan_tail_at(&d.path().join("t.jsonl"), NOW, OFFSET), None);

        // A long file: only the tail is read, and the cut line at its start is skipped.
        let filler = format!(r#"{{"type":"user","message":{{"content":"{}"}}}}"#, "x".repeat(4000));
        let mut long = std::iter::repeat_n(filler, 40).collect::<Vec<_>>();
        long.push(quota);
        let d = write_transcript(&long, "");
        assert!(scan_tail_at(&d.path().join("t.jsonl"), NOW, OFFSET).is_some());

        assert_eq!(scan_tail_at(&d.path().join("missing.jsonl"), NOW, OFFSET), None);
    }

    #[test]
    fn local_offset_is_sane() {
        let o = local_offset_secs(NOW);
        assert!((-14 * 3600..=14 * 3600).contains(&o));
    }
}
