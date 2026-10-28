/*
 * Copyright (c) 2026 joyal-droid. SPDX-License-Identifier: CPAL-1.0
 *
 * The contents of this file are subject to the Common Public Attribution License Version 1.0
 * (the "License"); you may not use this file except in compliance with the License. You may
 * obtain a copy of the License in the LICENSE file at the root of this repository or at
 * https://opensource.org/license/cpal-1-0. The License is based on the Mozilla Public License
 * Version 1.1 but Sections 14 and 15 have been added to cover use of software over a computer
 * network and provide for limited attribution for the Original Developer. In addition, Exhibit A
 * has been modified to be consistent with Exhibit B.
 *
 * Software distributed under the License is distributed on an "AS IS" basis, WITHOUT WARRANTY OF
 * ANY KIND, either express or implied. See the License for the specific language governing rights
 * and limitations under the License.
 *
 * The Original Code is Sessionmux. The Initial Developer of the Original Code is joyal-droid. All
 * portions of the code written by joyal-droid are Copyright (c) 2026 joyal-droid. All Rights Reserved.
 * Contributor(s): see the git history of this repository.
 *
 * Attribution (Exhibit B of the License): "Powered by Sessionmux", Copyright (c) 2026 joyal-droid,
 * https://github.com/joyal-droid/sessionmux and the Sessionmux mark must stay visible in the user
 * interface of every copy and derivative; see docs/attribution.md.
 */

/*
 * Sessionmux — panel renderer.
 *
 * Runs in WebView2 without any host objects and talks to the app only through
 * window.sessionmux (web/shim.js); window.sessionmux.onShowTab(cb) selects the tab the tray,
 * hotkey or a second launch asks for.
 *
 * The file has two halves:
 *   1. Pure helpers (no DOM) — exported via module.exports when loaded by Node
 *      so they can be unit-tested; ignored in the browser.
 *   2. The panel UI, started only when a DOM is present.
 *
 * All user-controlled strings (account names, session titles, paths) are put
 * into the DOM as text nodes, never via innerHTML. innerHTML is used only for
 * the constant icon SVGs below.
 */
'use strict';

/* ======================================================================== */
/* 1. Pure helpers                                                          */
/* ======================================================================== */

/** The repository; the fallback for getAppInfo().homeUrl until it arrives. */
const REPO_URL = 'https://github.com/joyal-droid/sessionmux';

/** The license attribution (CPAL-1.0 Exhibit B), shown in the About card of every build. */
const ATTRIBUTION = 'Powered by Sessionmux · Copyright (c) 2026 joyal-droid';

/** Account colour palette (same order as the store's; also the add-account swatches). */
const PALETTE = ['#F08A5D', '#5B9CF6', '#3DD6A1', '#B388FF', '#F6C453', '#F2709C', '#4FD1DB', '#9BA3B5'];

/** Account name rule: 1-32 characters, letters, digits, space, dot, dash, underscore; starts alphanumeric. */
const NAME_RE = /^[A-Za-z0-9][A-Za-z0-9 ._-]{0,31}$/;

/** Accepts ms epoch numbers, numeric strings or ISO strings; returns ms or NaN. */
function toMs(ts) {
  if (typeof ts === 'number') return ts;
  if (typeof ts === 'string' && ts.trim() !== '') {
    if (/^\d+$/.test(ts.trim())) return Number(ts);
    return Date.parse(ts);
  }
  return NaN;
}

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

/**
 * Compact relative time: "just now", "5 min ago", "2 h ago", "yesterday",
 * "3 d ago", then a short date ("Mar 3", or "Mar 3, 2024" in another year).
 */
function relativeTime(ts, now) {
  const t = toMs(ts);
  if (!Number.isFinite(t) || t <= 0) return '';
  const n = typeof now === 'number' ? now : Date.now();
  const diff = Math.max(0, n - t); // clock skew: never show "in the future"
  const min = 60 * 1000;
  const hour = 60 * min;
  const day = 24 * hour;
  if (diff < 45 * 1000) return 'just now';
  if (diff < hour) return Math.max(1, Math.round(diff / min)) + ' min ago';
  if (diff < day) return Math.floor(diff / hour) + ' h ago';
  if (diff < 2 * day) return 'yesterday';
  if (diff < 7 * day) return Math.floor(diff / day) + ' d ago';
  const d = new Date(t);
  const sameYear = d.getFullYear() === new Date(n).getFullYear();
  return MONTHS[d.getMonth()] + ' ' + d.getDate() + (sameYear ? '' : ', ' + d.getFullYear());
}

/** Absolute timestamp for tooltips. */
function absoluteTime(ts) {
  const t = toMs(ts);
  if (!Number.isFinite(t) || t <= 0) return '';
  try {
    return new Date(t).toLocaleString();
  } catch (_) {
    return new Date(t).toISOString();
  }
}

/** Last path segment of a Windows or POSIX path ("C:\a\b\" → "b", "C:\" → "C:\"). */
function cwdLeaf(cwd) {
  if (typeof cwd !== 'string' || cwd === '') return '';
  const trimmed = cwd.replace(/[\\/]+$/, '');
  if (trimmed === '') return cwd;
  if (/^[A-Za-z]:$/.test(trimmed)) return trimmed + '\\';
  const parts = trimmed.split(/[\\/]/);
  return parts[parts.length - 1] || trimmed;
}

/**
 * Short model badge text: "claude-opus-4-5-20251101" → "Opus 4.5",
 * "claude-sonnet-4-20250514" → "Sonnet 4". Unknown ids are shortened.
 */
function modelLabel(model) {
  if (typeof model !== 'string' || model === '') return '';
  const m = /(opus|sonnet|haiku|fable)[-_ ]?(\d+)(?:[-.](\d{1,2}))?(?!\d)/i.exec(model);
  if (m) {
    const fam = m[1][0].toUpperCase() + m[1].slice(1).toLowerCase();
    return fam + ' ' + m[2] + (m[3] ? '.' + m[3] : '');
  }
  const bare = /^(opus|sonnet|haiku|fable)$/i.exec(model.trim());
  if (bare) return bare[1][0].toUpperCase() + bare[1].slice(1).toLowerCase();
  const short = model.replace(/^claude-/i, '');
  return short.length > 16 ? short.slice(0, 15) + '…' : short;
}

/** Up to 2 uppercase letters from the first two words. */
function initialsOf(name) {
  const words = String(name || '').trim().split(/[\s._-]+/).filter(Boolean);
  if (words.length === 0) return '?';
  if (words.length === 1) return words[0].slice(0, 2).toUpperCase();
  return (words[0][0] + words[1][0]).toUpperCase();
}

/**
 * Comparison key for account names: trimmed, internal whitespace
 * collapsed to one space, lower-cased. "  Work  Mail " ≡ "work mail".
 */
function nameKey(name) {
  return String(name == null ? '' : name).trim().replace(/\s+/g, ' ').toLowerCase();
}

/** The existing account whose name clashes with `name` (ignoring `excludeId`), or null. */
function findNameClash(name, accounts, excludeId) {
  const key = nameKey(name);
  if (!key) return null;
  return (accounts || []).find((a) => a && a.id !== excludeId && nameKey(a.name) === key) || null;
}

/** Same text the app returns with reason 'name-taken'. */
function nameTakenMessage(existingName) {
  return 'An account named "' + existingName + '" already exists.';
}

/**
 * Validate an account name against NAME_RE. Returns '' when valid, otherwise
 * a human-readable message. `excludeId` skips the account being renamed.
 */
function validateName(name, accounts, excludeId) {
  const v = String(name || '').trim();
  if (v === '') return 'Give the account a name.';
  if (v.length > 32) return 'Keep it to 32 characters or fewer.';
  if (!/^[A-Za-z0-9]/.test(v)) return 'Start with a letter or a number.';
  if (!NAME_RE.test(v)) return 'Use letters, numbers, spaces, dots, dashes or underscores.';
  const clash = findNameClash(v, accounts, excludeId);
  if (clash) return nameTakenMessage(clash.name);
  return '';
}

/** Case-insensitive match of a session against a search query (title or cwd). */
function matchesQuery(session, query) {
  const q = String(query || '').trim().toLowerCase();
  if (!q) return true;
  const hay = ((session && session.title) || '') + '\n' + ((session && session.cwd) || '');
  return q.split(/\s+/).every((term) => hay.toLowerCase().includes(term));
}

/**
 * Move `movingId` before or after `targetId` in `ids`. Returns a new array
 * (or the original order when either id is unknown or they are equal).
 */
function reorderIds(ids, movingId, targetId, after) {
  const list = ids.slice();
  if (movingId === targetId) return list;
  const from = list.indexOf(movingId);
  if (from < 0 || list.indexOf(targetId) < 0) return list;
  list.splice(from, 1);
  let to = list.indexOf(targetId);
  if (after) to += 1;
  list.splice(to, 0, movingId);
  return list;
}

/** First palette colour not used by an existing account (round-robin fallback). */
function nextColor(accounts) {
  const used = new Set((accounts || []).map((a) => String(a.color || '').toUpperCase()));
  const free = PALETTE.find((c) => !used.has(c.toUpperCase()));
  return free || PALETTE[(accounts || []).length % PALETTE.length];
}

/**
 * Default account name for a found profile folder: "Claude-work" → "work",
 * coerced to the name rules. Never auto-suffixed: if it
 * clashes with an existing account the add form flags it and asks for
 * another name. `accounts` is accepted for call-site compatibility only.
 */
function defaultProfileName(folderName, accounts) { // eslint-disable-line no-unused-vars
  let base = String(folderName || '').replace(/^Claude-/i, '').replace(/[^A-Za-z0-9 ._-]/g, '-');
  base = base.replace(/^[^A-Za-z0-9]+/, '').slice(0, 32).trim();
  if (!base) base = 'Profile';
  return base;
}

/** Same text the app returns for adoptProfile with reason 'same-sign-in'. */
function sameSignInMessage(name) {
  return 'This folder is signed in to the same claude.ai account as ' + name + '.';
}

/** macOS modifier symbols in the order menus show them, with the accelerator names for each. */
const MAC_MODIFIERS = [
  ['⌃', /^(control|ctrl)$/i],
  ['⌥', /^(option|alt)$/i],
  ['⇧', /^shift$/i],
  ['⌘', /^(commandorcontrol|cmdorctrl|super|meta|command|cmd)$/i],
];

/** Spoken names of the macOS modifier symbols, for screen readers. */
const MAC_KEY_NAMES = { '⌃': 'Control', '⌥': 'Option', '⇧': 'Shift', '⌘': 'Command' };

/**
 * Split an accelerator into display keys ("CommandOrControl+Shift+Space" → ["Ctrl","Shift","Space"]).
 * With `mac` the modifiers become the symbols ⌃ ⌥ ⇧ ⌘ in menu order and CommandOrControl is
 * Command ("CommandOrControl+Shift+Space" → ["⇧","⌘","Space"]).
 */
function hotkeyParts(accel, mac) {
  if (typeof accel !== 'string' || accel.trim() === '') return [];
  const keys = accel.split('+').map((k) => k.trim()).filter(Boolean);
  const name = (k) => (k.length === 1 ? k.toUpperCase() : k[0].toUpperCase() + k.slice(1));
  if (mac) {
    const mods = MAC_MODIFIERS.filter(([, re]) => keys.some((k) => re.test(k))).map(([sym]) => sym);
    const rest = keys.filter((k) => !MAC_MODIFIERS.some(([, re]) => re.test(k))).map(name);
    return mods.concat(rest);
  }
  return keys.map((k) => {
    if (/^(commandorcontrol|cmdorctrl|control|ctrl)$/i.test(k)) return 'Ctrl';
    if (/^(super|meta|command|cmd)$/i.test(k)) return 'Win';
    if (/^(option|alt)$/i.test(k)) return 'Alt';
    return name(k);
  });
}

/**
 * Turn a transferSession() result into toast content.
 * Returns { kind: 'success'|'warn'|'error', title, body }.
 */
function transferMessage(res, mode, targetName, sourceName) {
  const verb = mode === 'move' ? 'Moved' : 'Copied';
  if (res && res.ok) {
    const notes = [];
    const warnings = Array.isArray(res.warnings) ? res.warnings : [];
    const sourceOpen = sourceRunning(res);
    const source = sourceName || 'the other account';
    if (mode === 'move' && sourceOpen) notes.push('The original is still open in ' + source + '; close it there when you are done.');
    else if (sourceOpen) notes.push('It was copied as it was just now; anything Claude was still writing is not in the copy.');
    if (res.targetRunning) notes.push('Restart ' + targetName + "'s Claude window to see it.");
    if (warnings.includes('no-transcript')) notes.push('No transcript was found, so only the session entry came across.');
    if (warnings.includes('source-not-removed')) notes.push('The original entry could not be removed; delete it by hand if you no longer need it.');
    if (warnings.includes('scratch-not-removed')) notes.push("The original scratch folder couldn't be removed.");
    const skipped = skippedFiles(warnings);
    if (skipped > 0) notes.push(plural(skipped, 'file') + (skipped === 1 ? ' was' : ' were') + ' in use and could not be copied.');
    // Leaving a running original open is the expected outcome of a move, not a problem.
    const problems = warnings.filter((w) => w !== 'source-left-open');
    return {
      kind: problems.length ? 'warn' : 'success',
      title: verb + ' to ' + targetName,
      body: notes.join(' '),
    };
  }
  const reason = res && res.reason;
  if (reason === 'running') {
    return { kind: 'error', title: "Can't " + mode + ' a running session', body: 'Close it in Claude first, then try again.' };
  }
  if (reason === 'target-never-opened-code') {
    return {
      kind: 'error',
      title: targetName + " hasn't used Code yet",
      body: 'Open the Code tab in ' + targetName + "'s Claude window once, then try again.",
    };
  }
  return {
    kind: 'error',
    title: (mode === 'move' ? 'Move' : 'Copy') + ' failed',
    body: (res && (res.error || reason)) || 'Something went wrong.',
  };
}

/** "1 file", "3 files". */
function plural(n, word) {
  return n + ' ' + word + (n === 1 ? '' : 's');
}

/** Whether a transfer result says the source session was still open (either spelling). */
function sourceRunning(res) {
  if (!res || typeof res !== 'object') return false;
  if (res.sourceRunning === true || res.source_running === true) return true;
  return Array.isArray(res.warnings) && res.warnings.includes('source-left-open');
}

/** Total of the `files-skipped:<n>` warnings of a transfer result. */
function skippedFiles(warnings) {
  let n = 0;
  for (const w of Array.isArray(warnings) ? warnings : []) {
    const m = /^files-skipped:(\d+)$/.exec(String(w));
    if (m) n += Number(m[1]);
  }
  return n;
}

/** Unix seconds or milliseconds → milliseconds (NaN when unusable). */
function unixMs(t) {
  const n = typeof t === 'string' && /^\d+$/.test(t.trim()) ? Number(t) : t;
  if (typeof n !== 'number' || !Number.isFinite(n) || n <= 0) return NaN;
  return n < 1e11 ? n * 1000 : n;
}

/** Local clock time "15:45" (or the locale's form) for a unix time; '' when unusable. */
function clockTime(t) {
  const ms = unixMs(t);
  if (!Number.isFinite(ms)) return '';
  try {
    return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  } catch (_) {
    const d = new Date(ms);
    return String(d.getHours()).padStart(2, '0') + ':' + String(d.getMinutes()).padStart(2, '0');
  }
}

/**
 * Badge text for an account's `limited` state: "Limited · resets 5:20pm (Asia/Singapore)" when the
 * message named the reset, "Limited until 15:45" when only the time is known, else "Limited".
 * '' when the account is not limited.
 */
function limitText(limited) {
  if (!limited || typeof limited !== 'object') return '';
  const reset = typeof limited.resetText === 'string' ? limited.resetText.trim() : '';
  if (reset) return 'Limited · ' + (/^resets?\b/i.test(reset) ? reset : 'resets ' + reset);
  const at = clockTime(limited.until);
  return at ? 'Limited until ' + at : 'Limited';
}

/** An account that can take a session: signed in and has opened the Code tab. */
function canReceive(a) {
  return !!a && !!a.accountUuid && a.signedIn !== false && a.codeReady !== false;
}

/**
 * Where "Hand over" sends a session of `sourceId`: the first account in panel order that can
 * take it and is not limited, or null.
 */
function handoverTarget(accounts, sourceId) {
  return (accounts || []).find((a) => a && a.id !== sourceId && canReceive(a) && !a.limited) || null;
}

/** The earliest known reset (unix ms) among the other accounts that could take a session, or null. */
function earliestReset(accounts, sourceId) {
  let best = null;
  for (const a of accounts || []) {
    if (!a || a.id === sourceId || !canReceive(a) || !a.limited) continue;
    const ms = unixMs(a.limited.until);
    if (Number.isFinite(ms) && (best === null || ms < best)) best = ms;
  }
  return best;
}

/** Badge text for a waiting session: "Waiting" for a permission prompt, "Needs you" otherwise. */
function waitingLabel(kind) {
  if (kind === 'permission') return 'Waiting';
  if (kind === 'idle' || kind === 'needs_input') return 'Needs you';
  return '';
}

/**
 * A folder shortened for a card: the whole path when it fits in `max` characters, else the root
 * and as many trailing folders as fit ("C:\…\work\app").
 */
function shortPath(cwd, max) {
  const limit = typeof max === 'number' && max > 8 ? max : 36;
  if (typeof cwd !== 'string' || cwd === '') return '';
  if (cwd.length <= limit) return cwd;
  const sep = cwd.includes('\\') ? '\\' : '/';
  const parts = cwd.replace(/[\\/]+$/, '').split(/[\\/]/);
  const root = parts[0] === '' ? '' : parts[0];
  let tail = parts[parts.length - 1] || '';
  for (let i = parts.length - 2; i > 0; i--) {
    const next = parts[i] + sep + tail;
    if ((root + sep + '…' + sep + next).length > limit) break;
    tail = next;
  }
  const out = root + sep + '…' + sep + tail;
  return out.length <= limit + 1 ? out : '…' + tail.slice(-(limit - 1));
}

/** Whether an approval summary is long enough to start collapsed behind "Show more". */
function summaryIsLong(text) {
  const s = String(text || '');
  return s.split('\n').length > 3 || s.length > 160;
}

/** How long an approve request stays answerable after it arrives. */
const APPROVAL_TTL_MS = 90 * 1000;

/** Unix ms at which an approval stops being answerable (NaN when its arrival time is unknown). */
function approvalExpiresAt(a) {
  const t = unixMs(a && a.receivedAt);
  return Number.isFinite(t) ? t + APPROVAL_TTL_MS : NaN;
}

if (typeof module === 'object' && module && module.exports) {
  module.exports = {
    PALETTE,
    relativeTime,
    absoluteTime,
    cwdLeaf,
    modelLabel,
    initialsOf,
    validateName,
    nameKey,
    findNameClash,
    nameTakenMessage,
    sameSignInMessage,
    matchesQuery,
    reorderIds,
    nextColor,
    defaultProfileName,
    hotkeyParts,
    MAC_KEY_NAMES,
    transferMessage,
    sourceRunning,
    skippedFiles,
    unixMs,
    clockTime,
    limitText,
    handoverTarget,
    earliestReset,
    waitingLabel,
    shortPath,
    summaryIsLong,
    approvalExpiresAt,
  };
}

/* ======================================================================== */
/* 2. Panel UI                                                              */
/* ======================================================================== */

if (typeof window !== 'undefined' && typeof document !== 'undefined') {
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', initPanel);
  else initPanel();
}

function initPanel() {
  /* ---------- Icons (constant markup only) ---------- */

  const ICONS = {
    plus: '<path d="M12 5v14M5 12h14"/>',
    layers: '<path d="M12 3l9 5-9 5-9-5 9-5z"/><path d="M3 13l9 5 9-5"/>',
    search: '<circle cx="11" cy="11" r="6.5"/><path d="M20 20l-4.2-4.2"/>',
    refresh: '<path d="M20 11a8 8 0 1 0-2.34 5.66"/><path d="M20 4v7h-7"/>',
    power: '<path d="M12 3v8"/><path d="M6.3 6.8a8 8 0 1 0 11.4 0"/>',
    more: '<circle cx="5.5" cy="12" r="1.2" fill="currentColor"/><circle cx="12" cy="12" r="1.2" fill="currentColor"/><circle cx="18.5" cy="12" r="1.2" fill="currentColor"/>',
    grip: '<circle cx="9" cy="6" r="1.1" fill="currentColor"/><circle cx="15" cy="6" r="1.1" fill="currentColor"/><circle cx="9" cy="12" r="1.1" fill="currentColor"/><circle cx="15" cy="12" r="1.1" fill="currentColor"/><circle cx="9" cy="18" r="1.1" fill="currentColor"/><circle cx="15" cy="18" r="1.1" fill="currentColor"/>',
    folder: '<path d="M3 7.5A2 2 0 0 1 5 5.5h4l2 2h8a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
    trash: '<path d="M4 7h16"/><path d="M10 11v6M14 11v6"/><path d="M6 7l1 12a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-12"/><path d="M9 7V4h6v3"/>',
    open: '<path d="M14 4h6v6"/><path d="M20 4l-9 9"/><path d="M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5"/>',
    close: '<path d="M6 6l12 12M18 6L6 18"/>',
    edit: '<path d="M4 20h4L19 9l-4-4L4 16z"/><path d="M13.5 6.5l4 4"/>',
    palette: '<path d="M12 3a9 9 0 1 0 0 18c1 0 1.6-.8 1.6-1.6 0-1.1-.9-1.5-.9-2.5 0-.9.7-1.6 1.6-1.6H16a5 5 0 0 0 5-5C21 6.5 17 3 12 3z"/><circle cx="7.5" cy="11" r="1" fill="currentColor"/><circle cx="10" cy="7" r="1" fill="currentColor"/><circle cx="14.5" cy="7" r="1" fill="currentColor"/>',
    remove: '<path d="M15 19v-1a4 4 0 0 0-4-4H7a4 4 0 0 0-4 4v1"/><circle cx="9" cy="7.5" r="3.5"/><path d="M16 11h5"/>',
    move: '<path d="M5 12h14"/><path d="M13 6l6 6-6 6"/>',
    copy: '<rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V6a2 2 0 0 1 2-2h8"/>',
    check: '<path d="M5 12.5l4.5 4.5L19 7.5"/>',
    alert: '<path d="M12 9v4"/><path d="M12 16.5v.01"/><path d="M10.3 4L2.6 17.5A2 2 0 0 0 4.3 20.5h15.4a2 2 0 0 0 1.7-3L13.7 4a2 2 0 0 0-3.4 0z"/>',
    info: '<circle cx="12" cy="12" r="9"/><path d="M12 11v5"/><path d="M12 8v.01"/>',
    sun: '<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>',
    moon: '<path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z"/>',
    monitor: '<rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8M12 16v4"/>',
    inbox: '<path d="M3 13h5l2 3h4l2-3h5"/><path d="M5.5 5h13l2.5 8v6a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1v-6z"/>',
    user: '<circle cx="12" cy="8" r="4"/><path d="M4 21a8 8 0 0 1 16 0"/>',
    caret: '<path d="M7 10l5 5 5-5"/>',
    bubble: '<circle cx="12" cy="12" r="7.5"/><circle cx="12" cy="12" r="2.2" fill="currentColor"/>',
    tray: '<rect x="2.5" y="7" width="19" height="10" rx="5"/><circle cx="8" cy="12" r="1.6" fill="currentColor"/><circle cx="12.5" cy="12" r="1.6" fill="currentColor"/><circle cx="17" cy="12" r="1.2" fill="currentColor"/>',
  };

  function icon(name, extraClass) {
    const span = document.createElement('span');
    span.className = 'ico' + (extraClass ? ' ' + extraClass : '');
    span.setAttribute('aria-hidden', 'true');
    span.innerHTML = '<svg viewBox="0 0 24 24">' + (ICONS[name] || '') + '</svg>';
    return span;
  }

  /** Fill every static <span data-icon="…"> in the HTML. */
  document.querySelectorAll('[data-icon]').forEach((el) => {
    el.setAttribute('aria-hidden', 'true');
    el.innerHTML = '<svg viewBox="0 0 24 24">' + (ICONS[el.dataset.icon] || '') + '</svg>';
  });

  /**
   * Tiny element builder. props: class, text, attrs{}, dataset{}, style{},
   * on{event: fn}, plus any direct DOM property (type, disabled, title, …).
   */
  function h(tag, props, ...children) {
    const el = document.createElement(tag);
    if (props) {
      for (const [k, v] of Object.entries(props)) {
        if (v === undefined || v === null || v === false) continue;
        if (k === 'class') el.className = v;
        else if (k === 'text') el.textContent = v;
        else if (k === 'attrs') for (const [ak, av] of Object.entries(v)) { if (av !== undefined && av !== null && av !== false) el.setAttribute(ak, String(av)); }
        else if (k === 'dataset') Object.assign(el.dataset, v);
        else if (k === 'style') for (const [sk, sv] of Object.entries(v)) el.style.setProperty(sk, sv);
        else if (k === 'on') for (const [ev, fn] of Object.entries(v)) el.addEventListener(ev, fn);
        else el[k] = v;
      }
    }
    for (const c of children.flat()) {
      if (c === null || c === undefined || c === false) continue;
      el.appendChild(typeof c === 'string' || typeof c === 'number' ? document.createTextNode(String(c)) : c);
    }
    return el;
  }

  const $ = (id) => document.getElementById(id);

  /* ---------- Bridge ---------- */

  const api = window.sessionmux || {};

  /** Call a window.sessionmux method; never throws. Missing methods resolve to undefined. */
  async function call(method, ...args) {
    if (typeof api[method] !== 'function') return undefined;
    try {
      return await api[method](...args);
    } catch (err) {
      return { ok: false, error: (err && err.message) || String(err) };
    }
  }

  // Menus and the confirm dialog are DOM overlays inside the panel: focus never
  // leaves the window while they are open, so the panel is not pinned for them
  // (pinning only made it ignore a real click-away and stay on top).

  /* ---------- State ---------- */

  const state = {
    tab: 'accounts',
    accounts: [],
    accountsLoaded: false,
    accountsJson: '',          // last rendered snapshot, to skip no-op re-renders
    accountsDirty: false,      // a push arrived while an edit/drag blocked re-rendering
    activeId: null,
    settings: null,
    sessionsAccountId: null,
    sessions: new Map(),       // accountId → { list, loading, error, loadedAt }
    search: '',
    editingId: null,           // account whose inline edit form is open
    adding: false,
    adoptingDir: null,         // dataDir of the found folder the add form is adopting
    discovered: [],            // Claude-* profile folders found on disk, not yet accounts
    busyDiscovered: new Set(),
    dragId: null,
    busyAccounts: new Set(),
    busySessions: new Set(),
    formValidators: new Set(), // open add/edit forms re-check their name when accounts change
    appInfo: null,             // getAppInfo()
    signIn: { active: false },  // last getSignIn() / onSignInChanged payload
    approvals: [],             // listApprovals(): requests waiting for a decision, oldest first
    approvalsLoaded: false,
    waiting: [],               // listWaiting(): sessions waiting for the user
    waitingJson: '',
    sessionsDirty: false,      // waiting state changed while a menu or dialog was open
    hooks: null,               // getHooksStatus()
    hooksBusy: false,          // installHooks / removeHooks in flight
  };

  const accountById = (id) => state.accounts.find((a) => a.id === id) || null;

  /** True when the app reports it runs on macOS (getAppInfo().platform); Windows otherwise. */
  const onMac = () => !!(state.appInfo && state.appInfo.platform === 'macos');

  /** macOS: running from a disk image or a quarantine copy, where our path cannot be written anywhere. */
  const movedAway = () => onMac() && state.appInfo.translocated === true;

  const MOVE_HINT = 'Move Sessionmux to your Applications folder and open it from there.';

  const FOCUS_FAILED_MAC = "Sessionmux could not bring this Claude window forward. Turn on 'Bring minimised Claude windows to the front' in Settings (needs Accessibility), or click this account's Claude icon in the Dock.";

  const EMAIL_CODE_ADVICE = 'Sign in with your e-mail address and the code Claude sends you; this needs no extra steps.';

  /* ---------- Theme ---------- */

  function applyTheme(theme) {
    const root = document.documentElement;
    if (theme === 'light' || theme === 'dark') root.setAttribute('data-theme', theme);
    else root.removeAttribute('data-theme');
  }

  /* ---------- Focus preservation across re-renders ---------- */

  /** Elements that should keep focus across re-renders carry data-key. */
  function withFocusKept(fn) {
    const active = document.activeElement;
    const key = active && active.dataset ? active.dataset.key : null;
    const isText = active && (active.tagName === 'TEXTAREA' || active.tagName === 'INPUT');
    const sel = isText ? [active.selectionStart, active.selectionEnd] : null;
    fn();
    if (key) {
      const again = document.querySelector('[data-key="' + CSS.escape(key) + '"]');
      if (again && again !== document.activeElement) {
        again.focus({ preventScroll: true });
        if (sel && typeof again.setSelectionRange === 'function') {
          try { again.setSelectionRange(sel[0], sel[1]); } catch (_) { /* not a text control */ }
        }
      }
    }
  }

  /* ======================================================================
   * Tabs
   * ==================================================================== */

  const TABS = ['accounts', 'sessions', 'settings'];
  const tabButtons = Array.from(document.querySelectorAll('.tab'));
  // The active tab is a white pill (`.tab.is-active`); #tabIndicator stays in the
  // markup but is hidden by CSS, so nothing measures or positions it any more.

  function showTab(name, opts) {
    if (!TABS.includes(name)) return;
    const focusTab = opts && opts.focus;
    closeMenu(false);
    const entering = state.tab !== name;
    state.tab = name;
    for (const b of tabButtons) {
      const on = b.dataset.tab === name;
      b.setAttribute('aria-selected', on ? 'true' : 'false');
      b.classList.toggle('is-active', on);
      b.tabIndex = on ? 0 : -1;
      if (on && focusTab) b.focus();
    }
    for (const t of TABS) $('view-' + t).hidden = t !== name;
    if (name === 'sessions') {
      enterSessions();
      // Cards hidden until now arm again, so Allow is never live the moment they appear.
      if (entering) armAllCards();
    }
    if (name === 'settings' && entering) refreshHooks();
    if (name === 'settings') {
      renderSettings();
      // Arriving on Settings starts at the top.
      if (entering) $('settingsScroll').scrollTop = 0;
    }
    if (name === 'accounts') renderAccounts();
  }

  tabButtons.forEach((b, i) => {
    b.addEventListener('click', () => showTab(b.dataset.tab));
    b.addEventListener('keydown', (e) => {
      let next = -1;
      if (e.key === 'ArrowRight') next = (i + 1) % tabButtons.length;
      else if (e.key === 'ArrowLeft') next = (i - 1 + tabButtons.length) % tabButtons.length;
      else if (e.key === 'Home') next = 0;
      else if (e.key === 'End') next = tabButtons.length - 1;
      if (next >= 0) {
        e.preventDefault();
        showTab(tabButtons[next].dataset.tab, { focus: true });
      }
    });
  });

  // App → panel tab switching (used by --smoke and the tray "Settings…" item). "about" opens
  // Settings scrolled to the About card.
  function showTabFromApp(tab) {
    const name = String(tab || '');
    if (name !== 'about') { showTab(name); return; }
    showTab('settings');
    const about = $('aboutTitle');
    if (about) about.scrollIntoView({ block: 'start' });
  }
  if (typeof api.onShowTab === 'function') {
    try { api.onShowTab(showTabFromApp); } catch (_) { /* optional */ }
  }

  /* ======================================================================
   * Popover menu (row "…" menu, Move to ▾ / Copy to ▾)
   * ==================================================================== */

  const card = $('card');
  const pop = $('popover');
  let menu = null; // { anchor, anchorKey, onClose }

  /**
   * items: [{ label, hint?, icon?, avatar?: account, danger?, disabled?, onSelect }]
   *        | { separator: true } | { heading: 'text' }
   */
  function openMenu(anchor, items, opts) {
    closeMenu(false);
    pop.textContent = '';
    pop.setAttribute('aria-label', (opts && opts.label) || 'Menu');
    const buttons = [];
    for (const it of items) {
      if (it.separator) { pop.appendChild(h('div', { class: 'menu__sep', attrs: { role: 'separator' } })); continue; }
      if (it.heading) { pop.appendChild(h('div', { class: 'menu__label', attrs: { role: 'presentation' }, text: it.heading })); continue; }
      const lead = it.avatar ? avatarEl(it.avatar, 'avatar--sm') : it.icon ? icon(it.icon) : null;
      const btn = h('button', {
        class: 'menu__item' + (it.danger ? ' menu__item--danger' : ''),
        type: 'button',
        tabIndex: -1,
        attrs: { role: 'menuitem', 'aria-disabled': it.disabled ? 'true' : null },
        on: {
          click: (e) => {
            e.stopPropagation();
            if (it.disabled) return;
            closeMenu(true);
            it.onSelect && it.onSelect();
          },
        },
      },
      lead,
      h('span', { class: 'menu__text' },
        h('span', { class: 'menu__main', text: it.label }),
        it.hint ? h('span', { class: 'menu__hint', text: it.hint }) : null));
      pop.appendChild(btn);
      buttons.push(btn);
    }

    pop.hidden = false;
    pop.classList.remove('is-above');
    // Position relative to the card; prefer below + right-aligned, flip above if needed.
    const cr = card.getBoundingClientRect();
    const ar = anchor.getBoundingClientRect();
    const pw = pop.offsetWidth;
    const ph = pop.offsetHeight;
    let top = ar.bottom - cr.top + 4;
    if (top + ph > cr.height - 8) {
      top = Math.max(8, ar.top - cr.top - ph - 4);
      pop.classList.add('is-above');
    }
    let left = ar.right - cr.left - pw;
    left = Math.min(Math.max(8, left), cr.width - pw - 8);
    pop.style.top = top + 'px';
    pop.style.left = left + 'px';

    anchor.setAttribute('aria-expanded', 'true');
    menu = { anchor, anchorKey: anchor.dataset.key || null, onClose: opts && opts.onClose };
    const first = buttons.find((b) => b.getAttribute('aria-disabled') !== 'true') || buttons[0];
    if (first) first.focus();
  }

  function closeMenu(restoreFocus) {
    if (!menu) return;
    const m = menu;
    menu = null;
    pop.hidden = true;
    pop.textContent = '';
    m.anchor.setAttribute('aria-expanded', 'false');
    if (m.onClose) m.onClose();
    if (restoreFocus) {
      // The anchor may have been re-rendered while the menu was open.
      const target = m.anchor.isConnected ? m.anchor
        : m.anchorKey ? document.querySelector('[data-key="' + CSS.escape(m.anchorKey) + '"]') : null;
      if (target) target.focus({ preventScroll: true });
    }
    if (state.accountsDirty) renderAccounts();
    if (state.sessionsDirty) refreshSessionRows();
  }

  /** Toggle helper for anchor buttons: clicking the open anchor closes it. */
  function toggleMenu(anchor, buildItems, opts) {
    if (menu && (menu.anchor === anchor || (menu.anchorKey && menu.anchorKey === anchor.dataset.key))) {
      closeMenu(true);
      return;
    }
    openMenu(anchor, buildItems(), opts);
  }

  pop.addEventListener('keydown', (e) => {
    const items = Array.from(pop.querySelectorAll('.menu__item'));
    const i = items.indexOf(document.activeElement);
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const d = e.key === 'ArrowDown' ? 1 : -1;
      const n = items[(i + d + items.length) % items.length];
      if (n) n.focus();
    } else if (e.key === 'Home' || e.key === 'End') {
      e.preventDefault();
      const n = e.key === 'Home' ? items[0] : items[items.length - 1];
      if (n) n.focus();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      closeMenu(true);
    } else if (e.key === 'Tab') {
      e.preventDefault();
      closeMenu(true);
    }
  });

  // Click outside closes the menu (anchor clicks are handled by toggleMenu).
  document.addEventListener('mousedown', (e) => {
    if (!menu) return;
    if (pop.contains(e.target)) return;
    if (menu.anchor.contains(e.target)) return;
    closeMenu(false);
  }, true);

  /* ======================================================================
   * Confirm dialog
   * ==================================================================== */

  const scrim = $('dialogScrim');
  const dlgTitle = $('dialogTitle');
  const dlgBody = $('dialogBody');
  const dlgCode = $('dialogCode');
  const dlgCheckWrap = $('dialogCheckWrap');
  const dlgCheck = $('dialogCheck');
  const dlgCheckLabel = $('dialogCheckLabel');
  const dlgOk = $('dialogOk');
  const dlgCancel = $('dialogCancel');
  let dialog = null; // { resolve, prevFocus }

  /**
   * Custom in-panel confirm. Resolves { confirmed: boolean, checked: boolean }.
   * opts: { title, body, code?, okLabel?, danger?, focusCancel?, check?: { label, checked? } }
   *  is shown as a scrollable monospace block under the body.
   */
  function confirmDialog(opts) {
    if (dialog) finishDialog(false);
    closeMenu(false);
    return new Promise((resolve) => {
      dlgTitle.textContent = opts.title || 'Are you sure?';
      dlgBody.textContent = opts.body || '';
      dlgBody.hidden = !opts.body;
      dlgCode.textContent = opts.code || '';
      dlgCode.hidden = !opts.code;
      dlgCode.scrollTop = 0;
      if (opts.check) {
        dlgCheckWrap.hidden = false;
        dlgCheckLabel.textContent = opts.check.label;
        dlgCheck.checked = !!opts.check.checked;
      } else {
        dlgCheckWrap.hidden = true;
        dlgCheck.checked = false;
      }
      dlgOk.textContent = opts.okLabel || 'OK';
      dlgOk.className = 'btn ' + (opts.danger ? 'btn--danger' : 'btn--primary btn--invert');
      dialog = { resolve, prevFocus: document.activeElement };
      scrim.hidden = false;
      // Destructive actions (and changes to files outside Sessionmux) default to Cancel so Enter
      // can't confirm them by accident.
      (opts.danger || opts.focusCancel ? dlgCancel : dlgOk).focus();
    });
  }

  function finishDialog(confirmed) {
    if (!dialog) return;
    const d = dialog;
    dialog = null;
    scrim.hidden = true;
    d.resolve({ confirmed, checked: dlgCheck.checked });
    if (d.prevFocus && d.prevFocus.isConnected) d.prevFocus.focus({ preventScroll: true });
    if (state.sessionsDirty) refreshSessionRows();
  }

  dlgOk.addEventListener('click', () => finishDialog(true));
  dlgCancel.addEventListener('click', () => finishDialog(false));
  scrim.addEventListener('mousedown', (e) => { if (e.target === scrim) finishDialog(false); });
  scrim.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      finishDialog(false);
    } else if (e.key === 'Tab') {
      // Focus trap inside the dialog.
      const f = Array.from(scrim.querySelectorAll('button, input, pre[tabindex]')).filter((el) => !el.closest('[hidden]'));
      if (!f.length) return;
      const i = f.indexOf(document.activeElement);
      const n = e.shiftKey ? (i <= 0 ? f.length - 1 : i - 1) : (i === f.length - 1 ? 0 : i + 1);
      e.preventDefault();
      f[n].focus();
    }
  });

  /* ======================================================================
   * Toasts
   * ==================================================================== */

  const toasts = $('toasts');
  const TOAST_ICON = { success: 'check', warn: 'alert', error: 'alert', info: 'info' };

  /** action (optional): { label, onClick } — a small primary button; clicking it dismisses the toast. */
  function toast(kind, title, body, timeoutMs, action) {
    const el = h('div', { class: 'toast toast--' + kind, attrs: { role: kind === 'error' ? 'alert' : null } },
      icon(TOAST_ICON[kind] || 'info', 'toast__icon'),
      h('div', { class: 'toast__text' },
        h('span', { class: 'toast__title', text: title }),
        body ? h('span', { class: 'toast__body', text: body }) : null),
      action ? h('button', {
        class: 'btn btn--sm btn--primary toast__action',
        type: 'button',
        text: action.label,
        on: { click: () => { dismiss(); action.onClick(); } },
      }) : null,
      h('button', {
        class: 'icon-btn icon-btn--sm toast__close',
        type: 'button',
        attrs: { 'aria-label': 'Dismiss' },
        on: { click: () => dismiss() },
      }, icon('close')));
    let timer = null;
    const dismiss = () => {
      if (!el.isConnected || el.classList.contains('is-leaving')) return;
      clearTimeout(timer);
      el.classList.add('is-leaving');
      setTimeout(() => el.remove(), 200);
    };
    const arm = () => {
      clearTimeout(timer);
      if (el.matches(':hover') || el.contains(document.activeElement)) return;
      timer = setTimeout(dismiss, timeoutMs || (kind === 'error' ? 7000 : 4500));
    };
    // Paused while the pointer or keyboard focus is on the toast (its action must not vanish).
    el.addEventListener('mouseenter', () => clearTimeout(timer));
    el.addEventListener('mouseleave', arm);
    el.addEventListener('focusin', () => clearTimeout(timer));
    el.addEventListener('focusout', (e) => { if (!el.contains(e.relatedTarget)) arm(); });
    toasts.appendChild(el);
    while (toasts.children.length > 3) toasts.firstChild.remove();
    arm();
  }

  /** A product URL from getAppInfo(), or `fallback` when it is missing or not http(s). */
  function urlOf(key, fallback) {
    const v = state.appInfo && state.appInfo[key];
    return typeof v === 'string' && /^https?:\/\//i.test(v) ? v : fallback;
  }

  /* ======================================================================
   * Shared bits
   * ==================================================================== */

  function avatarEl(a, extraClass) {
    return h('span', {
      class: 'avatar' + (extraClass ? ' ' + extraClass : ''),
      style: { '--avatar-color': a.color || 'var(--accent)' },
      attrs: { 'aria-hidden': 'true' },
      text: a.initials || initialsOf(a.name),
    });
  }

  function emptyState(iconName, title, body, action) {
    return h('div', { class: 'empty' },
      h('span', { class: 'empty__icon' }, icon(iconName)),
      h('p', { class: 'empty__title', text: title }),
      body ? h('p', { class: 'empty__body', text: body }) : null,
      action ? h('button', { class: 'btn btn--secondary btn--sm', type: 'button', text: action.label, on: { click: action.onClick } }) : null);
  }

  /* ======================================================================
   * Accounts tab
   * ==================================================================== */

  const accountList = $('accountList');
  const addHost = $('addFormHost');
  const btnAdd = $('btnAdd');
  const btnOpenAll = $('btnOpenAll');

  /* ---------- Sign-in routing notice ---------- */

  const appNotice = $('appNotice');
  const appNoticeText = $('appNoticeText');

  /** The top notice for an app that runs from a disk image or a quarantine copy (macOS). */
  function renderAppNotice() {
    const moved = movedAway();
    appNoticeText.textContent = moved ? MOVE_HINT : '';
    appNotice.hidden = !moved;
  }

  const signinNotice = $('signinNotice');
  const signinNoticeText = $('signinNoticeText');
  const signinNoticeBtn = $('signinNoticeBtn');
  const signinNoticeAlt = $('signinNoticeAlt');
  // 'cancel' | 'enable', and while signing in on macOS 'pending' | 'declined' | 'automation' | 'fallback'
  let signinNoticeMode = null;
  let signinNoticeKey = null;  // last rendered mode + name: the role=status region is only rewritten on change

  const routingOn = () => !state.settings || state.settings.signInRouting !== false;

  function renderSignInNotice() {
    const si = state.signIn;
    let mode = null;
    if (si && si.active) {
      // The system's answer about the link handler or the hand-over decides what the user has to do.
      if (si.automationDenied) mode = 'automation';
      else if (si.forwardFailed) mode = 'fallback';
      else if (si.handlerDeclined) mode = 'declined';
      else if (si.handlerPending) mode = 'pending';
      else mode = 'cancel';
    } else if (!routingOn() && state.accounts.some((a) => !a.isMain && a.running && a.signedIn === false)) mode = 'enable';
    signinNoticeMode = mode;
    const key = mode ? mode + '|' + (mode !== 'enable' ? si.accountName || '' : '') : null;
    if (key === signinNoticeKey) return;
    signinNoticeKey = key;
    if (!mode) {
      // Keep keyboard focus in the Accounts tab when the focused Cancel / Turn on button goes away.
      const hadFocus = signinNotice.contains(document.activeElement);
      signinNotice.hidden = true;
      if (hadFocus) {
        const first = accountList.querySelector('.acct__main');
        (first || $('view-accounts')).focus();
      }
      return;
    }
    signinNoticeAlt.hidden = mode !== 'automation';
    if (mode === 'cancel') {
      signinNoticeText.textContent = 'Signing in ' + (si.accountName || 'the new account') +
        ' — use the Claude window that opened. Google, Apple and e-mail sign-in all work; Sessionmux sends the link back to this account.';
      signinNoticeBtn.textContent = 'Cancel';
      signinNoticeBtn.setAttribute('aria-label', 'Cancel sign-in routing');
    } else if (mode !== 'enable') {
      const name = si.accountName || 'the new account';
      if (mode === 'pending') {
        signinNoticeText.textContent = 'macOS is asking whether Sessionmux may open Claude sign-in links. Choose Use.';
      } else if (mode === 'declined') {
        signinNoticeText.textContent = 'Sign-in links stay with Claude. Sign in with an e-mail code instead, or quit your other Claude accounts while this one signs in.';
      } else if (mode === 'automation') {
        signinNoticeText.textContent = 'Sessionmux needs your permission to pass the sign-in link to ' + name +
          '. Allow it in System Settings > Privacy & Security > Automation, or sign in with an e-mail code.';
        signinNoticeAlt.textContent = 'Open Automation';
        signinNoticeAlt.setAttribute('aria-label', 'Open the Automation settings');
      } else {
        signinNoticeText.textContent = 'Quit your other Claude accounts while this one signs in, or sign in with an e-mail code.';
      }
      signinNoticeBtn.textContent = 'Cancel';
      signinNoticeBtn.setAttribute('aria-label', 'Cancel sign-in routing');
    } else {
      signinNoticeText.textContent = '';
      signinNoticeText.append(
        'Google and Apple sign-in open the browser and the link back lands in your main account. Use ',
        h('strong', { text: 'Continue with email' }),
        ' and type the code, or turn on sign-in routing in Settings.');
      signinNoticeBtn.textContent = 'Turn on';
      signinNoticeBtn.setAttribute('aria-label', 'Turn on sign-in routing');
    }
    signinNotice.hidden = false;
  }

  signinNoticeBtn.addEventListener('click', async () => {
    if (signinNoticeMode === 'enable') {
      updateSetting('signInRouting', true);
      return;
    }
    const res = await call('cancelSignIn');
    if (res && res.ok === false) toast('error', "Couldn't cancel sign-in routing", res.error || '');
  });

  signinNoticeAlt.addEventListener('click', () => call('openSystemSettings', 'automation'));

  /** Accept a getSignIn() reply or a signin-changed event; a just-ended relay gets a toast. */
  function setSignIn(si) {
    if (!si || typeof si !== 'object' || typeof si.active !== 'boolean') return;
    const prev = state.signIn || {};
    state.signIn = si;
    renderSignInNotice();
    // Settings shows the give-back button and the consent wording from these two flags.
    if (state.tab === 'settings' && (!!prev.restoreDeclined !== !!si.restoreDeclined || !!prev.needsConsent !== !!si.needsConsent)) {
      renderSettings();
    }
    const name = typeof si.accountName === 'string' && si.accountName ? si.accountName : 'The account';
    switch (si.ended) {
      case 'signedIn': toast('success', name + ' is signed in'); break;
      case 'timeout': toast('warn', 'Sign-in for ' + name + ' timed out', 'Open the account again to retry.', 9000); break;
      case 'cancelled': toast('info', 'Sign-in routing cancelled', 'Sign-in links open your main account again.'); break;
      case 'closed': toast('info', 'Sign-in routing ended', name + "'s Claude window was closed."); break;
      default: break;
    }
  }

  function setAccounts(list) {
    if (!Array.isArray(list)) return;
    state.accounts = list;
    state.accountsLoaded = true;
    state.formValidators.forEach((fn) => { try { fn(); } catch (_) { /* form gone */ } });
    if (!state.activeId) {
      const running = list.find((a) => a.running);
      state.activeId = running ? running.id : null;
    }
    renderAccounts();
    renderSignInNotice();
    if (state.tab === 'sessions') renderChips();
    if (state.tab === 'settings') renderSettings();
    scheduleDiscoveredRefresh();
  }

  // Account changes can add/remove found folders; refresh once per burst of pushes.
  let discTimer = null;
  function scheduleDiscoveredRefresh() {
    if (typeof api.listDiscovered !== 'function') return;
    clearTimeout(discTimer);
    discTimer = setTimeout(() => { refreshDiscovered(); }, 250);
  }

  function renderAccounts() {
    if (!state.accountsLoaded) return;
    // Never yank an open edit form, an in-flight drag or a menu's anchor row.
    if (state.editingId || state.dragId || menu) {
      state.accountsDirty = true;
      return;
    }
    const snapshot = JSON.stringify([state.accounts, state.activeId, [...state.busyAccounts]]);
    if (snapshot === state.accountsJson && accountList.childElementCount && !state.accountsDirty) return;
    state.accountsJson = snapshot;
    state.accountsDirty = false;

    withFocusKept(() => {
      accountList.textContent = '';
      accountList.setAttribute('aria-busy', 'false');
      if (state.accounts.length === 0) {
        accountList.appendChild(h('li', null, emptyState('user', 'No accounts yet', 'Add an account to get started.')));
        return;
      }
      for (const a of state.accounts) accountList.appendChild(accountRow(a));
    });
  }

  function sameSignInHint(name) {
    return 'This profile is signed in to the same claude.ai account as ' + name +
      '. Open it, sign out there and sign in with another account — or remove it.';
  }

  function statusText(a) {
    if (state.busyAccounts.has(a.id)) return 'Opening…';
    return a.running ? 'Open' : 'Not running';
  }

  function accountRow(a) {
    const busy = state.busyAccounts.has(a.id);
    const li = h('li', {
      class: 'acct' + (a.id === state.activeId ? ' acct--active' : '') + (busy ? ' is-busy' : ''),
      dataset: { id: a.id },
    });

    const handle = h('span', {
      class: 'acct__handle',
      draggable: true,
      title: onMac() ? 'Drag to reorder (or ⌥↑ / ⌥↓)' : 'Drag to reorder (or Alt+↑ / Alt+↓)',
      attrs: { 'aria-hidden': 'true' },
      on: {
        dragstart: (e) => {
          state.dragId = a.id;
          li.classList.add('is-dragging');
          e.dataTransfer.effectAllowed = 'move';
          e.dataTransfer.setData('text/plain', a.id);
          try { e.dataTransfer.setDragImage(li, 24, 28); } catch (_) { /* cosmetic */ }
        },
        dragend: () => endDrag(),
      },
    }, icon('grip'));

    const sessionCount = Number(a.sessionCount) || 0;
    const sameAs = typeof a.sameSignInAs === 'string' && a.sameSignInAs ? a.sameSignInAs : null;
    const signedOut = a.signedIn === false;
    const needsSignIn = signedOut && a.running;
    const status = h('span', { class: 'acct__status' + (a.running ? ' is-running' : '') + (needsSignIn ? ' needs-signin' : '') },
      h('span', { class: 'status-dot' }),
      h('span', { text: statusText(a) }));
    const signInBadge = signedOut
      ? h('span', { class: 'badge badge--warn acct__signin', text: needsSignIn ? 'Sign in needed' : 'Not signed in' })
      : null;
    const limited = limitText(a.limited);
    const limitBadge = limited
      ? h('span', {
        class: 'badge badge--warn acct__limited',
        text: limited,
        title: a.name + ' hit its plan limit. Hand its sessions over from the Sessions tab.',
      })
      : null;
    const countBadge = h('span', {
      class: 'badge--count acct__count',
      text: String(sessionCount),
      title: plural(sessionCount, 'session'),
      attrs: { 'aria-hidden': 'true' },
    });

    const main = h('button', {
      class: 'acct__main',
      type: 'button',
      dataset: { key: 'acct-main-' + a.id },
      title: a.running ? 'Switch to ' + a.name : 'Open ' + a.name,
      attrs: {
        'aria-label': a.name + (a.label ? ' (' + a.label + ')' : '') + ', ' + statusText(a) +
          ', ' + plural(sessionCount, 'session') +
          (a.id === state.activeId ? ', active' : '') +
          (signedOut ? (needsSignIn ? ', sign in needed' : ', not signed in') : '') +
          (limited ? ', ' + limited : '') +
          (sameAs ? ', same sign-in as ' + sameAs : '') + '. Press Enter to ' + (a.running ? 'switch to it' : 'open it') +
          (onMac() ? '. Option+Up or Option+Down to reorder.' : '. Alt+Up or Alt+Down to reorder.'),
      },
      on: {
        click: () => openAccount(a.id),
        keydown: (e) => {
          if (e.altKey && (e.key === 'ArrowUp' || e.key === 'ArrowDown')) {
            e.preventDefault();
            nudgeAccount(a.id, e.key === 'ArrowUp' ? -1 : 1);
          } else if (e.key === 'ContextMenu' || (e.shiftKey && e.key === 'F10')) {
            e.preventDefault();
            openAccountMenu(li.querySelector('.acct__more'), a);
          }
        },
      },
    },
    h('span', { class: 'acct__avatar-wrap' },
      avatarEl(a),
      h('span', { class: 'acct__dot' + (a.running ? ' is-running' : '') + (needsSignIn ? ' needs-signin' : '') })),
    h('span', { class: 'acct__text' },
      h('span', { class: 'acct__name-line' },
        h('span', { class: 'acct__name', text: a.name }),
        a.isMain ? h('span', { class: 'badge', text: 'Main' }) : null),
      a.label ? h('span', { class: 'acct__label', text: a.label }) : null,
      sameAs || signInBadge || limitBadge
        ? h('span', { class: 'acct__status-line' },
          status,
          signInBadge,
          limitBadge,
          sameAs ? h('span', {
            class: 'badge badge--warn acct__same',
            text: 'Same sign-in as ' + sameAs,
            title: sameSignInHint(sameAs),
          }) : null)
        : status),
    countBadge);

    const more = h('button', {
      class: 'icon-btn acct__more',
      type: 'button',
      dataset: { key: 'acct-more-' + a.id },
      attrs: { 'aria-label': 'More actions for ' + a.name, 'aria-haspopup': 'menu', 'aria-expanded': 'false' },
      on: { click: (e) => { e.stopPropagation(); openAccountMenu(more, a); } },
    }, icon('more'));

    li.append(handle, main, more);
    li.addEventListener('contextmenu', (e) => { e.preventDefault(); openAccountMenu(more, a); });
    return li;
  }

  function openAccountMenu(anchor, a) {
    if (!anchor) return;
    toggleMenu(anchor, () => [
      { label: a.running ? 'Switch to window' : 'Open', icon: 'open', onSelect: () => openAccount(a.id) },
      {
        label: 'Close window',
        icon: 'close',
        disabled: !a.running,
        hint: a.running ? null : 'Not running',
        onSelect: () => closeAccount(a),
      },
      { separator: true },
      { label: 'Rename…', icon: 'edit', onSelect: () => startEdit(a.id, 'name') },
      { label: 'Change colour', icon: 'palette', onSelect: () => startEdit(a.id, 'color') },
      { separator: true },
      {
        label: 'Remove…',
        icon: 'remove',
        danger: true,
        disabled: !!a.isMain,
        hint: a.isMain ? "The Main account can't be removed" : null,
        onSelect: () => removeAccount(a),
      },
    ], { label: 'Actions for ' + a.name });
  }

  async function openAccount(id) {
    const a = accountById(id);
    if (!a || state.busyAccounts.has(id)) return;
    state.busyAccounts.add(id);
    state.activeId = id;
    renderAccounts();
    const res = await call('openAccount', id);
    state.busyAccounts.delete(id);
    state.accountsDirty = true;
    renderAccounts();
    // The account's window was in front, so the click minimised it: nothing to report.
    if (res && res.minimised) return;
    if (res && res.ok === false) openFailedToast(a.name, res);
  }

  /** The toast for a failed open; `res` is the reply ({error, reason}) or an openFailed event. */
  function openFailedToast(name, res) {
    const reason = res && res.reason;
    const title = "Couldn't switch to " + name;
    const settings = { label: 'Settings', onClick: () => showTab('settings') };
    if (reason === 'focus-failed') {
      toast('warn', title, onMac() ? FOCUS_FAILED_MAC : (res.error || ''), 12000, settings);
    } else if (reason === 'focus-no-window') {
      toast('warn', title, name + "'s Claude is open but its window is closed and could not be reopened. Click its Claude icon in the Dock.", 12000);
    } else if (reason === 'focus-automation-denied') {
      toast('warn', title, name + "'s Claude window is closed, and Sessionmux is not allowed to ask Claude to reopen it. Allow Sessionmux to control Claude in Privacy & Security > Automation, or click its Claude icon in the Dock.", 15000,
        { label: 'Open Automation settings', onClick: () => call('openSystemSettings', 'automation') });
    } else if (reason === 'focus-accessibility-untrusted') {
      toast('warn', title, "macOS does not let Sessionmux raise Claude's windows. Turn Sessionmux on in Privacy & Security > Accessibility; after moving or updating Sessionmux, remove it there and add it again.", 15000,
        { label: 'Open Accessibility settings', onClick: () => call('openSystemSettings', 'accessibility') });
    } else if (reason === 'focus-accessibility-off') {
      toast('warn', title, name + "'s Claude window is minimised or hidden. Turn on 'Bring minimised Claude windows to the front' in Settings, or click its Claude icon in the Dock.", 12000, settings);
    } else if (reason === 'focus-refused') {
      toast('warn', title, 'macOS did not let Sessionmux bring ' + name + "'s Claude forward. Click its Claude icon in the Dock.", 12000);
    } else if (reason === 'focus-not-responding') {
      toast('warn', title, name + "'s Claude is not responding. Wait a moment and try again, or quit and reopen it.", 12000);
    } else if (reason === 'focus-gone') {
      toast('warn', title, name + "'s Claude quit before its window came forward. Click the account again to start it.", 12000);
    } else {
      toast('error', "Couldn't open " + name, (res && res.error) || 'Claude did not start.');
    }
  }

  async function closeAccount(a) {
    const res = await call('closeAccount', a.id);
    if (res && res.ok === false) toast('error', "Couldn't close " + a.name, res.error || '');
    else if (res && res.notRunning) toast('info', a.name + ' is not running');
    else toast('info', 'Closed ' + a.name + "'s Claude window");
    refreshAccounts();
  }

  async function removeAccount(a) {
    const deletable = !!a.dataDir;
    const r = await confirmDialog({
      title: 'Remove ' + a.name + '?',
      body: 'Sessionmux will close its Claude window and stop listing it.' +
        (deletable ? '\nIts data folder stays on disk unless you tick the box below.' : ''),
      okLabel: 'Remove',
      danger: true,
      check: deletable ? { label: 'Also delete its data folder (sign-in, settings and Code sessions)' } : null,
    });
    if (!r.confirmed) return;
    const res = await call('removeAccount', a.id, { deleteData: deletable && r.checked });
    if (res && res.ok === false) {
      toast('error', "Couldn't remove " + a.name, res.error);
    } else {
      toast('success', 'Removed ' + a.name, r.checked ? 'Its data folder was deleted.' : '');
      if (state.sessionsAccountId === a.id) state.sessionsAccountId = null;
      state.sessions.delete(a.id);
    }
    refreshAccounts();
  }

  async function refreshAccounts() {
    const list = await call('listAccounts');
    if (Array.isArray(list)) setAccounts(list);
  }

  /* ---------- Reorder (HTML5 DnD + Alt+Arrow) ---------- */

  function clearDropMarks() {
    accountList.querySelectorAll('.drop-before, .drop-after').forEach((el) => el.classList.remove('drop-before', 'drop-after'));
  }

  function endDrag() {
    clearDropMarks();
    accountList.querySelectorAll('.is-dragging').forEach((el) => el.classList.remove('is-dragging'));
    state.dragId = null;
    if (state.accountsDirty) renderAccounts();
  }

  accountList.addEventListener('dragover', (e) => {
    if (!state.dragId) return;
    const li = e.target.closest('.acct[data-id]');
    if (!li) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';
    const r = li.getBoundingClientRect();
    const after = e.clientY > r.top + r.height / 2;
    clearDropMarks();
    if (li.dataset.id !== state.dragId) li.classList.add(after ? 'drop-after' : 'drop-before');
  });

  accountList.addEventListener('dragleave', (e) => {
    if (!accountList.contains(e.relatedTarget)) clearDropMarks();
  });

  accountList.addEventListener('drop', (e) => {
    if (!state.dragId) return;
    e.preventDefault();
    const li = e.target.closest('.acct[data-id]');
    const moving = state.dragId;
    if (li && li.dataset.id !== moving) {
      const r = li.getBoundingClientRect();
      const after = e.clientY > r.top + r.height / 2;
      const ids = reorderIds(state.accounts.map((a) => a.id), moving, li.dataset.id, after);
      endDrag();
      commitOrder(ids);
    } else {
      endDrag();
    }
  });

  function nudgeAccount(id, delta) {
    const ids = state.accounts.map((a) => a.id);
    const i = ids.indexOf(id);
    const j = i + delta;
    if (i < 0 || j < 0 || j >= ids.length) return;
    commitOrder(reorderIds(ids, id, ids[j], delta > 0));
    const btn = accountList.querySelector('[data-key="acct-main-' + CSS.escape(id) + '"]');
    if (btn) btn.focus();
  }

  function commitOrder(ids) {
    const byId = new Map(state.accounts.map((a) => [a.id, a]));
    state.accounts = ids.map((id) => byId.get(id)).filter(Boolean);
    state.accountsDirty = true;
    renderAccounts();
    call('reorderAccounts', ids);
  }

  /* ---------- Inline account form (add + edit) ---------- */

  /**
   * Builds the shared add/edit form. Returns the <form>.
   * opts: { title, initial:{name,label,color}, submitLabel, excludeId, focus, onSubmit(values) → Promise<string|''>, onCancel }
   * onSubmit resolves to an error string to show, or '' on success.
   */
  function accountForm(opts) {
    let color = opts.initial.color || PALETTE[0];
    const uid = 'f' + Math.random().toString(36).slice(2, 8);

    const preview = h('span', { class: 'avatar avatar--sm', style: { '--avatar-color': color }, attrs: { 'aria-hidden': 'true' } });
    const nameInput = h('input', {
      class: 'input', id: uid + '-name', type: 'text', value: opts.initial.name || '', maxLength: 32,
      placeholder: 'e.g. Work', autocomplete: 'off',
      attrs: { spellcheck: 'false', 'aria-describedby': uid + '-err', required: true },
    });
    const labelInput = h('input', {
      class: 'input', id: uid + '-label', type: 'text', value: opts.initial.label || '', maxLength: 80,
      placeholder: 'Email or note (optional)', autocomplete: 'off', attrs: { spellcheck: 'false' },
    });
    const err = h('span', { class: 'field__error', id: uid + '-err', attrs: { 'aria-live': 'polite' } });

    const updatePreview = () => {
      preview.textContent = initialsOf(nameInput.value || opts.initial.name || '?');
      preview.style.setProperty('--avatar-color', color);
    };

    const swatchWrap = h('div', { class: 'swatches', attrs: { role: 'radiogroup', 'aria-label': 'Colour' } });
    const swatches = PALETTE.map((c) => h('button', {
      class: 'swatch', type: 'button', style: { '--swatch': c }, title: c,
      attrs: { role: 'radio', 'aria-label': 'Colour ' + c, 'aria-checked': c.toUpperCase() === color.toUpperCase() ? 'true' : 'false' },
      on: { click: () => pick(c) },
    }));
    // If the account uses a colour outside the palette, show it as an extra swatch.
    if (!PALETTE.some((c) => c.toUpperCase() === color.toUpperCase())) {
      const own = color;
      swatches.unshift(h('button', {
        class: 'swatch', type: 'button', style: { '--swatch': own }, title: own,
        attrs: { role: 'radio', 'aria-label': 'Current colour ' + own, 'aria-checked': 'true' },
        on: { click: () => pick(own) },
      }));
    }
    swatches.forEach((s) => { s.tabIndex = s.getAttribute('aria-checked') === 'true' ? 0 : -1; });
    swatchWrap.append(...swatches);

    function pick(c) {
      color = c;
      swatches.forEach((s) => {
        const on = s.title.toUpperCase() === c.toUpperCase();
        s.setAttribute('aria-checked', on ? 'true' : 'false');
        s.tabIndex = on ? 0 : -1;
      });
      updatePreview();
    }

    swatchWrap.addEventListener('keydown', (e) => {
      const i = swatches.indexOf(document.activeElement);
      if (i < 0) return;
      let n = -1;
      if (e.key === 'ArrowRight' || e.key === 'ArrowDown') n = (i + 1) % swatches.length;
      if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') n = (i - 1 + swatches.length) % swatches.length;
      if (n >= 0) {
        e.preventDefault();
        swatches[n].focus();
        pick(swatches[n].title);
      }
    });

    const submit = h('button', { class: 'btn btn--primary btn--sm', type: 'submit', text: opts.submitLabel });
    const cancel = h('button', { class: 'btn btn--ghost btn--sm', type: 'button', text: 'Cancel', on: { click: () => opts.onCancel() } });

    const form = h('form', { class: 'form', attrs: { 'aria-label': opts.title }, noValidate: true },
      h('div', { class: 'form__head' }, preview, h('h3', { class: 'form__title', text: opts.title })),
      opts.note ? h('p', { class: 'form__note', text: opts.note }) : null,
      h('div', { class: 'field' },
        h('label', { class: 'field__label', htmlFor: uid + '-name', text: 'Name' }),
        nameInput,
        err),
      h('div', { class: 'field' },
        h('label', { class: 'field__label', htmlFor: uid + '-label', text: 'Label' }),
        labelInput),
      h('div', { class: 'field' },
        h('span', { class: 'field__label', text: 'Colour' }),
        swatchWrap),
      h('div', { class: 'form__actions' }, cancel, submit));

    const showErr = (msg) => {
      err.textContent = msg;
      nameInput.setAttribute('aria-invalid', msg ? 'true' : 'false');
    };

    // Live validation (debounced): a clash with an existing account
    // name is shown and blocks submit while it lasts; other rule errors appear
    // once something is typed. A server name-taken error stays (and keeps
    // submit disabled) until the name is changed.
    let submitting = false;
    let serverErr = '';
    let serverErrFor = '';
    let liveTimer = null;
    function liveValidate() {
      if (!form.isConnected) { state.formValidators.delete(liveValidate); return; }
      const value = nameInput.value;
      const clash = findNameClash(value, state.accounts, opts.excludeId);
      const serverHit = !!serverErr && nameKey(value) === serverErrFor;
      let msg = '';
      if (clash) msg = nameTakenMessage(clash.name);
      else if (serverHit) msg = serverErr;
      else if (value.trim() !== '' || err.textContent) msg = validateName(value, state.accounts, opts.excludeId);
      showErr(msg);
      if (!submitting) submit.disabled = !!clash || serverHit;
    }
    const scheduleLive = () => {
      clearTimeout(liveTimer);
      liveTimer = setTimeout(liveValidate, 150);
    };
    state.formValidators.add(liveValidate);

    nameInput.addEventListener('input', () => {
      updatePreview();
      scheduleLive();
    });

    form.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') {
        // Close the form, not the panel.
        e.preventDefault();
        e.stopPropagation();
        opts.onCancel();
      }
    });

    form.addEventListener('submit', async (e) => {
      e.preventDefault();
      if (submitting) return;
      clearTimeout(liveTimer);
      const name = nameInput.value.trim();
      const msg = validateName(name, state.accounts, opts.excludeId);
      if (msg) {
        showErr(msg);
        submit.disabled = !!findNameClash(name, state.accounts, opts.excludeId);
        nameInput.focus();
        return;
      }
      submitting = true;
      submit.disabled = true;
      cancel.disabled = true;
      const result = await opts.onSubmit({ name, label: labelInput.value.trim(), color });
      submitting = false;
      // onSubmit resolves to '' (done), an error string, or { error, blockName }
      // where blockName keeps submit disabled until the name is edited.
      const failure = result && typeof result === 'object' ? result.error : result;
      if (!form.isConnected) { state.formValidators.delete(liveValidate); return; }
      if (failure) {
        cancel.disabled = false;
        if (result && typeof result === 'object' && result.blockName) {
          serverErr = failure;
          serverErrFor = nameKey(name);
        }
        submit.disabled = !!serverErr && nameKey(nameInput.value) === serverErrFor;
        showErr(failure);
        nameInput.focus();
      } else {
        state.formValidators.delete(liveValidate);
      }
    });

    updatePreview();
    // Focus once the caller has inserted the form (it does so synchronously),
    // and flag a prefilled name that already clashes (e.g. an adopted folder).
    queueMicrotask(() => {
      liveValidate();
      if (opts.focus === 'color') {
        const sel = swatches.find((s) => s.getAttribute('aria-checked') === 'true') || swatches[0];
        sel.focus();
      } else {
        nameInput.focus();
        nameInput.select();
      }
    });
    return form;
  }

  function startEdit(id, focus) {
    const a = accountById(id);
    if (!a) return;
    if (state.adding) closeAddForm();
    // Ensure the list reflects the latest data before swapping one row for the form.
    state.editingId = null;
    state.accountsDirty = true;
    renderAccounts();
    state.editingId = id;
    const li = accountList.querySelector('.acct[data-id="' + CSS.escape(id) + '"]');
    if (!li) { state.editingId = null; return; }
    const form = accountForm({
      title: focus === 'color' ? 'Colour for ' + a.name : 'Edit ' + a.name,
      initial: { name: a.name, label: a.label || '', color: a.color },
      submitLabel: 'Save',
      excludeId: a.id,
      focus,
      onCancel: () => endEdit(id),
      onSubmit: async (v) => {
        const patch = {};
        if (v.name !== a.name) patch.name = v.name;
        if ((v.label || '') !== (a.label || '')) patch.label = v.label || null;
        if (String(v.color).toUpperCase() !== String(a.color).toUpperCase()) patch.color = v.color;
        if (Object.keys(patch).length === 0) { endEdit(id); return ''; }
        const res = await call('renameAccount', id, patch);
        if (res && res.ok === false) {
          if (res.reason === 'name-taken') return { error: res.error || 'That name is already taken.', blockName: true };
          return res.error || 'Could not save changes.';
        }
        // Optimistic local update; the accounts-changed push will confirm.
        Object.assign(a, patch, patch.name ? { initials: initialsOf(patch.name) } : {});
        endEdit(id);
        refreshAccounts();
        return '';
      },
    });
    const wrap = h('li', { class: 'acct-edit', dataset: { id } }, form);
    li.replaceWith(wrap);
  }

  function endEdit(id) {
    state.formValidators.clear();
    state.editingId = null;
    state.accountsDirty = true;
    renderAccounts();
    const btn = accountList.querySelector('[data-key="acct-main-' + CSS.escape(id) + '"]');
    if (btn) btn.focus();
  }

  /**
   * Open the inline add-account form. With `disc` (a Discovered profile
   * folder) the form is prefilled and submitting adopts that folder via
   * adoptProfile instead of creating a new one with addAccount.
   */
  function openAddForm(disc) {
    if (state.editingId) endEdit(state.editingId);
    state.adding = true;
    state.adoptingDir = disc ? disc.dataDir : null;
    btnAdd.setAttribute('aria-expanded', disc ? 'false' : 'true');
    syncAdoptExpanded();
    addHost.textContent = '';
    const returnFocus = () => {
      const btn = disc && discList.querySelector('[data-key="disc-add-' + CSS.escape(disc.dataDir) + '"]');
      (btn || btnAdd).focus();
    };
    const form = accountForm({
      title: disc ? 'Add ' + (disc.folderName || 'profile folder') : 'Add account',
      initial: {
        name: disc ? defaultProfileName(disc.folderName, state.accounts) : '',
        label: '',
        color: nextColor(state.accounts),
      },
      submitLabel: disc ? 'Add' : 'Add & open',
      excludeId: null,
      // Where every link handler change asks the user, an e-mail code is the easy way in.
      note: !disc && state.signIn && state.signIn.needsConsent ? EMAIL_CODE_ADVICE : null,
      onCancel: () => { closeAddForm(); returnFocus(); },
      onSubmit: async (v) => {
        if (disc) {
          if (typeof api.adoptProfile !== 'function') return 'This version of Sessionmux cannot add existing folders.';
          const opts = { name: v.name, color: v.color };
          if (v.label) opts.label = v.label;
          const res = await call('adoptProfile', disc.dataDir, opts);
          if (!res || res.ok === false) {
            if (res && res.reason === 'name-taken') return { error: res.error || 'That name is already taken.', blockName: true };
            if (res && res.reason === 'same-sign-in') refreshDiscovered(); // the row then shows the badge and locks Add
            return (res && res.error) || 'Could not add the profile folder.';
          }
          closeAddForm();
          toast('success', 'Added ' + v.name, (disc.folderName || 'The folder') + ' is now listed as an account.');
          state.discovered = state.discovered.filter((d) => d.dataDir !== disc.dataDir);
          renderDiscovered();
          refreshAccounts();
          refreshDiscovered();
          btnAdd.focus();
          return '';
        }
        const res = await call('addAccount', { name: v.name, color: v.color, label: v.label || undefined });
        if (!res || res.ok === false) {
          if (res && res.reason === 'name-taken') return { error: res.error || 'That name is already taken.', blockName: true };
          return (res && res.error) || 'Could not add the account.';
        }
        closeAddForm();
        toast('success', 'Added ' + v.name, 'Sign in in the Claude window that just opened.');
        refreshAccounts();
        refreshDiscovered();
        return '';
      },
    });
    addHost.appendChild(form);
    requestAnimationFrame(() => form.scrollIntoView({ block: 'nearest', behavior: 'smooth' }));
  }

  function closeAddForm() {
    state.formValidators.clear();
    state.adding = false;
    state.adoptingDir = null;
    btnAdd.setAttribute('aria-expanded', 'false');
    syncAdoptExpanded();
    addHost.textContent = '';
  }

  btnAdd.addEventListener('click', () => (state.adding && !state.adoptingDir ? closeAddForm() : openAddForm()));

  /* ---------- Found profile folders (discovered, not yet added) ---------- */

  const discHost = $('discoveredHost');
  const discList = $('discoveredList');
  let discJson = '';
  let discInFlight = null;
  let discQueued = false;

  function setDiscovered(list) {
    if (!Array.isArray(list)) return;
    state.discovered = list.filter((d) => d && typeof d.dataDir === 'string' && d.dataDir);
    // An open "adopt" form whose folder vanished has nothing left to adopt.
    if (state.adoptingDir && !state.discovered.some((d) => d.dataDir === state.adoptingDir && !d.sameSignInAs)) {
      const hadFocus = addHost.contains(document.activeElement);
      closeAddForm();
      if (hadFocus) btnAdd.focus();
    }
    renderDiscovered();
  }

  /** Re-fetch the discovered list (coalesces overlapping calls). */
  function refreshDiscovered() {
    if (typeof api.listDiscovered !== 'function') return Promise.resolve();
    if (discInFlight) { discQueued = true; return discInFlight; }
    discInFlight = (async () => {
      try {
        const list = await call('listDiscovered');
        if (Array.isArray(list)) setDiscovered(list);
      } finally {
        discInFlight = null;
        if (discQueued) { discQueued = false; refreshDiscovered(); }
      }
    })();
    return discInFlight;
  }

  function syncAdoptExpanded() {
    if (!discList) return;
    discList.querySelectorAll('[data-adopt]').forEach((b) => {
      b.setAttribute('aria-expanded', b.dataset.adopt === state.adoptingDir ? 'true' : 'false');
    });
  }

  function discSubtitle(d) {
    const n = Number(d.sessionCount) || 0;
    // Count + a status badge; badges wrap cleanly, a "·" separator would dangle.
    const parts = [h('span', { class: 'disc__count', text: plural(n, 'session') })];
    if (d.sameSignInAs) {
      parts.push(h('span', { class: 'badge badge--warn disc__warn', text: 'Same sign-in as ' + d.sameSignInAs }));
    } else {
      parts.push(h('span', { class: 'badge', text: d.signedIn ? 'Signed in' : 'Not signed in' }));
    }
    return parts;
  }

  function discRow(d) {
    const busy = state.busyDiscovered.has(d.dataDir);
    const name = d.folderName || d.dataDir;
    const sub = discSubtitle(d);
    const sameAs = typeof d.sameSignInAs === 'string' && d.sameSignInAs ? d.sameSignInAs : null;
    const blockedMsg = sameAs ? sameSignInMessage(sameAs) : null;
    const li = h('li', { class: 'disc' + (busy ? ' is-busy' : ''), dataset: { dir: d.dataDir }, title: d.dataDir },
      h('span', { class: 'disc__icon' }, icon('folder')),
      h('span', { class: 'disc__text' },
        h('span', { class: 'disc__name', text: name }),
        h('span', {
          class: 'disc__desc',
          title: d.sameSignInAs ? sameSignInHint(d.sameSignInAs) : null,
        }, sub)),
      h('span', { class: 'disc__actions' },
        h('button', {
          class: 'btn btn--secondary btn--sm',
          type: 'button',
          text: 'Add',
          disabled: busy || !!sameAs,
          title: blockedMsg,
          dataset: { key: 'disc-add-' + d.dataDir, adopt: d.dataDir },
          attrs: {
            'aria-label': sameAs ? 'Add ' + name + ' (unavailable: ' + blockedMsg + ')' : 'Add ' + name + ' as an account',
            'aria-expanded': state.adoptingDir === d.dataDir ? 'true' : 'false',
          },
          on: {
            click: () => {
              if (sameAs) return;
              if (state.adoptingDir === d.dataDir) closeAddForm(); else openAddForm(d);
            },
          },
        }),
        h('button', {
          class: 'btn btn--ghost btn--sm',
          type: 'button',
          text: 'Ignore',
          disabled: busy || typeof api.ignoreProfile !== 'function',
          dataset: { key: 'disc-ignore-' + d.dataDir },
          attrs: { 'aria-label': 'Ignore ' + name + ' (stop showing it here)' },
          on: { click: () => ignoreDiscovered(d) },
        })));
    return li;
  }

  function renderDiscovered() {
    if (!discHost || !discList) return;
    const list = state.discovered;
    const snapshot = JSON.stringify([list, [...state.busyDiscovered], state.adoptingDir]);
    if (snapshot === discJson) return;
    discJson = snapshot;
    withFocusKept(() => {
      discList.textContent = '';
      for (const d of list) discList.appendChild(discRow(d));
    });
    discHost.hidden = list.length === 0;
  }

  async function ignoreDiscovered(d) {
    if (state.busyDiscovered.has(d.dataDir)) return;
    const name = d.folderName || d.dataDir;
    // Where focus goes once the row is gone: the next row's Add, else the previous, else "Add account".
    const idx = state.discovered.findIndex((x) => x.dataDir === d.dataDir);
    const neighbour = state.discovered[idx + 1] || state.discovered[idx - 1] || null;
    const hadFocus = discList.contains(document.activeElement);
    state.busyDiscovered.add(d.dataDir);
    renderDiscovered();
    const res = await call('ignoreProfile', d.dataDir);
    state.busyDiscovered.delete(d.dataDir);
    if (!res || res.ok === false) {
      renderDiscovered();
      if (hadFocus) {
        const again = discList.querySelector('[data-key="disc-ignore-' + CSS.escape(d.dataDir) + '"]');
        if (again) again.focus();
      }
      toast('error', "Couldn't ignore " + name, (res && res.error) || 'Sessionmux could not save that.');
      return;
    }
    if (state.adoptingDir === d.dataDir) closeAddForm();
    state.discovered = state.discovered.filter((x) => x.dataDir !== d.dataDir);
    renderDiscovered();
    if (hadFocus) {
      let next = neighbour && discList.querySelector('[data-key="disc-add-' + CSS.escape(neighbour.dataDir) + '"]');
      if (next && next.disabled) next = discList.querySelector('[data-key="disc-ignore-' + CSS.escape(neighbour.dataDir) + '"]');
      (next || btnAdd).focus();
    }
    toast('info', 'Ignored ' + name, 'Sessionmux will not suggest this folder again.');
    refreshDiscovered();
  }

  btnOpenAll.addEventListener('click', async () => {
    btnOpenAll.disabled = true;
    toast('info', 'Opening all accounts…');
    const res = await call('openAll');
    btnOpenAll.disabled = false;
    if (res && res.ok === false) toast('error', "Couldn't open every account", res.error);
    refreshAccounts();
  });

  /* ======================================================================
   * Sessions tab
   * ==================================================================== */

  const chipsEl = $('sessionChips');
  const sessionList = $('sessionList');
  const searchInput = $('sessionSearch');
  const btnRefresh = $('btnRefreshSessions');

  function enterSessions() {
    if (!state.sessionsAccountId || !accountById(state.sessionsAccountId)) {
      const pick = accountById(state.activeId) || state.accounts[0];
      state.sessionsAccountId = pick ? pick.id : null;
    }
    renderChips();
    if (state.sessionsAccountId) loadSessions(state.sessionsAccountId);
    else renderSessions();
  }

  function renderChips() {
    withFocusKept(() => {
      chipsEl.textContent = '';
      for (const a of state.accounts) {
        const on = a.id === state.sessionsAccountId;
        const count = state.sessions.has(a.id) && Array.isArray(state.sessions.get(a.id).list)
          ? state.sessions.get(a.id).list.length
          : Number(a.sessionCount) || 0;
        chipsEl.appendChild(h('button', {
          class: 'chip', type: 'button', tabIndex: on ? 0 : -1,
          dataset: { id: a.id, key: 'chip-' + a.id },
          attrs: { role: 'radio', 'aria-checked': on ? 'true' : 'false', 'aria-label': a.name + ', ' + plural(count, 'session') },
          on: { click: () => selectSessionAccount(a.id) },
        }, avatarEl(a, 'avatar--xs'), h('span', { text: a.name }), h('span', { class: 'chip__count badge--count', text: String(count), attrs: { 'aria-hidden': 'true' } })));
      }
      if (!state.accounts.length && !state.accountsLoaded) {
        chipsEl.appendChild(h('span', { class: 'sk', style: { width: '80px', height: '28px', 'border-radius': '999px' } }));
        chipsEl.appendChild(h('span', { class: 'sk', style: { width: '64px', height: '28px', 'border-radius': '999px' } }));
      }
    });
    const sel = chipsEl.querySelector('[aria-checked="true"]');
    if (sel && document.activeElement !== sel) sel.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  }

  chipsEl.addEventListener('keydown', (e) => {
    const chips = Array.from(chipsEl.querySelectorAll('.chip'));
    const i = chips.indexOf(document.activeElement);
    if (i < 0) return;
    let n = -1;
    if (e.key === 'ArrowRight') n = Math.min(chips.length - 1, i + 1);
    else if (e.key === 'ArrowLeft') n = Math.max(0, i - 1);
    else if (e.key === 'Home') n = 0;
    else if (e.key === 'End') n = chips.length - 1;
    if (n >= 0 && n !== i) {
      e.preventDefault();
      selectSessionAccount(chips[n].dataset.id);
      const again = chipsEl.querySelector('[data-id="' + CSS.escape(chips[n].dataset.id) + '"]');
      if (again) again.focus();
    }
  });

  function selectSessionAccount(id) {
    if (state.sessionsAccountId === id) return;
    closeMenu(false);
    state.sessionsAccountId = id;
    renderChips();
    $('sessionsScroll').scrollTop = 0;
    loadSessions(id);
  }

  /** Fetch sessions for an account; cached entries render immediately while refreshing. */
  async function loadSessions(id, opts) {
    const quiet = opts && opts.quiet;
    const entry = state.sessions.get(id) || { list: null, loading: false, error: null, loadedAt: 0 };
    if (entry.loading) return;
    entry.loading = true;
    entry.error = null;
    state.sessions.set(id, entry);
    if (!quiet) {
      btnRefresh.classList.add('is-spinning');
      if (id === state.sessionsAccountId) renderSessions();
    }
    const res = await call('listSessions', id);
    entry.loading = false;
    entry.loadedAt = Date.now();
    if (Array.isArray(res)) {
      entry.list = res;
    } else {
      entry.error = (res && res.error) || 'Could not read sessions.';
    }
    btnRefresh.classList.remove('is-spinning');
    if (id === state.sessionsAccountId && state.tab === 'sessions') {
      renderSessions();
      renderChips();
    }
  }

  function renderSessions() {
    const acct = accountById(state.sessionsAccountId);
    const entry = acct ? state.sessions.get(acct.id) : null;

    withFocusKept(() => {
      sessionList.textContent = '';
      sessionList.setAttribute('aria-busy', entry && entry.loading && !entry.list ? 'true' : 'false');

      if (!acct) {
        sessionList.appendChild(h('li', null, emptyState('user', 'No account selected', 'Pick an account above to see its Code sessions.')));
        return;
      }
      if (!entry || (entry.loading && !entry.list)) {
        for (let i = 0; i < 4; i++) {
          sessionList.appendChild(h('li', { class: 'sess sess--skeleton', attrs: { 'aria-hidden': 'true' } },
            h('span', { class: 'sk sk--line', style: { width: (60 - i * 7) + '%' } }),
            h('span', { class: 'sk sk--line sk--short' })));
        }
        return;
      }
      if (entry.error && !entry.list) {
        sessionList.appendChild(h('li', null, emptyState('alert', "Couldn't load sessions", entry.error,
          { label: 'Try again', onClick: () => loadSessions(acct.id) })));
        return;
      }
      const all = entry.list || [];
      if (all.length === 0) {
        const neverSignedIn = !acct.accountUuid;
        sessionList.appendChild(h('li', null, neverSignedIn
          ? emptyState('user', acct.name + " isn't signed in yet", 'Open its Claude window and sign in, then start a Code session.',
            { label: 'Open ' + acct.name, onClick: () => openAccount(acct.id) })
          : emptyState('inbox', 'No Code sessions yet', 'Sessions you start in ' + acct.name + "'s Code tab will show up here.")));
        return;
      }
      // Archived sessions sink to the bottom; otherwise keep the app's order (most recent first).
      const shown = all.filter((s) => matchesQuery(s, state.search))
        .map((s, i) => ({ s, i }))
        .sort((x, y) => (x.s.isArchived ? 1 : 0) - (y.s.isArchived ? 1 : 0) || x.i - y.i)
        .map((x) => x.s);
      if (shown.length === 0) {
        sessionList.appendChild(h('li', null, emptyState('search', 'No matches', 'Nothing matches “' + state.search.trim() + '”.',
          { label: 'Clear search', onClick: () => { searchInput.value = ''; state.search = ''; renderSessions(); searchInput.focus(); } })));
        return;
      }
      for (const s of shown) sessionList.appendChild(sessionRow(acct, s));
    });
  }

  function sessionRow(acct, s) {
    const busy = state.busySessions.has(s.sessionId);
    const others = state.accounts.filter((a) => a.id !== acct.id);
    const title = s.title && String(s.title).trim() ? String(s.title) : 'Untitled session';
    const leaf = cwdLeaf(s.cwd);
    const model = modelLabel(s.model);
    const keyBase = 'sess-' + s.sessionId;

    // A running session can be copied or moved: it is copied as it is now (see transfer()).
    const blockReason = others.length === 0 ? 'Add another account first.' : null;
    const waitKind = sessionWaitingKind(acct, s);
    const waitText = waitingLabel(waitKind);

    const transferBtn = (mode) => {
      const b = h('button', {
        class: 'btn btn--ghost btn--sm', type: 'button',
        disabled: !!blockReason,
        title: blockReason || (mode === 'move' ? 'Move this session to another account' : 'Copy this session to another account'),
        dataset: { key: keyBase + '-' + mode },
        attrs: { 'aria-haspopup': 'menu', 'aria-expanded': 'false' },
        on: {
          click: (e) => {
            e.stopPropagation();
            li.classList.add('has-open-menu');
            toggleMenu(b, () => transferItems(acct, s, mode), {
              label: (mode === 'move' ? 'Move' : 'Copy') + ' to account',
              onClose: () => li.classList.remove('has-open-menu'),
            });
          },
        },
      }, icon(mode === 'move' ? 'move' : 'copy'), mode === 'move' ? 'Move to' : 'Copy to', h('span', { class: 'caret', text: '▾', attrs: { 'aria-hidden': 'true' } }));
      return b;
    };

    const li = h('li', {
      class: 'sess' + (s.isArchived ? ' is-archived' : '') + (busy ? ' is-busy' : ''),
      dataset: { id: s.sessionId },
      attrs: {
        'aria-label': title + (waitText ? ', ' + waitText.toLowerCase() : '') + (s.running ? ', running' : '') +
          (s.isArchived ? ', archived' : ''),
      },
    },
    h('div', { class: 'sess__top' },
      h('span', { class: 'sess__title', text: title, title }),
      waitText ? h('span', {
        class: 'badge ' + (waitKind === 'permission' ? 'badge--warn' : 'badge--accent') + ' sess__waiting',
        text: waitText,
        title: waitKind === 'permission' ? 'Claude is waiting for your approval' : 'Claude is waiting for you',
      }) : null,
      s.running ? h('span', { class: 'badge badge--success', text: 'running' }) : null,
      s.isScratch ? h('span', { class: 'badge', text: 'scratch', title: 'Started without a project folder' }) : null,
      s.isArchived ? h('span', { class: 'badge', text: 'archived' }) : null,
      s.hasTranscript === false ? h('span', { class: 'badge badge--warn', text: 'no transcript', title: 'No transcript file was found for this session' }) : null),
    h('div', { class: 'sess__meta' },
      leaf ? h('span', { class: 'sess__cwd', title: s.cwd }, icon('folder'), h('span', { class: 'sess__cwd-text', text: leaf })) : null,
      leaf ? h('span', { class: 'sep', text: '·', attrs: { 'aria-hidden': 'true' } }) : null,
      h('time', {
        class: 'sess__time',
        dataset: { ts: String(s.lastActivityAt || s.createdAt || '') },
        title: absoluteTime(s.lastActivityAt || s.createdAt),
        text: relativeTime(s.lastActivityAt || s.createdAt) || '—',
      }),
      model ? h('span', { class: 'badge badge--accent', text: model, title: s.model + (s.effort ? ' · effort ' + s.effort : '') }) : null),
    h('div', { class: 'sess__actions' },
      transferBtn('move'),
      transferBtn('copy'),
      h('span', { class: 'spacer' }),
      h('button', {
        class: 'icon-btn icon-btn--sm', type: 'button', title: 'Show folder',
        dataset: { key: keyBase + '-folder' },
        attrs: { 'aria-label': 'Show folder of ' + title },
        on: {
          click: async () => {
            const res = await call('openSessionFolder', acct.id, s.sessionId);
            if (res && res.ok === false) toast('error', "Couldn't show the folder", res.error || '');
          },
        },
      }, icon('folder')),
      h('button', {
        class: 'icon-btn icon-btn--sm icon-btn--danger', type: 'button',
        title: s.running ? 'Close the session in Claude before deleting it' : 'Delete session',
        disabled: !!s.running,
        dataset: { key: keyBase + '-delete' },
        attrs: { 'aria-label': 'Delete ' + title },
        on: { click: () => deleteSession(acct, s, title) },
      }, icon('trash')),
      h('button', {
        class: 'icon-btn icon-btn--sm sess__more', type: 'button', title: 'More actions',
        dataset: { key: keyBase + '-more' },
        attrs: { 'aria-label': 'More actions for ' + title, 'aria-haspopup': 'menu', 'aria-expanded': 'false' },
        on: { click: (e) => { e.stopPropagation(); openSessionMenu(e.currentTarget, li, acct, s); } },
      }, icon('more'))));
    li.addEventListener('contextmenu', (e) => {
      e.preventDefault();
      openSessionMenu(li.querySelector('.sess__more'), li, acct, s);
    });
    return li;
  }

  /** The waiting kind of a session row (from listWaiting or a pending approval), or ''. */
  function sessionWaitingKind(acct, s) {
    const ids = [s.sessionId, s.cliSessionId].filter((x) => typeof x === 'string' && x);
    const same = (x) => !!x && x.accountId === acct.id && ids.includes(x.sessionId);
    if (state.approvals.some(same)) return 'permission';
    const w = state.waiting.find(same);
    return w && waitingLabel(w.kind) ? w.kind : '';
  }

  function openSessionMenu(anchor, li, acct, s) {
    if (!anchor) return;
    li.classList.add('has-open-menu');
    toggleMenu(anchor, () => {
      const target = handoverTarget(state.accounts, acct.id);
      let hint = null;
      if (target) hint = 'To ' + target.name;
      else if (state.accounts.some((a) => a.id !== acct.id && a.limited)) hint = 'Every other account is limited';
      else hint = 'No other account is ready';
      return [{ label: 'Hand over…', icon: 'move', hint, onSelect: () => handOver(acct, s) }];
    }, { label: 'Actions for this session', onClose: () => li.classList.remove('has-open-menu') });
  }

  /**
   * Copy a session to the next account that is signed in and not limited, then show it there.
   * The app opens that account's Claude window when none is open; it restarts an open one only
   * when `restart` is ticked and no session is running there.
   */
  async function handOver(from, s) {
    const title = s.title && String(s.title).trim() ? String(s.title) : 'Untitled session';
    const to = handoverTarget(state.accounts, from.id);
    if (!to) { noHandoverTarget(from.id, null); return; }
    const lines = [
      '“' + title + '” is copied to ' + to.name + '. Sessionmux opens ' + to.name + "'s Claude window if it is closed, and restarts an open one (so the session appears) only when the box below is ticked and no session is running there.",
    ];
    if (s.running) {
      lines.push('The session is open in ' + from.name + '. Sessionmux copies it as it is now; the original stays open there until you close it. Anything Claude is still writing will not be in the copy.');
    } else {
      lines.push('The original stays in ' + from.name + '.');
    }
    const r = await confirmDialog({
      title: 'Hand over to ' + to.name + '?',
      body: lines.join('\n'),
      okLabel: 'Hand over',
      check: { label: 'Restart ' + to.name + "'s Claude window if no session is running there", checked: !to.running },
    });
    if (!r.confirmed) return;
    state.busySessions.add(s.sessionId);
    renderSessions();
    const res = await call('handOverSession', { sessionId: s.sessionId, fromId: from.id, to: to.id, restart: r.checked === true });
    state.busySessions.delete(s.sessionId);
    if (res && res.ok === false) {
      if (res.reason === 'no-target') noHandoverTarget(from.id, res.until);
      else toast('error', "Couldn't hand over the session", res.error || 'Something went wrong.');
    } else if (!res) {
      toast('error', "Couldn't hand over the session", 'No response from Sessionmux.');
    } else {
      const name = (typeof res.toName === 'string' && res.toName) || to.name;
      const notes = [];
      if (sourceRunning(res)) notes.push('The original is still open in ' + from.name + '; close it there when you are done.');
      const skipped = skippedFiles(res.warnings);
      if (skipped > 0) notes.push(plural(skipped, 'file') + (skipped === 1 ? ' was' : ' were') + ' in use and could not be copied.');
      const warn = skipped > 0 || needsRestart(res) || res.windowLeftOpen === true;
      toast(warn ? 'warn' : 'success', handoverHeadline(res, name), notes.join(' '), warn ? 10000 : undefined);
    }
    state.sessions.delete(to.id);
    await loadSessions(from.id, { quiet: true });
    refreshAccounts();
  }

  /** True when the copy shows up only after the target's window is restarted by hand; a window
   *  that failed to open or restart counts too. */
  function needsRestart(res) {
    return res.restartNeeded === true || res.opened === false;
  }

  /** The toast title for a finished hand-over, from the app's restartNeeded / windowLeftOpen. */
  function handoverHeadline(res, name) {
    if (res.windowLeftOpen === true) return 'Handed over to ' + name + '; its window stays open because a session is running there.';
    if (needsRestart(res)) return 'Handed over to ' + name + '. Restart ' + name + "'s Claude window to see it.";
    return 'Handed over to ' + name + '.';
  }

  /** Toast for an automatic handover the app finished on its own. */
  function handoverDone(res) {
    if (!res || typeof res !== 'object' || res.ok === false) return;
    const name = (typeof res.toName === 'string' && res.toName) || 'another account';
    const from = (typeof res.fromName === 'string' && res.fromName) || 'the limited account';
    const notes = [];
    if (sourceRunning(res)) notes.push('The original is still open in ' + from + '; close it there when you are done.');
    const skipped = skippedFiles(res.warnings);
    if (skipped > 0) notes.push(plural(skipped, 'file') + (skipped === 1 ? ' was' : ' were') + ' in use and could not be copied.');
    const warn = skipped > 0 || needsRestart(res);
    toast(warn ? 'warn' : 'success', handoverHeadline(res, name), notes.join(' '), warn ? 10000 : undefined);
    if (typeof res.toId === 'string') state.sessions.delete(res.toId);
    if (typeof res.fromId === 'string' && state.sessions.has(res.fromId)) loadSessions(res.fromId, { quiet: true });
    refreshAccounts();
  }

  /** Toast for a handover with nowhere to go; `until` (unix) is the app's earliest reset, if any. */
  function noHandoverTarget(sourceId, until) {
    const others = state.accounts.filter((a) => a.id !== sourceId);
    if (!others.some((a) => a.limited)) {
      toast('error', 'No other account can take it', 'Sign in to another account and open its Code tab once.');
      return;
    }
    const ms = Number.isFinite(unixMs(until)) ? unixMs(until) : earliestReset(state.accounts, sourceId);
    const at = ms ? clockTime(ms) : '';
    toast('warn', 'Every other account is limited', at ? 'The first one resets at ' + at + '.' : '', 7000);
  }

  function transferItems(acct, s, mode) {
    const others = state.accounts.filter((a) => a.id !== acct.id);
    return [{ heading: (mode === 'move' ? 'Move' : 'Copy') + ' to' }].concat(others.map((t) => {
      let disabled = false;
      let hint = null;
      if (!t.accountUuid) {
        disabled = true;
        hint = 'Not signed in yet — open it and sign in first';
      } else if (t.codeReady === false) {
        disabled = true;
        hint = "Hasn't opened the Code tab yet — open it once in Claude";
      } else if (t.limited) {
        hint = limitText(t.limited);
      } else if (t.running) {
        hint = "Window is open — you'll need to restart it";
      } else if (t.label) {
        hint = t.label;
      }
      return { label: t.name, avatar: t, disabled, hint, onSelect: () => transfer(acct, s, t, mode) };
    }));
  }

  async function transfer(from, s, to, mode) {
    const title = s.title && String(s.title).trim() ? String(s.title) : 'Untitled session';
    if (mode === 'move') {
      const r = await confirmDialog({
        title: 'Move to ' + to.name + '?',
        body: s.running
          ? '“' + title + '” will move from ' + from.name + ' to ' + to.name + '.\nThe session is open in ' + from.name +
            '. Sessionmux copies it as it is now; the original stays open there until you close it. Anything Claude is still writing will not be in the copy.'
          : '“' + title + '” will move from ' + from.name + ' to ' + to.name + '. The original entry is removed after the copy succeeds.',
        okLabel: 'Move',
      });
      if (!r.confirmed) return;
    }
    state.busySessions.add(s.sessionId);
    renderSessions();
    const res = await call('transferSession', { fromId: from.id, toId: to.id, sessionId: s.sessionId, mode });
    state.busySessions.delete(s.sessionId);
    const msg = transferMessage(res || { ok: false, error: 'No response from Sessionmux.' }, mode, to.name, from.name);
    toast(msg.kind, msg.title, msg.body);
    state.sessions.delete(to.id); // target list is stale now
    await loadSessions(from.id, { quiet: true });
    refreshAccounts();
  }

  async function deleteSession(acct, s, title) {
    const r = await confirmDialog({
      title: 'Delete session?',
      body: '“' + title + '” will be removed from ' + acct.name + '.\nThe transcript in your .claude folder stays on disk.',
      okLabel: 'Delete',
      danger: true,
    });
    if (!r.confirmed) return;
    state.busySessions.add(s.sessionId);
    renderSessions();
    const res = await call('deleteSession', acct.id, s.sessionId);
    state.busySessions.delete(s.sessionId);
    if (res && res.ok === false) toast('error', "Couldn't delete session", res.error);
    else toast('success', 'Session deleted');
    await loadSessions(acct.id, { quiet: true });
    refreshAccounts();
  }

  let searchTimer = null;
  searchInput.addEventListener('input', () => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      state.search = searchInput.value;
      renderSessions();
    }, 80);
  });
  searchInput.addEventListener('keydown', (e) => {
    // First Escape clears the search; a second one hides the panel.
    if (e.key === 'Escape' && searchInput.value) {
      e.preventDefault();
      e.stopPropagation();
      searchInput.value = '';
      state.search = '';
      renderSessions();
    } else if (e.key === 'ArrowDown') {
      const first = sessionList.querySelector('button:not(:disabled)');
      if (first) { e.preventDefault(); first.focus(); }
    }
  });

  btnRefresh.addEventListener('click', () => {
    if (state.sessionsAccountId) loadSessions(state.sessionsAccountId);
  });

  /* ---------- Waiting for your approval ---------- */

  const approvalsHost = $('approvalsHost');
  const approvalList = $('approvalList');
  const sessionsTabCount = $('sessionsTabCount');
  const sessionsTab = $('tab-sessions');
  const approvalCards = new Map(); // id → { li, allow, timer, armTimer, busy }
  const seenApprovals = new Set(); // ids already announced with a toast
  const ARM_MS = 600;

  const approvalTitle = (a) => (a.sessionTitle && String(a.sessionTitle).trim() ? String(a.sessionTitle) : 'Untitled session');

  /**
   * Allow stays disabled for ARM_MS after a card is shown, with a visible arming bar, so a click
   * or key press meant for something else can never approve a request that just appeared.
   */
  function armAllow(card) {
    clearTimeout(card.armTimer);
    card.allow.disabled = true;
    card.allow.classList.remove('is-arming');
    void card.allow.offsetWidth; // restart the arming bar
    card.allow.classList.add('is-arming');
    card.armTimer = setTimeout(() => {
      card.allow.classList.remove('is-arming');
      if (!card.busy) card.allow.disabled = false;
    }, ARM_MS);
  }

  function armAllCards() {
    approvalCards.forEach((card) => { if (!card.busy) armAllow(card); });
  }

  function approvalCard(a) {
    const acct = accountById(a.accountId);
    const who = { name: a.accountName || (acct && acct.name) || 'Account', color: acct && acct.color };
    const title = approvalTitle(a);
    const tool = String(a.toolName || 'Tool');
    const summary = String(a.summary || '');
    const long = summaryIsLong(summary);
    const keyBase = 'appr-' + a.id;

    const code = h('pre', { class: 'appr__summary' + (long ? ' is-collapsed' : '') },
      h('span', { class: 'appr__text', text: summary || tool }));
    const more = long ? h('button', {
      class: 'link-btn appr__more', type: 'button', text: 'Show more',
      dataset: { key: keyBase + '-more' },
      attrs: { 'aria-expanded': 'false' },
      on: {
        click: () => {
          const open = code.classList.toggle('is-collapsed') === false;
          more.textContent = open ? 'Show less' : 'Show more';
          more.setAttribute('aria-expanded', open ? 'true' : 'false');
        },
      },
    }) : null;

    const deny = h('button', {
      class: 'btn btn--secondary btn--sm', type: 'button', text: 'Deny',
      dataset: { key: keyBase + '-deny' },
      attrs: { 'aria-label': 'Deny ' + tool + ' in ' + title },
      on: { click: () => decide(a, 'deny') },
    });
    const allow = h('button', {
      class: 'btn btn--primary btn--sm appr__allow', type: 'button', text: 'Allow',
      dataset: { key: keyBase + '-allow' },
      attrs: { 'aria-label': 'Allow ' + tool + ' in ' + title },
      on: {
        click: () => decide(a, 'allow'),
        // Enter never approves; only a click or Space on the focused button does.
        keydown: (e) => { if (e.key === 'Enter') { e.preventDefault(); e.stopPropagation(); } },
        keypress: (e) => { if (e.key === 'Enter') e.preventDefault(); },
      },
    });
    const open = h('button', {
      class: 'btn btn--ghost btn--sm', type: 'button',
      dataset: { key: keyBase + '-open' },
      title: 'Switch to ' + who.name + "'s Claude window",
      on: { click: () => { if (acct) openAccount(acct.id); } },
      disabled: !acct,
    }, icon('open'), 'Open in Claude');

    const cwdShort = shortPath(a.cwd, 34);
    const li = h('li', {
      class: 'appr',
      dataset: { id: a.id },
      attrs: { 'aria-label': who.name + ': ' + title + ' wants to use ' + tool + (a.mcpServer ? ' from ' + a.mcpServer : '') },
    },
    h('div', { class: 'appr__head' },
      avatarEl(who, 'avatar--xs'),
      h('span', { class: 'appr__acct', text: who.name }),
      h('span', { class: 'spacer' }),
      a.mcpServer ? h('span', { class: 'badge appr__server', text: String(a.mcpServer), title: 'MCP server' }) : null,
      h('span', { class: 'badge badge--warn appr__tool', text: tool, title: 'Tool' })),
    h('div', { class: 'appr__title', text: title, title }),
    cwdShort ? h('div', { class: 'appr__meta', title: a.cwd },
      icon('folder'), h('span', { class: 'appr__cwd', text: cwdShort })) : null,
    code,
    more,
    h('div', { class: 'appr__actions' }, deny, allow, h('span', { class: 'spacer' }), open));

    const card = { li, allow, timer: null, armTimer: null, busy: false };
    const expires = approvalExpiresAt(a);
    if (Number.isFinite(expires)) {
      card.timer = setTimeout(() => dropApproval(a.id), Math.max(0, expires - Date.now()));
    }
    return card;
  }

  /** Remove a card; keyboard focus moves to the next card's Deny (never Allow) or the list. */
  function dropApproval(id) {
    const card = approvalCards.get(id);
    state.approvals = state.approvals.filter((x) => x.id !== id);
    if (card) {
      approvalCards.delete(id);
      clearTimeout(card.timer);
      clearTimeout(card.armTimer);
      const hadFocus = card.li.contains(document.activeElement);
      const next = card.li.nextElementSibling || card.li.previousElementSibling;
      card.li.remove();
      if (hadFocus) {
        const deny = next && next.querySelector('[data-key$="-deny"]');
        (deny || $('view-sessions')).focus({ preventScroll: true });
      }
    }
    syncApprovalChrome();
  }

  async function decide(a, decision) {
    const card = approvalCards.get(a.id);
    if (!card || card.busy) return;
    if (decision === 'allow' && card.allow.classList.contains('is-arming')) return;
    card.busy = true;
    card.li.classList.add('is-busy');
    card.li.querySelectorAll('.appr__actions button').forEach((b) => { b.disabled = true; });
    const res = await call('decideApproval', { id: a.id, decision });
    if (res && res.ok === false && res.error !== 'gone' && res.reason !== 'gone') {
      card.busy = false;
      card.li.classList.remove('is-busy');
      card.li.querySelectorAll('.appr__actions button').forEach((b) => { b.disabled = false; });
      armAllow(card);
      toast('error', "Couldn't send your answer", res.error || 'Answer in Claude instead.');
      return;
    }
    dropApproval(a.id);
    if (res && res.ok === false) {
      toast('info', 'Already answered', 'The request was answered in Claude or it expired.');
    } else {
      toast(decision === 'allow' ? 'success' : 'info', (decision === 'allow' ? 'Allowed ' : 'Denied ') + String(a.toolName || 'the request'), approvalTitle(a));
    }
    pollWaiting();
  }

  /** Count badge on the Sessions tab and the group's visibility. */
  function syncApprovalChrome() {
    const n = approvalCards.size;
    approvalsHost.hidden = n === 0;
    sessionsTabCount.hidden = n === 0;
    sessionsTabCount.textContent = n ? String(n) : '';
    if (n) sessionsTab.setAttribute('aria-label', 'Sessions, ' + n + ' waiting for your approval');
    else sessionsTab.removeAttribute('aria-label');
  }

  /** Apply a listApprovals() reply: keep the cards still listed, add new ones, drop the rest. */
  function setApprovals(list) {
    if (!Array.isArray(list)) return;
    const now = Date.now();
    const valid = list.filter((a) => a && typeof a.id === 'string' && a.id &&
      !(approvalExpiresAt(a) <= now));
    const ids = new Set(valid.map((a) => a.id));
    for (const id of Array.from(approvalCards.keys())) if (!ids.has(id)) dropApproval(id);
    state.approvals = valid;
    const fresh = [];
    for (const a of valid) {
      if (approvalCards.has(a.id)) continue;
      const card = approvalCard(a);
      approvalCards.set(a.id, card);
      approvalList.appendChild(card.li);
      armAllow(card);
      if (!seenApprovals.has(a.id)) { seenApprovals.add(a.id); fresh.push(a); }
    }
    // Keep the cards in the app's order (oldest first); a card already in place is not moved,
    // so focus and a text selection inside it survive the poll.
    let at = approvalList.firstElementChild;
    for (const a of valid) {
      const li = approvalCards.get(a.id).li;
      if (li === at) at = at.nextElementSibling;
      else approvalList.insertBefore(li, at);
    }
    for (const id of Array.from(seenApprovals)) if (!ids.has(id)) seenApprovals.delete(id);
    syncApprovalChrome();
    if (fresh.length) announceApprovals(fresh);
    state.approvalsLoaded = true;
  }

  function announceApprovals(fresh) {
    const a = fresh[0];
    const show = state.tab === 'sessions' ? null : { label: 'Show', onClick: () => showTab('sessions') };
    if (fresh.length === 1) {
      toast('warn', 'Waiting for your approval', (a.accountName || 'Claude') + ' · ' + String(a.toolName || 'a tool') + ' in ' + approvalTitle(a), 8000, show);
    } else {
      toast('warn', fresh.length + ' requests are waiting for your approval', 'Answer them in the Sessions tab.', 8000, show);
    }
  }

  /** Apply a listWaiting() reply; session rows re-render only when something changed. */
  function setWaiting(list) {
    if (!Array.isArray(list)) return;
    state.waiting = list.filter((w) => w && typeof w.sessionId === 'string' && typeof w.accountId === 'string');
    const snapshot = JSON.stringify([state.waiting.map((w) => [w.accountId, w.sessionId, w.kind]),
      state.approvals.map((a) => [a.accountId, a.sessionId])]);
    if (snapshot === state.waitingJson) return;
    state.waitingJson = snapshot;
    refreshSessionRows();
  }

  function refreshSessionRows() {
    if (state.tab !== 'sessions') return;
    if (menu || dialog) { state.sessionsDirty = true; return; }
    state.sessionsDirty = false;
    renderSessions();
  }

  let approvalsInFlight = false;
  async function pollApprovals() {
    if (approvalsInFlight || typeof api.listApprovals !== 'function') return;
    approvalsInFlight = true;
    try {
      const [list, waiting] = await Promise.all([call('listApprovals'), call('listWaiting')]);
      setApprovals(list);
      setWaiting(waiting);
    } finally {
      approvalsInFlight = false;
    }
  }

  async function pollWaiting() {
    setWaiting(await call('listWaiting'));
  }

  /* ======================================================================
   * Settings tab
   * ==================================================================== */

  const settingsBody = $('settingsBody');
  const versionLabel = $('versionLabel');

  function setSettings(s) {
    if (!s || typeof s !== 'object' || s.ok === false) return;
    state.settings = Object.assign({}, state.settings || {}, s);
    applyTheme(state.settings.theme);
    renderSignInNotice();
    renderHotkeyHint();
    if (state.tab === 'settings') renderSettings();
  }

  async function updateSetting(key, value) {
    const prev = state.settings ? state.settings[key] : undefined;
    state.settings = Object.assign({}, state.settings || {}, { [key]: value });
    if (key === 'theme') applyTheme(value);
    renderSettings();
    const res = await call('setSettings', { [key]: value });
    if (!res || res.ok === false) {
      state.settings[key] = prev;
      if (key === 'theme') applyTheme(prev);
      renderSettings();
      toast('error', "Couldn't save that setting", (res && res.error) || '');
      return;
    }
    setSettings(res);
  }

  /**
   * A setting bound to a switch. opts (optional): { onToggle(next), disabled, hint } — onToggle
   * replaces the plain save; a disabled row shows `hint` under its explanation.
   */
  function toggleRow(key, label, desc, opts) {
    const o = opts || {};
    const on = !!(state.settings && state.settings[key]);
    const id = 'set-' + key;
    const sw = h('button', {
      class: 'switch', type: 'button', id,
      dataset: { key: id },
      disabled: !!o.disabled,
      attrs: {
        role: 'switch', 'aria-checked': on ? 'true' : 'false', 'aria-labelledby': id + '-l',
        'aria-describedby': id + '-d' + (o.disabled && o.hint ? ' ' + id + '-h' : ''),
      },
      on: {
        click: (e) => {
          e.stopPropagation();
          if (o.onToggle) o.onToggle(!on); else updateSetting(key, !on);
        },
      },
    });
    return h('div', {
      class: 'setting' + (o.disabled ? ' is-disabled' : ' setting--clickable'),
      on: { click: () => { if (!sw.disabled) sw.click(); } },
    },
    h('span', { class: 'setting__text' },
      h('span', { class: 'setting__label', id: id + '-l', text: label }),
      h('span', { class: 'setting__desc', id: id + '-d', text: desc }),
      o.disabled && o.hint ? h('span', { class: 'setting__hint', id: id + '-h', text: o.hint }) : null),
    sw);
  }

  /* ---------- Waiting sessions, approvals and limits (Claude Code hooks) ---------- */

  const HOOK_NOTE_ALL = "Claude Code's settings turn every hook off (disableAllHooks), so Claude can't tell Sessionmux when a session waits. Remove that setting to use this.";
  const HOOK_NOTE_MANAGED = "Only hooks from your organisation's managed settings may run here (allowManagedHooksOnly), so Sessionmux's hooks will not run.";

  /** Plain-language notes for hook settings that stop our hooks from running. */
  function hookNotes(st) {
    const notes = [];
    if (st && st.disabledAll) notes.push(HOOK_NOTE_ALL);
    if (st && st.managedOnly) notes.push(HOOK_NOTE_MANAGED);
    return notes;
  }

  async function refreshHooks() {
    if (typeof api.getHooksStatus !== 'function') return;
    const st = await call('getHooksStatus');
    if (!st || typeof st !== 'object' || st.ok === false) return;
    const before = JSON.stringify(state.hooks);
    state.hooks = st;
    if (before !== JSON.stringify(st) && state.tab === 'settings') renderSettings();
  }

  /** On: show the exact entries, then back up and edit settings.json. Off: remove our entries. */
  async function setApproveFromPanel(next) {
    if (state.hooksBusy) return;
    state.hooksBusy = true;
    renderSettings();
    try {
      if (next) {
        const [preview, st] = await Promise.all([call('previewHooks'), call('getHooksStatus')]);
        if (st && typeof st === 'object' && st.ok !== false) state.hooks = st;
        const json = preview && typeof preview === 'object' && preview.ok !== false
          ? (typeof preview.json === 'string' ? preview.json : preview.json ? JSON.stringify(preview.json, null, 2) : '')
          : '';
        if (!json) {
          toast('error', "Couldn't prepare the hooks", (preview && preview.error) || 'Nothing was changed.');
          return;
        }
        const file = (typeof preview.path === 'string' && preview.path) || 'your Claude Code settings.json';
        const notes = hookNotes(state.hooks);
        const lines = ['Sessionmux adds these entries to ' + file + '. A copy of the file is kept next to it as a backup first, and turning this off removes exactly these entries again.']
          .concat(notes);
        if (notes.length) lines.push('Until that setting is removed, the panel will not show waiting sessions or approve requests.');
        const r = await confirmDialog({
          title: 'Add Claude Code hooks?', body: lines.join('\n'), code: json,
          okLabel: notes.length ? 'Add hooks anyway' : 'Add hooks', focusCancel: true,
        });
        if (!r.confirmed) return;
        const res = await call('installHooks');
        if (!res || res.ok === false) {
          toast('error', "Couldn't add the hooks", (res && res.error) || 'settings.json was not changed.');
          return;
        }
        await updateSetting('approveFromPanel', true);
        await refreshHooks();
        const after = hookNotes(state.hooks);
        if (after.length) toast('warn', 'Hooks added, but Claude Code will not run them', after.join(' '));
        else toast('success', 'Hooks added', 'Sessions that wait for you now show up in the Sessions tab.');
      } else {
        const res = await call('removeHooks');
        if (!res || res.ok === false) {
          toast('error', "Couldn't remove the hooks", (res && res.error) || 'settings.json was not changed.');
          return;
        }
        await updateSetting('approveFromPanel', false);
        toast('info', 'Hooks removed', "Sessionmux's entries were taken out of settings.json.");
      }
    } finally {
      state.hooksBusy = false;
      await refreshHooks();
      renderSettings();
    }
  }

  function hooksGroup() {
    const st = state.hooks || {};
    const installed = st.installed === true;
    const notes = hookNotes(st);
    return h('div', { class: 'group' },
      h('h3', { class: 'group__title', text: 'Approvals and limits' }),
      toggleRow('approveFromPanel', 'Show waiting sessions and approve from Sessionmux',
        'Adds hooks to your Claude Code settings so a session that waits for you shows up here. Nothing is ever approved without you.',
        { onToggle: setApproveFromPanel, disabled: movedAway() && !installed, hint: MOVE_HINT }),
      notes.length ? h('div', { class: 'setting setting--note', attrs: { role: 'note' } },
        icon('alert', 'setting__note-icon'),
        h('span', { class: 'setting__desc', text: notes.join(' ') })) : null,
      toggleRow('autoHandover', 'Hand sessions over automatically when an account hits its limit',
        'Copies the running session to the next account that is signed in and not limited.',
        { disabled: !installed, hint: 'Turn on the setting above first: the limit signal comes from the same hooks.' }));
  }

  /**
   * A setting bound to a segmented radio group (same look as Theme).
   * o: { key, label, desc?, options: [[value, text, icon]], fallback, keyPrefix, wrap? }
   */
  function segmentedRow(o) {
    const raw = state.settings && state.settings[o.key];
    const cur = o.options.some(([v]) => v === raw) ? raw : o.fallback;
    const id = 'set-' + o.key;
    const group = h('div', { class: 'segmented', attrs: { role: 'radiogroup', 'aria-labelledby': id + '-l' } });
    const btns = o.options.map(([val, text, ic]) => h('button', {
      class: 'segmented__opt' + (val === cur ? ' is-active' : ''), type: 'button', tabIndex: val === cur ? 0 : -1,
      dataset: { key: o.keyPrefix + val, val },
      attrs: { role: 'radio', 'aria-checked': val === cur ? 'true' : 'false' },
      on: { click: () => { if (val !== cur) updateSetting(o.key, val); } },
    }, icon(ic), text));
    group.append(...btns);
    group.addEventListener('keydown', (e) => {
      const i = btns.indexOf(document.activeElement);
      if (i < 0) return;
      let n = -1;
      if (e.key === 'ArrowRight') n = (i + 1) % btns.length;
      if (e.key === 'ArrowLeft') n = (i - 1 + btns.length) % btns.length;
      if (n >= 0) {
        e.preventDefault();
        const val = btns[n].dataset.val;
        updateSetting(o.key, val); // re-renders synchronously
        const again = settingsBody.querySelector('[data-key="' + o.keyPrefix + val + '"]');
        if (again) again.focus();
      }
    });
    const text = h('span', { class: 'setting__text' }, h('span', { class: 'setting__label', id: id + '-l', text: o.label }));
    if (o.desc) {
      text.appendChild(h('span', { class: 'setting__desc', id: id + '-d', text: o.desc }));
      group.setAttribute('aria-describedby', id + '-d');
    }
    return h('div', { class: 'setting' + (o.wrap ? ' setting--wrap' : '') }, text, group);
  }

  function themeRow() {
    return segmentedRow({
      key: 'theme', label: 'Theme', fallback: 'system', keyPrefix: 'theme-',
      options: [['system', 'System', 'monitor'], ['light', 'Light', 'sun'], ['dark', 'Dark', 'moon']],
    });
  }

  /** Floating button shape — single icon or mini tray of the first 3 accounts. */
  function bubbleModeRow() {
    return segmentedRow({
      key: 'bubbleMode', label: 'Floating button', fallback: 'icon', keyPrefix: 'bubble-mode-', wrap: true,
      desc: 'Mini tray shows your first 3 accounts — reorder them in Accounts.',
      options: [['icon', 'Icon', 'bubble'], ['tray', 'Mini tray', 'tray']],
    });
  }

  function hotkeyRow() {
    const mac = onMac();
    const parts = hotkeyParts((state.settings && state.settings.hotkey) || 'Ctrl+Shift+Space', mac);
    const spoken = mac ? parts.map((p) => MAC_KEY_NAMES[p] || p).join(' ') : parts.join(' plus ');
    return h('div', { class: 'setting' },
      h('span', { class: 'setting__text' },
        h('span', { class: 'setting__label', text: 'Show / hide panel' }),
        h('span', { class: 'setting__desc', text: mac ? 'Works from anywhere on your Mac.' : 'Works from anywhere in Windows.' })),
      h('span', { class: 'kbd-group', attrs: { 'aria-label': 'Shortcut ' + spoken } },
        parts.map((p) => h('kbd', { text: p }))));
  }

  /** The quiet show/hide shortcut line under the Accounts footer buttons; hidden without a shortcut. */
  function renderHotkeyHint() {
    const hint = $('hotkeyHint');
    if (!hint) return;
    const mac = onMac();
    const parts = hotkeyParts(state.settings && state.settings.hotkey, mac);
    hint.textContent = '';
    hint.hidden = parts.length === 0;
    if (!parts.length) return;
    const spoken = mac ? parts.map((p) => MAC_KEY_NAMES[p] || p).join(' ') : parts.join(' plus ');
    hint.append(
      h('span', { text: 'Show or hide panel' }),
      h('span', { class: 'footer__hint-spoken', text: ': ' + spoken }),
      h('span', { class: 'kbd-group', attrs: { 'aria-hidden': 'true' } },
        parts.map((p) => h('kbd', { text: p }))));
  }

  /** A warning line in a settings card, with an optional button under the text. */
  function noteRow(text, action) {
    return h('div', { class: 'setting setting--note', attrs: { role: 'note' } },
      icon('alert', 'setting__note-icon'),
      h('span', { class: 'setting__text' },
        h('span', { class: 'setting__desc', text }),
        action ? h('button', {
          class: 'btn btn--secondary btn--sm', type: 'button', text: action.label,
          dataset: { key: action.key }, on: { click: action.onClick },
        }) : null));
  }

  /** Settings → "Give sign-in links back to Claude": one restore of the claude:// handler. */
  async function giveLinksBack() {
    const res = await call('restoreLinkHandler');
    if (!res || res.ok === false) {
      toast('error', "Couldn't give the sign-in links back", (res && res.error) || '');
    } else if (res.linkHandler === 'pending') {
      toast('info', 'macOS is asking you to confirm', 'Choose Use when macOS asks whether Claude may open Claude sign-in links.', 9000);
    } else if (res.linkHandler === 'declined') {
      toast('warn', 'Sign-in links still open through Sessionmux', 'macOS kept Sessionmux for them. Try again and choose Use for Claude.', 9000);
    } else {
      toast('success', 'Sign-in links open in Claude again');
    }
  }

  /** Settings → "Remove Sessionmux's changes": hooks, link handler and login item, after a confirm. */
  async function removeChanges() {
    if (state.hooksBusy) return;
    const r = await confirmDialog({
      title: "Remove Sessionmux's changes?",
      body: [
        'Sessionmux takes out what it changed outside its own folder:',
        '• its hooks in your Claude Code settings',
        '• the claude:// link handler, which goes back to Claude',
        '• the login item',
        'Your accounts and their data folders stay as they are. To remove Sessionmux completely, quit it afterwards and move it to the Trash.',
      ].join('\n'),
      okLabel: 'Remove changes',
      danger: true,
    });
    if (!r.confirmed) return;
    state.hooksBusy = true;
    renderSettings();
    try {
      const res = await call('removeChanges');
      if (!res || res.ok === false) {
        toast('error', "Couldn't remove all of Sessionmux's changes", (res && res.error) || '');
        return;
      }
      if (state.settings && state.settings.approveFromPanel) await updateSetting('approveFromPanel', false);
      if (res.linkHandler === 'pending') {
        toast('info', "Sessionmux's changes were removed", 'macOS may ask whether Claude may open Claude sign-in links; choose Use.', 9000);
      } else if (res.linkHandler === 'declined' || res.linkHandler === 'failed') {
        toast('warn', 'Hooks and login item removed', "Sign-in links still open through Sessionmux. Use 'Give sign-in links back to Claude' in Settings to try again.", 9000);
      } else {
        toast('success', "Sessionmux's changes were removed", 'You can quit Sessionmux and move it to the Trash now.');
      }
    } finally {
      state.hooksBusy = false;
      await refreshHooks();
      renderSettings();
    }
  }

  /** macOS only: the card that undoes everything before the app is moved to the Trash. */
  function removeGroup() {
    return h('div', { class: 'group' },
      h('h3', { class: 'group__title', text: 'Remove Sessionmux' }),
      h('div', { class: 'setting' },
        h('span', { class: 'setting__text' },
          h('span', { class: 'setting__label', text: "Remove Sessionmux's changes" }),
          h('span', { class: 'setting__desc', text: 'Takes out its hooks, the sign-in link handler and the login item. Then quit Sessionmux and move it to the Trash.' })),
        h('button', {
          class: 'btn btn--danger-ghost btn--sm', type: 'button', text: 'Remove',
          disabled: !!state.hooksBusy, dataset: { key: 'remove-changes' },
          attrs: { 'aria-label': "Remove Sessionmux's changes" },
          on: { click: removeChanges },
        })));
  }

  function pathRow(label, path, key) {
    return h('div', { class: 'path-row' },
      h('span', { class: 'path-row__text' },
        h('span', { class: 'path-row__label', text: label }),
        h('span', { class: 'path-row__path', text: path, title: path })),
      h('button', {
        class: 'icon-btn icon-btn--sm', type: 'button', title: 'Copy path',
        dataset: { key: 'path-' + key },
        attrs: { 'aria-label': 'Copy path for ' + label },
        on: {
          click: async () => {
            try {
              await navigator.clipboard.writeText(path);
              toast('success', 'Path copied');
            } catch (_) {
              toast('error', "Couldn't copy to the clipboard");
            }
          },
        },
      }, icon('copy')));
  }

  /** Settings → About (last card): version, links, the license attribution and the disclaimer. */
  function aboutGroup() {
    const info = state.appInfo || {};
    const name = info.productName || 'Sessionmux';
    const version = info.version || (state.settings && state.settings.appVersion) || '';
    const built = info.buildDate || '';
    const licenseName = info.licenseName || 'CPAL-1.0';
    const home = urlOf('homeUrl', REPO_URL);
    const sponsor = urlOf('sponsorUrl', '');
    const disclaimer = info.disclaimer || '';
    const link = (text, key, onClick) => h('button', {
      class: 'link-btn', type: 'button', text, dataset: { key: 'about-' + key }, on: { click: onClick },
    });
    const summary = [
      name + (version ? ' ' + version : ''),
      built ? 'built ' + built : '',
      'free and open source (' + licenseName + ')',
    ].filter(Boolean).join(' · ');
    return h('div', { class: 'group', attrs: { role: 'region', 'aria-labelledby': 'aboutTitle' } },
      h('h3', { class: 'group__title', id: 'aboutTitle', text: 'About' }),
      h('div', { class: 'setting about' },
        h('span', { class: 'setting__label', text: summary }),
        h('div', { class: 'about__links' },
          link('Website', 'home', () => call('openExternal', home)),
          link('Report a problem', 'support', () => call('openExternal', urlOf('supportUrl', REPO_URL + '/issues'))),
          link('License', 'license', () => call('openLicense')),
          sponsor ? link('Support the project', 'sponsor', () => call('openExternal', sponsor)) : null),
        // Attribution required by the license (CPAL-1.0 Exhibit B): keep it visible in every build.
        h('p', { class: 'about__attribution' },
          ATTRIBUTION + ' · ',
          h('button', {
            class: 'link-btn link-btn--inline', type: 'button', text: home,
            dataset: { key: 'about-attribution' }, on: { click: () => call('openExternal', home) },
          })),
        disclaimer ? h('p', { class: 'about__disclaimer', text: disclaimer }) : null));
  }

  function renderSettings() {
    const s = state.settings;
    const version = (s && (s.appVersion || s.version)) || '';
    versionLabel.textContent = version ? 'Sessionmux ' + version : 'Sessionmux';
    withFocusKept(() => {
      settingsBody.textContent = '';
      if (!s) {
        for (let i = 0; i < 4; i++) {
          settingsBody.appendChild(h('div', { class: 'setting', attrs: { 'aria-hidden': 'true' } },
            h('span', { class: 'sk-lines' }, h('span', { class: 'sk sk--line' }), h('span', { class: 'sk sk--line sk--short' }))));
        }
        return;
      }
      // Optional paths the app may include in the settings; fall back to the documented defaults.
      const p = (s.paths && typeof s.paths === 'object') ? s.paths : {};
      const accountDirs = state.accounts.map((a) => pathRow(
        a.name,
        a.dataDir || p.mainDataDir || 'Claude Desktop’s default profile',
        'acct-' + a.id));

      const mac = onMac();
      const moved = movedAway();
      const si = state.signIn || {};
      // The switches that write our path somewhere stay off while the app runs from a moved copy.
      const pathGuard = (key) => (moved && !s[key] ? { disabled: true, hint: MOVE_HINT } : null);
      const routingUnavailable = mac && typeof s.signInRoutingUnavailable === 'string' && s.signInRoutingUnavailable && !s.signInRouting
        ? { disabled: true, hint: s.signInRoutingUnavailable } : null;
      const routingDesc = mac && si.needsConsent
        ? 'macOS will ask two or three times during each sign-in whether Sessionmux or Claude should open Claude sign-in links. Choose Use when Sessionmux asks at the start, and Use when Claude asks at the end.'
        : 'While an account is signing in, Sessionmux temporarily becomes the handler for claude:// links and restores Claude as the handler afterwards.';

      settingsBody.append(
        h('div', { class: 'group' },
          h('h3', { class: 'group__title', text: 'General' }),
          mac
            ? toggleRow('startWithWindows', 'Open at login', 'Launch Sessionmux quietly when you log in.', pathGuard('startWithWindows'))
            : toggleRow('startWithWindows', 'Start with Windows', 'Launch Sessionmux quietly when you sign in.'),
          mac && s.startWithWindows && s.autostartStatus === 'requiresApproval'
            ? noteRow('Allow Sessionmux in System Settings > General > Login Items.',
              { label: 'Open Login Items', key: 'open-login-items', onClick: () => call('openSystemSettings', 'loginItems') })
            : null,
          toggleRow('showBubble', 'Show floating bubble', 'A small always-on-top button for quick switching.'),
          toggleRow('shareMcpConfig', 'Share MCP servers from Main', "Copy Main's MCP server config into the other accounts when it changes."),
          mac
            ? toggleRow('raiseWindowsWithAccessibility', 'Bring minimised Claude windows to the front',
              'When switching to an account leaves its window minimised or in another Space, Sessionmux uses Accessibility to bring it forward. It also lets a click on the account in front minimise its window. macOS asks for it again after each update.')
            : null,
          mac && s.raiseWindowsWithAccessibility && s.accessibilityTrusted === false
            ? noteRow('Allow Sessionmux in System Settings > Privacy & Security > Accessibility.',
              { label: 'Open Accessibility', key: 'open-accessibility', onClick: () => call('openSystemSettings', 'accessibility') })
            : null),
        h('div', { class: 'group' },
          h('h3', { class: 'group__title', text: 'Sign-in' }),
          toggleRow('signInRouting', 'Route sign-in links to the account being signed in', routingDesc,
            (mac && (pathGuard('signInRouting') || routingUnavailable)) || undefined),
          si.restoreDeclined
            ? noteRow('Sign-in links open through Sessionmux, which passes them to your Main account.',
              { label: 'Give sign-in links back to Claude', key: 'restore-links', onClick: giveLinksBack })
            : null),
        hooksGroup(),
        h('div', { class: 'group' },
          h('h3', { class: 'group__title', text: 'Appearance' }),
          themeRow(),
          bubbleModeRow()),
        h('div', { class: 'group' },
          h('h3', { class: 'group__title', text: 'Keyboard' }),
          hotkeyRow()),
        h('div', { class: 'group' },
          h('h3', { class: 'group__title', text: 'Where Sessionmux keeps data' }),
          pathRow('Sessionmux settings', p.store || (mac ? '~/Library/Application Support/Sessionmux/sessionmux.json' : '%APPDATA%\\Sessionmux\\sessionmux.json'), 'store'),
          pathRow('Log file', p.log || (mac ? '~/Library/Application Support/Sessionmux/sessionmux.log' : '%APPDATA%\\Sessionmux\\sessionmux.log'), 'log'),
          pathRow('Code transcripts (shared)', p.transcripts || (mac ? '~/.claude/projects' : '%USERPROFILE%\\.claude\\projects'), 'transcripts'),
          accountDirs),
        ...(mac ? [removeGroup()] : []),
        aboutGroup());
    });
  }

  $('btnQuit').addEventListener('click', () => call('quit'));

  /* ======================================================================
   * Global keys, live updates, startup
   * ==================================================================== */

  document.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape' || e.defaultPrevented) return;
    if (dialog) { finishDialog(false); return; }
    if (menu) { closeMenu(true); return; }
    if (state.adding && addHost.contains(document.activeElement)) { closeAddForm(); btnAdd.focus(); return; }
    call('hidePanel');
  });

  // Ctrl+1/2/3 jump between tabs (⌘1/2/3 on macOS); Ctrl+F (⌘F) searches the sessions.
  document.addEventListener('keydown', (e) => {
    const mod = onMac() ? e.metaKey && !e.ctrlKey : e.ctrlKey;
    if (mod && !e.altKey && !e.shiftKey && /^[123]$/.test(e.key)) {
      e.preventDefault();
      showTab(TABS[Number(e.key) - 1], { focus: true });
    } else if (mod && e.key.toLowerCase() === 'f' && state.tab === 'sessions') {
      e.preventDefault();
      searchInput.focus();
      searchInput.select();
    }
  });

  // Keep relative times fresh without re-rendering rows.
  setInterval(() => {
    document.querySelectorAll('time[data-ts]').forEach((t) => {
      const txt = relativeTime(t.dataset.ts);
      if (txt && t.textContent !== txt) t.textContent = txt;
    });
  }, 30 * 1000);

  // The panel window is reused (hidden/shown); refresh sessions when it regains focus.
  let lastFocusRefresh = 0;
  window.addEventListener('focus', () => {
    const now = Date.now();
    if (now - lastFocusRefresh < 2000) return;
    lastFocusRefresh = now;
    if (state.tab === 'sessions' && state.sessionsAccountId) loadSessions(state.sessionsAccountId, { quiet: true });
  });

  // The app may hide the panel on blur while a dialog/menu is up; drop overlays when hidden.
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) {
      closeMenu(false);
      if (dialog) finishDialog(false);
    } else {
      // Shown again: approval cards arm afresh and the lists catch up at once.
      armAllCards();
      pollApprovals();
    }
  });

  // Approvals and waiting sessions: every 2 s while the panel is shown, every 6 s while hidden.
  let lastApprovalPoll = 0;
  setInterval(() => {
    const now = Date.now();
    if (document.hidden && now - lastApprovalPoll < 6000) return;
    lastApprovalPoll = now;
    pollApprovals();
  }, 2000);

  function subscribe(method, cb) {
    if (typeof api[method] !== 'function') return;
    try { api[method](cb); } catch (_) { /* ignore */ }
  }

  subscribe('onAccountsChanged', (list) => setAccounts(list));
  subscribe('onDiscoveredChanged', (list) => setDiscovered(list));
  subscribe('onSettings', (s) => setSettings(s));
  subscribe('onSignInChanged', (si) => setSignIn(si));
  subscribe('onHandover', (res) => handoverDone(res));
  subscribe('onOpenFailed', (ev) => {
    if (!ev) return;
    const a = ev.id ? accountById(ev.id) : null;
    openFailedToast((a && a.name) || ev.name || 'this account', ev);
  });
  subscribe('onActiveAccount', (info) => {
    if (!info || !info.id || info.id === state.activeId) return;
    state.activeId = info.id;
    // Cheap in-place update so an open form/menu is not disturbed.
    accountList.querySelectorAll('.acct[data-id]').forEach((li) => {
      li.classList.toggle('acct--active', li.dataset.id === info.id);
    });
    state.accountsJson = '';
  });

  async function boot() {
    tabButtons.forEach((b) => b.classList.toggle('is-active', b.dataset.tab === state.tab));
    renderSettings(); // skeleton until settings arrive
    const [list, settings, info, signIn] = await Promise.all([
      call('listAccounts'), call('getSettings'), call('getAppInfo'), call('getSignIn'),
    ]);
    if (info && typeof info === 'object' && info.ok !== false) state.appInfo = info;
    renderAppNotice();
    setSettings(settings);
    if (signIn && !signIn.ended) setSignIn(signIn);
    if (Array.isArray(list)) {
      setAccounts(list);
    } else {
      state.accountsLoaded = true;
      accountList.setAttribute('aria-busy', 'false');
      accountList.textContent = '';
      accountList.appendChild(h('li', null, emptyState('alert', "Couldn't load accounts",
        (list && list.error) || 'Sessionmux could not read its account list.',
        { label: 'Try again', onClick: () => refreshAccounts() })));
    }
    refreshDiscovered();
    pollApprovals();
    refreshHooks();
  }

  boot();
}
