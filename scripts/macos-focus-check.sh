#!/usr/bin/env bash
# Copyright (c) 2026 joyal-droid. SPDX-License-Identifier: CPAL-1.0
#
# The contents of this file are subject to the Common Public Attribution License Version 1.0
# (the "License"); you may not use this file except in compliance with the License. You may
# obtain a copy of the License in the LICENSE file at the root of this repository or at
# https://opensource.org/license/cpal-1-0. The License is based on the Mozilla Public License
# Version 1.1 but Sections 14 and 15 have been added to cover use of software over a computer
# network and provide for limited attribution for the Original Developer. In addition, Exhibit A
# has been modified to be consistent with Exhibit B.
#
# Software distributed under the License is distributed on an "AS IS" basis, WITHOUT WARRANTY OF
# ANY KIND, either express or implied. See the License for the specific language governing rights
# and limitations under the License.
#
# The Original Code is Sessionmux. The Initial Developer of the Original Code is joyal-droid. All
# portions of the code written by joyal-droid are Copyright (c) 2026 joyal-droid. All Rights Reserved.
# Contributor(s): see the git history of this repository.
#
# Attribution (Exhibit B of the License): "Powered by Sessionmux", Copyright (c) 2026 joyal-droid,
# https://github.com/joyal-droid/sessionmux and the Sessionmux mark must stay visible in the user
# interface of every copy and derivative; see docs/attribution.md.

# Checks on this Mac that clicking an account in Sessionmux brings that account's Claude window
# forward, with the real Claude Desktop and real mouse clicks, that a click on the account whose
# window is in front puts that window away and the next click brings it back, and that a click
# that cannot bring a window forward shows why in the panel.
#
# Usage: scripts/macos-focus-check.sh <out-dir>
#
# Needs Sessionmux.app in /Applications (SESSIONMUX_APP to use another copy), Claude Desktop in
# /Applications (CLAUDE_APP), a logged-in desktop, cc, python3 and System Events scripting. It
# runs on GitHub Actions only, unless SESSIONMUX_FOCUS_ALLOW=1 is set: it replaces Sessionmux's
# settings file, starts and stops Claude, and records an Automation permission.
#
# What it does, in order:
#   - writes a Sessionmux settings file with two accounts: Main (Claude's default data folder)
#     and Second (a data folder in a temporary folder);
#   - starts Claude for both accounts and waits for their windows;
#   - starts Sessionmux once as a user would (`open -a`), checks that its log file gets the
#     startup line, then stops it;
#   - records "Sessionmux may control Claude" in the user's Automation permissions, as a user
#     answering the system's question once would, and Sessionmux in the Accessibility list
#     (best effort; the outcomes are reported);
#   - runs `Sessionmux --focus-check` (see src/focuscheck.rs) through Launch Services and, for
#     each step, arranges the windows with System Events, posts a real mouse click with
#     tests/fixtures/macos/postclick.c at the point Sessionmux names, and takes a screenshot:
#       S1  panel row, Second's window open behind TextEdit            must come forward
#       S2  panel row, Second's window closed with Cmd-W               must reopen and come forward
#       S3  panel row, Second's window minimised, raise setting off    recorded
#           (the settings file leaves "Bring minimised Claude windows to the front" off)
#       S4  floating button avatar, Second's window closed with Cmd-W  must reopen and come forward
#       S5  panel row, Second's window closed and Claude stopped      must fail visibly in the panel
#       S6  opened from posted work, with no input event               recorded
#       S7  panel row, Second's window closed with Cmd-W, raise setting   must reopen and come forward
#           on (Accessibility recorded for Sessionmux when the runner lets it)
#       S8  panel row, Second's window open and Second in front,      must be minimised: no window
#           raise setting on                                           of it on screen, no message
#                                                                      in the panel
#       S9  panel row, Second's window minimised as S8 left it, raise  must come back in front
#           setting still on (un-minimised through Accessibility)
#
# Leaves in <out-dir>: focus-check.json, sessionmux.log, startup.txt, script.log, summary.txt
# and one screenshot per step. Prints one GitHub notice per step. Exits 0 when the startup log
# check and the steps S1, S2, S4, S5, S7, S8 and S9 passed, 1 otherwise. A step that could not
# prove anything (its click never reached Sessionmux, or the windows could not be arranged) is
# reported as unproven and fails the run too, unless SESSIONMUX_FOCUS_ALLOW_UNPROVEN=1 is set.

set -uo pipefail

if [ "${GITHUB_ACTIONS:-}" != "true" ] && [ "${SESSIONMUX_FOCUS_ALLOW:-}" != "1" ]; then
    echo "This check replaces Sessionmux's settings and starts and stops Claude." >&2
    echo "It runs on GitHub Actions only; set SESSIONMUX_FOCUS_ALLOW=1 to run it anyway." >&2
    exit 2
fi
if [ "$(uname -s)" != "Darwin" ]; then
    echo "This check runs on macOS only." >&2
    exit 2
fi

OUT="${1:?usage: macos-focus-check.sh <out-dir>}"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="${SESSIONMUX_APP:-/Applications/Sessionmux.app}"
APP_EXE="$APP/Contents/MacOS/Sessionmux"
BUNDLE_ID="io.github.joyal-droid.sessionmux"
CLAUDE_APP="${CLAUDE_APP:-/Applications/Claude.app}"
CLAUDE_EXE="$CLAUDE_APP/Contents/MacOS/Claude"
CLAUDE_ID="com.anthropic.claudefordesktop"
SUPPORT="$HOME/Library/Application Support/Sessionmux"
LOG_FILE="$SUPPORT/sessionmux.log"
WORK="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/sessionmux-focus"
SYNC="$WORK/sync"
SECOND_DIR="$WORK/second"
SECOND_ID="second-account"
RUNNER_LABEL="${FOCUS_RUNNER:-macOS $(sw_vers -productVersion 2>/dev/null)}"
SCRIPT_LOG="$OUT/script.log"
SUMMARY="$OUT/summary.txt"
# S7 turns the raise setting on for the rest of the run and S5 leaves Second's Claude slow to
# answer after it is resumed, so S7 runs before S5 and S5 runs last. S9 starts from what S8 left.
# S8 and S9 keep the raise setting on, as a user who has the click minimise a window has it: S8
# minimises Second's window through Accessibility and S9 un-minimises it.
STEPS="S1 S2 S3 S4 S6 S7 S8 S9 S5"
STOPPED_PID=""

rm -rf "$WORK"
mkdir -p "$WORK" "$SYNC" "$SECOND_DIR"
: >"$SCRIPT_LOG"
: >"$SUMMARY"

log() { echo "focus-check: $*" | tee -a "$SCRIPT_LOG" >&2; }

# ---------------------------------------------------------------- Claude processes and windows

# Claude main processes (no --type= argument): "<pid> <arguments>".
claude_lines() {
    ps -axww -o pid=,command= | awk -v exe="$CLAUDE_EXE" '
        { pid = $1; $1 = ""; sub(/^ /, "") }
        index($0, exe) == 1 && index($0, "--type=") == 0 { print pid " " $0 }'
}
claude_mains() { claude_lines | awk '{ print $1 }'; }
main_pid() { claude_lines | awk 'index($0, "--user-data-dir=") == 0 { print $1; exit }'; }
second_pid() { claude_lines | awk -v udd="--user-data-dir=$SECOND_DIR" 'index($0, udd) > 0 { print $1; exit }'; }

cleanup() {
    status=$?
    trap - EXIT
    [ -n "$STOPPED_PID" ] && kill -CONT "$STOPPED_PID" 2>/dev/null
    pkill -x Sessionmux 2>/dev/null
    for pid in $(claude_mains); do kill -TERM "$pid" 2>/dev/null; done
    exit "$status"
}
trap cleanup EXIT

# System Events, with its error text on failure.
sysevents() { osascript -e 'tell application "System Events"' "$@" -e 'end tell' 2>&1; }

# Windows System Events lists for a process (minimised ones included).
window_count() { sysevents -e "count windows of (first process whose unix id is $1)"; }

# "yes" when a process has a window that is not minimised.
# A window whose minimised state cannot be read is skipped.
open_window() {
    sysevents -e "set p to first process whose unix id is $1" \
        -e 'repeat with w in windows of p' \
        -e 'try' -e 'if value of attribute "AXMinimized" of w is false then return "yes"' -e 'end try' \
        -e 'end repeat' -e 'return "no"'
}

has_open_window() { [ "$(open_window "$1")" = "yes" ]; }

# Each window's minimised state as System Events reads it, for the notes.
window_info() {
    sysevents -e "set p to first process whose unix id is $1" -e 'set out to ""' \
        -e 'repeat with w in windows of p' \
        -e 'try' -e 'set m to (value of attribute "AXMinimized" of w) as text' \
        -e 'on error e' -e 'set m to "unreadable: " & e' -e 'end try' \
        -e 'set out to out & "[" & m & "]"' -e 'end repeat' -e 'return out'
}

# "yes" when System Events lists at least one window for a process.
any_window() { [ "$(window_count "$1")" -ge 1 ] 2>/dev/null; }

wait_for() { # seconds command...
    local end=$((SECONDS + $1))
    shift
    while [ "$SECONDS" -lt "$end" ]; do
        "$@" && return 0
        sleep 1
    done
    return 1
}

# TextEdit in front, with a document of its own.
textedit_front() {
    [ -f "$WORK/front.txt" ] || printf 'Another app in front.\n' >"$WORK/front.txt"
    open -a TextEdit "$WORK/front.txt" >>"$SCRIPT_LOG" 2>&1
    sleep 2
}

# Close the front window of a process with Cmd-W (with its close button when Cmd-W left a window
# listed). A process already in front is not activated again, which can put another instance of
# the same app in front instead.
close_window() {
    local out
    if [ "$(front_pid)" = "$1" ]; then
        out="$(sysevents -e 'keystroke "w" using command down')"
    else
        out="$(sysevents -e "set frontmost of (first process whose unix id is $1) to true" -e 'delay 1' \
            -e 'keystroke "w" using command down')"
    fi
    sleep 2
    if any_window "$1"; then
        out="$out; $(sysevents -e "set p to first process whose unix id is $1" \
            -e 'click (first button of window 1 of p whose subrole is "AXCloseButton")')"
        sleep 2
    fi
    if any_window "$1"; then
        echo "error: the window is still open ($out)"
    else
        echo "closed (windows left: $(window_count "$1"))"
    fi
}

minimise_window() {
    local out
    out="$(sysevents -e "set p to first process whose unix id is $1" \
        -e 'set value of attribute "AXMinimized" of window 1 of p to true')"
    sleep 2
    if has_open_window "$1"; then
        echo "error: the window is still open ($out)"
    else
        echo "minimised (windows: $(window_info "$1"))"
    fi
}

restore_windows() {
    sysevents -e "set p to first process whose unix id is $1" \
        -e 'repeat with w in windows of p' -e 'set value of attribute "AXMinimized" of w to false' \
        -e 'end repeat' >/dev/null
    sleep 1
}

# Show a process again when it is hidden; a visible one is left as it is.
show_process() {
    sysevents -e "set p to first process whose unix id is $1" \
        -e 'if not (visible of p) then set visible of p to true' >/dev/null
}

# Put a process in front by its own pid (System Events may pick another instance of the same app).
bring_front() {
    show_process "$1"
    osascript -l JavaScript -e "ObjC.import('AppKit');
        var a = \$.NSRunningApplication.runningApplicationWithProcessIdentifier($1);
        a.isNil() ? 'no such app' : (a.activateWithOptions(1) ? 'activated' : 'refused')" 2>&1
    sleep 2
}

# Click the title bar of a process's first window, so that window takes the keyboard (after a
# click on the panel, the panel can keep it while another app is in front).
click_window() {
    local at
    at="$(sysevents -e "set p to first process whose unix id is $1" \
        -e 'set {x, y} to position of window 1 of p' -e 'set {w, h} to size of window 1 of p' \
        -e 'return ((x + w div 2) as text) & " " & ((y + 12) as text)')"
    case "$at" in
        [0-9]*" "[0-9]*) [ -x "$WORK/postclick" ] && "$WORK/postclick" $at >>"$SCRIPT_LOG" 2>&1 ;;
        *) echo "click_window $1: $at" >>"$SCRIPT_LOG" ;;
    esac
    sleep 1
}

# The unix id of the app in front, as System Events sees it.
front_pid() { sysevents -e 'unix id of first process whose frontmost is true'; }

# ---------------------------------------------------------------- hand-shake with Sessionmux

write_atomic() { # file text
    printf '%s\n' "$2" >"$1.tmp" && mv "$1.tmp" "$1"
}

json_note() { python3 -c 'import json, sys; print(json.dumps({sys.argv[1]: sys.argv[2]}))' "$1" "$2"; }

# Arrange the windows for a step; prints "note: ..." or "error: ...".
setup_step() {
    local second r
    second="$(second_pid)"
    if [ -z "$second" ]; then
        echo "error: Second's Claude is not running"
        return
    fi
    case "$1" in
        S1 | S6)
            restore_windows "$second"
            if has_open_window "$second"; then
                textedit_front
                echo "note: window open, TextEdit in front"
            elif any_window "$second"; then
                textedit_front
                echo "note: window listed but its state is unclear ($(window_info "$second")), TextEdit in front"
            else
                echo "error: Second's Claude has no open window ($(open_window "$second"); windows: $(window_count "$second"))"
            fi
            ;;
        S2 | S4 | S7)
            restore_windows "$second"
            r="$(close_window "$second")"
            textedit_front
            case "$r" in error*) echo "$r" ;; *) echo "note: $r with Cmd-W, TextEdit in front" ;; esac
            ;;
        S3)
            restore_windows "$second"
            if ! has_open_window "$second" && ! any_window "$second"; then
                echo "error: Second's Claude has no window to minimise ($(open_window "$second"); windows: $(window_count "$second"))"
                return
            fi
            r="$(minimise_window "$second")"
            textedit_front
            case "$r" in error*) echo "$r" ;; *) echo "note: $r, TextEdit in front" ;; esac
            ;;
        S8)
            # S7 leaves Second's window open and in front; it is brought forward only if not.
            has_open_window "$second" || restore_windows "$second"
            r=""
            [ "$(front_pid)" = "$second" ] || r="$(bring_front "$second")"
            if ! has_open_window "$second" && ! any_window "$second"; then
                echo "error: Second's Claude has no open window ($(open_window "$second"); windows: $(window_count "$second"))"
            elif [ "$(front_pid)" != "$second" ]; then
                echo "error: Second's Claude did not come to the front (front pid $(front_pid)${r:+, $r})"
            else
                echo "note: window open, Second in front"
            fi
            ;;
        S9)
            echo "note: windows as S8 left them (front pid $(front_pid); windows: $(window_info "$second"))"
            ;;
        S5)
            # Second may still be hidden when S9 failed; it is shown again before its window is closed.
            show_process "$second"
            # A stopped app's visible window is still brought forward by the window server, so
            # the window is closed first: a stopped Claude cannot show it again.
            restore_windows "$second"
            # S8 and S9 leave the panel holding the keyboard, which would take the Cmd-W.
            click_window "$second"
            r="$(close_window "$second")"
            textedit_front
            case "$r" in error*) echo "$r"; return ;; esac
            if kill -STOP "$second" 2>/dev/null; then
                STOPPED_PID="$second"
                echo "note: $r with Cmd-W, pid $second stopped, TextEdit in front"
            else
                echo "error: pid $second could not be stopped"
            fi
            ;;
    esac
}

after_step() {
    if [ -n "$STOPPED_PID" ]; then
        kill -CONT "$STOPPED_PID" 2>/dev/null
        STOPPED_PID=""
        sleep 2
    fi
}

# Post the click Sessionmux asks for in <step>.target.json and report it in <step>.clicked.
click_step() {
    local step=$1 target="$SYNC/$1.target.json" kind x y result
    if ! wait_for 90 test -f "$target"; then
        log "$step: no click target"
        return
    fi
    read -r kind x y < <(python3 -c 'import json, sys
d = json.load(open(sys.argv[1]))
print(d.get("kind", "none"), d.get("x", ""), d.get("y", ""))' "$target")
    if [ "$kind" = "none" ]; then
        log "$step: nothing to click ($(cat "$target"))"
        return
    fi
    if [ -x "$WORK/postclick" ]; then
        result="$("$WORK/postclick" "$x" "$y" 2>&1)"
        case "$result" in
            "{"*) ;;
            *) result="$(json_note detail "$result" | sed 's/^{/{"posted": false, /')" ;;
        esac
    else
        result='{"posted": false, "detail": "the click helper could not be built"}'
    fi
    log "$step: click on the $kind at $x,$y: $result"
    write_atomic "$SYNC/$step.clicked" "$result"
}

# ---------------------------------------------------------------- 1. settings file

log "$RUNNER_LABEL, $(uname -m)"
log "Claude $(defaults read "$CLAUDE_APP/Contents/Info.plist" CFBundleShortVersionString 2>/dev/null || echo 'not found')"
if [ ! -x "$APP_EXE" ] || [ ! -x "$CLAUDE_EXE" ]; then
    log "Sessionmux or Claude is not installed"
    exit 1
fi
mkdir -p "$SUPPORT"
cat >"$SUPPORT/sessionmux.json" <<JSON
{
  "version": 2,
  "accounts": [
    { "id": "main", "name": "Main", "dataDir": null, "color": "#5B9CF6", "label": null, "order": 0 },
    { "id": "$SECOND_ID", "name": "Second", "dataDir": "$SECOND_DIR", "color": "#F08A5D", "label": null, "order": 1 }
  ],
  "settings": {
    "startWithWindows": false, "shareMcpConfig": false, "showBubble": true, "bubblePosition": null,
    "theme": "dark", "hotkey": "Ctrl+Shift+Space", "panelSide": "auto", "bubbleMode": "tray",
    "signInRouting": false
  },
  "ignoredDataDirs": []
}
JSON

if cc -O2 -Wall -o "$WORK/postclick" "$ROOT/tests/fixtures/macos/postclick.c" -framework ApplicationServices \
    >>"$SCRIPT_LOG" 2>&1; then
    log "click helper built"
else
    log "the click helper could not be built"
fi

# ---------------------------------------------------------------- 2. Claude for both accounts

open -n -a "$CLAUDE_APP" >>"$SCRIPT_LOG" 2>&1
open -n -a "$CLAUDE_APP" --args "--user-data-dir=$SECOND_DIR" >>"$SCRIPT_LOG" 2>&1
both_up() {
    local m s
    m="$(main_pid)"
    s="$(second_pid)"
    [ -n "$m" ] && [ -n "$s" ] && has_open_window "$m" && has_open_window "$s"
}
if wait_for 150 both_up; then
    log "Claude windows open: Main pid $(main_pid), Second pid $(second_pid)"
else
    log "Claude did not open both windows: Main pid '$(main_pid)', Second pid '$(second_pid)'"
    claude_lines | cut -c1-200 >>"$SCRIPT_LOG"
fi
# Claude's first start settles (first page, update check) before the steps begin.
sleep 15

# ---------------------------------------------------------------- 3. the log of a normal start

startup_ok=0
{
    echo "log file: $LOG_FILE"
    echo "before the start:"
    ls -la "$SUPPORT" 2>&1
} >"$OUT/startup.txt"
open -a "$APP" >>"$OUT/startup.txt" 2>&1
if wait_for 30 grep -Eq 'Sessionmux [^ ]+ starting' "$LOG_FILE"; then
    startup_ok=1
fi
sleep 3
app_pid="$(pgrep -x Sessionmux | head -n 1)"
{
    echo "process: ${app_pid:-not running}"
    if [ -n "$app_pid" ]; then
        ps -o pid=,ppid=,command= -p "$app_pid"
        lsof -p "$app_pid" 2>/dev/null | grep -F 'sessionmux.log'
    fi
    echo "after the start:"
    ls -la "$SUPPORT" 2>&1
    echo "log size: $(wc -c <"$LOG_FILE" 2>/dev/null || echo missing)"
    echo "log lines:"
    head -n 40 "$LOG_FILE" 2>&1
} >>"$OUT/startup.txt" 2>&1
screencapture -x "$OUT/startup.png" >>"$SCRIPT_LOG" 2>&1 || log "screenshot not allowed"
pkill -TERM -x Sessionmux 2>/dev/null
wait_for 10 sh -c '! pgrep -x Sessionmux >/dev/null' || pkill -KILL -x Sessionmux 2>/dev/null
sleep 1
if [ "$startup_ok" = 1 ]; then
    startup_note="ok: $(grep -Em1 'Sessionmux [^ ]+ starting' "$LOG_FILE" | cut -c1-160)"
else
    startup_note="FAILED: no startup line (log size $(wc -c <"$LOG_FILE" 2>/dev/null || echo missing))"
fi
log "startup log: $startup_note"

# ---------------------------------------------------------------- 4. Automation permission

tcc="$HOME/Library/Application Support/com.apple.TCC/TCC.db"
# An ad hoc signature has an implicit requirement, printed as "# designated => cdhash ...".
req_of() { codesign -d -r- "$1" 2>&1 | sed -n "s/^[# ]*designated => //p" | head -n 1; }
blob_of() { # requirement text -> hex of its compiled form
    printf '%s' "$1" | csreq -r- -b "$WORK/req.bin" >/dev/null 2>&1 && xxd -p "$WORK/req.bin" | tr -d '\n'
}
client_req="$(req_of "$APP")"
target_req="$(req_of "$CLAUDE_APP")"
client_blob="$(blob_of "$client_req")"
target_blob="$(blob_of "$target_req")"
if [ -n "$client_blob" ] && [ -n "$target_blob" ] && [ -f "$tcc" ]; then
    sql="INSERT OR REPLACE INTO access (service, client, client_type, auth_value, auth_reason, auth_version,
        csreq, indirect_object_identifier_type, indirect_object_identifier, indirect_object_code_identity,
        flags, last_modified) VALUES ('kTCCServiceAppleEvents', '$BUNDLE_ID', 0, 2, 3, 1, X'$client_blob', 0,
        '$CLAUDE_ID', X'$target_blob', 0, CAST(strftime('%s','now') AS INTEGER));"
    if out="$(sqlite3 "$tcc" "$sql" 2>&1)"; then
        grant_note="recorded (auth_value $(sqlite3 "$tcc" "SELECT auth_value FROM access WHERE
            service='kTCCServiceAppleEvents' AND client='$BUNDLE_ID' AND indirect_object_identifier='$CLAUDE_ID';" 2>&1))"
    else
        grant_note="could not be recorded: $out"
    fi
else
    grant_note="could not be recorded: requirement of Sessionmux '${client_req:-none}', of Claude '${target_req:-none}'"
fi
log "Automation permission: $grant_note"

# Accessibility lives in the system's own list; runners whose list can be written get the entry
# a user turning Sessionmux on in Privacy & Security would make.
systcc="/Library/Application Support/com.apple.TCC/TCC.db"
if [ -n "$client_blob" ]; then
    sql="INSERT OR REPLACE INTO access (service, client, client_type, auth_value, auth_reason, auth_version,
        csreq, indirect_object_identifier_type, indirect_object_identifier, flags, last_modified)
        VALUES ('kTCCServiceAccessibility', '$BUNDLE_ID', 0, 2, 4, 1, X'$client_blob', 0, 'UNUSED', 0,
        CAST(strftime('%s','now') AS INTEGER));"
    if out="$(sudo -n sqlite3 "$systcc" "$sql" 2>&1)"; then
        ax_note="recorded"
    else
        ax_note="could not be recorded: $out"
    fi
else
    ax_note="could not be recorded: no requirement for Sessionmux"
fi
log "Accessibility permission: $ax_note"

# ---------------------------------------------------------------- 5. the focus check

cat >"$WORK/plan.json" <<JSON
{
  "out": "$OUT/focus-check.json",
  "sync": "$SYNC",
  "waitForRunning": ["main", "$SECOND_ID"],
  "steps": [
    { "name": "S1", "account": "$SECOND_ID", "via": "panel", "expect": "front", "windowBefore": true },
    { "name": "S2", "account": "$SECOND_ID", "via": "panel", "expect": "front", "windowBefore": false },
    { "name": "S3", "account": "$SECOND_ID", "via": "panel", "expect": "record", "windowBefore": false },
    { "name": "S4", "account": "$SECOND_ID", "via": "bubble", "expect": "front", "windowBefore": false },
    { "name": "S6", "account": "$SECOND_ID", "via": "posted", "expect": "record", "windowBefore": true },
    { "name": "S7", "account": "$SECOND_ID", "via": "panel", "expect": "front", "windowBefore": false,
      "settings": { "raiseWindowsWithAccessibility": true } },
    { "name": "S8", "account": "$SECOND_ID", "via": "panel", "expect": "minimised", "windowBefore": true },
    { "name": "S9", "account": "$SECOND_ID", "via": "panel", "expect": "front", "windowBefore": false },
    { "name": "S5", "account": "$SECOND_ID", "via": "panel", "expect": "visible-failure" }
  ]
}
JSON

launch_note="Launch Services"
if ! open -n -a "$APP" --env SESSIONMUX_FOCUS_CHECK=1 --args --focus-check "$WORK/plan.json" >>"$SCRIPT_LOG" 2>&1; then
    launch_note="started directly (open --env failed)"
    SESSIONMUX_FOCUS_CHECK=1 "$APP_EXE" --focus-check "$WORK/plan.json" >>"$SCRIPT_LOG" 2>&1 &
fi
log "focus check started: $launch_note"
wait_for 60 test -f "$OUT/focus-check.json" || log "the focus check did not start"

for step in $STEPS; do
    pgrep -x Sessionmux >/dev/null || { log "Sessionmux is not running any more"; break; }
    note="$(setup_step "$step")"
    log "$step setup: $note"
    case "$note" in
        error:*) write_atomic "$SYNC/$step.go" "$(json_note error "${note#error: }")" ;;
        *) write_atomic "$SYNC/$step.go" "$(json_note note "${note#note: }")" ;;
    esac
    [ "$step" = S6 ] || click_step "$step"
    wait_for 120 test -f "$SYNC/$step.done" || log "$step did not finish"
    screencapture -x "$OUT/$step.png" >>"$SCRIPT_LOG" 2>&1 || log "$step: screenshot not allowed"
    after_step
done

wait_for 60 sh -c '! pgrep -x Sessionmux >/dev/null' || { log "Sessionmux did not quit; stopping it"; pkill -x Sessionmux; }
cp "$LOG_FILE" "$OUT/sessionmux.log" 2>/dev/null || log "no log file"

# ---------------------------------------------------------------- 6. verdicts

python3 - "$OUT/focus-check.json" "$RUNNER_LABEL" "$startup_note" "$grant_note" "$launch_note" "$SUMMARY" "$ax_note" <<'PY'
import json
import os
import sys

path, runner, startup, grant, launch, summary, ax_grant = sys.argv[1:8]
ASSERTED = {"S1", "S2", "S4", "S5", "S7", "S8", "S9"}
ALLOW_UNPROVEN = os.environ.get("SESSIONMUX_FOCUS_ALLOW_UNPROVEN") == "1"
KEY = ("open ", "claude_app: focus", "claude_app: reopen", "claude_app: activation", "claude_app: Accessibility",
       "claude_app: minimise", "claude_app: pid", "focus check: step", "ipc: openAccount", "bubble event",
       "panel: hidden")


def esc(s):
    return s.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def annotate(kind, title, body):
    title = esc(title).replace(":", "%3A").replace(",", "%2C")
    print(f"::{kind} title={title}::{esc(body)}")


lines = [f"{runner}", f"startup log: {startup}", f"Automation permission: {grant}",
         f"Accessibility permission: {ax_grant}", f"focus check: {launch}"]
failed = [] if startup.startswith("ok") else ["startup log"]
try:
    with open(path) as f:
        doc = json.load(f)
except Exception as e:  # the check never wrote its result
    doc = {"steps": [], "finished": False, "error": str(e)}
if not doc.get("finished"):
    failed.append("the focus check did not finish")
seen = {s.get("name"): s for s in doc.get("steps", [])}
for name in ["S1", "S2", "S3", "S4", "S5", "S6", "S7", "S8", "S9"]:
    s = seen.get(name)
    if s is None:
        verdict, why, body = "missing", "the step did not run", ""
    else:
        verdict, why = s.get("verdict", "?"), s.get("why", "")
        o = s.get("open") or {}
        reply = o.get("reply") or {}
        b, a = s.get("before") or {}, s.get("after") or {}
        key = [line[25:] if len(line) > 25 else line for line in s.get("log", []) if any(k in line for k in KEY)]
        body = "\n".join([
            f"setup: {json.dumps(s.get('setup'))}",
            f"click: {json.dumps(s.get('clicked'))}, received by Sessionmux: {len(s.get('clicksReceived') or [])}",
            f"open: {o.get('note', 'none')}; reply ok={reply.get('ok')} reason={reply.get('reason')}",
            f"before: front={b.get('frontAppPid')} top={b.get('topWindowPid')} target={b.get('targetPids')} "
            f"onScreen={b.get('targetOnScreen')} offScreen={b.get('targetOffScreen')}",
            f"after: front={a.get('frontAppPid')} top={a.get('topWindowPid')} onScreen={a.get('targetOnScreen')} "
            f"offScreen={a.get('targetOffScreen')} hidden={a.get('targetHidden')} "
            f"panel={a.get('panelVisible')} toasts={json.dumps(s.get('toasts'))}",
            "log:",
            *key[:25],
        ])
    asserted = name in ASSERTED
    if asserted and verdict != "pass" and not (verdict == "unproven" and ALLOW_UNPROVEN):
        failed.append(name)
    tag = "asserted" if asserted else "recorded"
    annotate("notice", f"focus {name} ({runner}): {verdict}", f"{tag}: {why}\n{body}")
    if asserted and verdict == "unproven":
        annotate("warning", f"focus {name} ({runner}) unproven", why)
    lines.append(f"{name} [{tag}] {verdict}: {why}")
annotate("notice", f"focus check ({runner})", "\n".join(lines[:5]))
lines.append("result: " + ("FAILED: " + ", ".join(failed) if failed else "passed"))
with open(summary, "w") as f:
    f.write("\n".join(lines) + "\n")
print("\n".join(lines))
sys.exit(1 if failed else 0)
PY
