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

# Records how Claude Desktop behaves on this Mac when several copies run with different data
# folders, and how the claude:// link handler behaves, and writes the answers as a plain text table.
#
# Usage: scripts/macos-probe.sh [result-file]     (default: macos-probe.txt)
#
# What it does, in order:
#   - reads the macOS version, the CPU architecture and Claude's version;
#   - below macOS 26.4 only: moves the claude:// handler to a throwaway test app and back, with
#     timings, and checks whether Claude takes the handler back when it starts. From macOS 26.4 on
#     every handler change waits for a person to answer a "Use / Keep" question, so these steps are
#     skipped and reported as "not run (needs a person)";
#   - starts Claude twice through Launch Services (`open -n -a`), once with its default data
#     folder and once with a separate test folder, and checks that both run at the same time;
#   - reads each Claude process's arguments with the same kernel call Sessionmux uses
#     (KERN_PROCARGS2), as the current user, without root;
#   - starts Claude a third time on the test folder and checks whether that copy exits and leaves
#     the running one alone;
#   - quits every Claude it started and puts the handler back.
#
# It never signs in to anything and sends nothing anywhere. The table holds process ids, counts,
# timings and yes/no answers only: no paths, no user names, no argument values. Because it quits
# Claude and changes the link handler, it refuses to run outside GitHub Actions unless
# SESSIONMUX_PROBE_ALLOW=1 is set. The script exits 0 whenever it ran to the end, whatever the
# answers were.

set -uo pipefail

if [ "${GITHUB_ACTIONS:-}" != "true" ] && [ "${SESSIONMUX_PROBE_ALLOW:-}" != "1" ]; then
    echo "This probe quits Claude and changes the claude:// link handler." >&2
    echo "It runs on GitHub Actions only; set SESSIONMUX_PROBE_ALLOW=1 to run it anyway." >&2
    exit 2
fi
if [ "$(uname -s)" != "Darwin" ]; then
    echo "This probe runs on macOS only." >&2
    exit 2
fi

OUT="${1:-macos-probe.txt}"
case "$OUT" in
    /*) ;;
    *) OUT="$PWD/$OUT" ;;
esac

CLAUDE_APP="${CLAUDE_APP:-/Applications/Claude.app}"
CLAUDE_EXE="$CLAUDE_APP/Contents/MacOS/Claude"
APP_SUPPORT="$HOME/Library/Application Support"
MAIN_DIR="$APP_SUPPORT/Claude"
TEST_DIR="$APP_SUPPORT/Claude-ProbeTest"
LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sessionmux-probe.XXXXXX")"
ROWS="$WORK/rows.txt"
: >"$ROWS"
PROBE_APP="$WORK/SessionmuxProbe.app"
PROCARGS="$WORK/procargs"
LSTOOL="$WORK/lstool"
HAVE_PROCARGS=0
HAVE_LSTOOL=0
HAVE_PROBE_APP=0
HANDLER_STEPS=0
HANDLER_SKIP_REASON=""
STARTED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

log() { echo "probe: $*" >&2; }

# One table row: id, question, answer, evidence.
row() {
    printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" >>"$ROWS"
    log "$1 $3 - $4"
}

write_table() {
    {
        echo "Sessionmux macOS probe"
        echo "runner: ${PROBE_RUNNER:-unknown}   started: $STARTED_AT   finished: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
        echo
        printf '%-6s | %-62s | %-24s | %s\n' "id" "question" "answer" "evidence"
        printf '%s\n' "-------+----------------------------------------------------------------+--------------------------+---------"
        while IFS="$(printf '\t')" read -r id q a e; do
            printf '%-6s | %-62s | %-24s | %s\n' "$id" "$q" "$a" "$e"
        done <"$ROWS"
    } >"$OUT"
}

cleanup() {
    status=$?
    trap - EXIT
    quit_all_claude
    if [ "$HANDLER_STEPS" = "1" ] && [ "$HAVE_LSTOOL" = "1" ]; then
        if [ "$(handler_label)" = "test app" ] && [ -d "$CLAUDE_APP" ]; then
            "$LSTOOL" set claude "$CLAUDE_APP" 10 >/dev/null 2>&1 || true
        fi
    fi
    if [ "$HAVE_PROBE_APP" = "1" ] && [ -x "$LSREGISTER" ]; then
        "$LSREGISTER" -u "$PROBE_APP" >/dev/null 2>&1 || true
    fi
    if [ "$status" -ne 0 ]; then
        row "ERR" "probe stopped early" "unknown" "exit status $status"
    fi
    write_table
    rm -rf "$WORK" "$TEST_DIR" 2>/dev/null || true
    echo "probe: table written to $(basename "$OUT")" >&2
    exit "$status"
}
trap cleanup EXIT

# ---------------------------------------------------------------- helpers built on the runner

build_procargs() {
    cat >"$WORK/procargs.c" <<'EOF'
/* Prints what KERN_PROCARGS2 returns for one pid: status, exec path, argc, the
   --user-data-dir value if any, and whether a --type= argument is present. */
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/sysctl.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    pid_t pid = (pid_t)atoi(argv[1]);
    int mib[3] = {CTL_KERN, KERN_ARGMAX, 0};
    int argmax = 0;
    size_t size = sizeof(argmax);
    if (sysctl(mib, 2, &argmax, &size, NULL, 0) != 0 || argmax <= 0) {
        printf("status=error\nerrno=%d\n", errno);
        return 0;
    }
    char *buf = malloc((size_t)argmax);
    if (!buf) return 1;
    mib[1] = KERN_PROCARGS2;
    mib[2] = pid;
    size = (size_t)argmax;
    int rc = sysctl(mib, 3, buf, &size, NULL, 0);
    if (rc != 0 && (errno == EINVAL || errno == EPERM)) {
        usleep(10000);
        size = (size_t)argmax;
        rc = sysctl(mib, 3, buf, &size, NULL, 0);
    }
    if (rc != 0 || size < sizeof(int)) {
        printf("status=error\nerrno=%d\n", errno);
        return 0;
    }
    int nargs = 0;
    memcpy(&nargs, buf, sizeof(int));
    char *p = buf + sizeof(int);
    char *end = buf + size;
    size_t len = strnlen(p, (size_t)(end - p));
    printf("status=ok\nexec=%.*s\n", (int)len, p);
    p += len;
    while (p < end && *p == '\0') p++;
    int seen = 0, type = 0;
    const char *udd = "";
    size_t udd_len = 0;
    for (int i = 0; i < nargs && p < end; i++) {
        len = strnlen(p, (size_t)(end - p));
        if (len >= 16 && strncmp(p, "--user-data-dir=", 16) == 0) {
            udd = p + 16;
            udd_len = len - 16;
        }
        if (len >= 7 && strncmp(p, "--type=", 7) == 0) type = 1;
        seen++;
        p += len + 1;
    }
    printf("argc=%d\nread=%d\nudd=%.*s\ntype=%d\n", nargs, seen, (int)udd_len, udd, type);
    free(buf);
    return 0;
}
EOF
    if cc -O1 -o "$PROCARGS" "$WORK/procargs.c" >"$WORK/cc.log" 2>&1; then
        HAVE_PROCARGS=1
    else
        log "could not build the argument reader"
    fi
}

build_lstool() {
    cat >"$WORK/lstool.swift" <<'EOF'
// Reads and changes the default app for a URL scheme and counts windows, for the probe.
//   current <scheme>                  -> app path or "none"
//   set <scheme> <app> <timeout-s>    -> completion_ms=<n|none> error=<domain:code|none> agent_windows_max=<n>
//   windows <pid>                     -> onscreen=<n> any=<n>   (layer-0 windows of at least 100x100)
//   agent                             -> on-screen window count of CoreServicesUIAgent
import AppKit
import CoreGraphics
import Foundation

func windowList(_ options: CGWindowListOption) -> [[String: Any]] {
    (CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]]) ?? []
}

func agentWindows() -> Int {
    windowList([.optionOnScreenOnly]).filter {
        ($0[kCGWindowOwnerName as String] as? String) == "CoreServicesUIAgent"
    }.count
}

func appWindows(_ pid: Int32, _ options: CGWindowListOption) -> Int {
    windowList(options).filter { w in
        guard (w[kCGWindowOwnerPID as String] as? Int32) == pid,
              (w[kCGWindowLayer as String] as? Int) == 0,
              let b = w[kCGWindowBounds as String] as? [String: Any],
              let width = b["Width"] as? Double, let height = b["Height"] as? Double
        else { return false }
        return width >= 100 && height >= 100
    }.count
}

final class Outcome {
    let lock = NSLock()
    var done = false
    var error: Error?
}

let args = CommandLine.arguments
guard args.count >= 2 else { exit(2) }
switch args[1] {
case "current" where args.count == 3:
    if let url = URL(string: "\(args[2])://probe"),
       let app = NSWorkspace.shared.urlForApplication(toOpen: url) {
        print(app.path)
    } else {
        print("none")
    }
case "set" where args.count == 5:
    let app = URL(fileURLWithPath: args[3])
    let timeout = Double(args[4]) ?? 20
    let outcome = Outcome()
    let start = Date()
    var finishedAt: Date?
    var agentMax = 0
    NSWorkspace.shared.setDefaultApplication(at: app, toOpenURLsWithScheme: args[2]) { error in
        outcome.lock.lock()
        outcome.error = error
        outcome.done = true
        outcome.lock.unlock()
    }
    while Date().timeIntervalSince(start) < timeout {
        RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.1))
        agentMax = max(agentMax, agentWindows())
        outcome.lock.lock()
        let done = outcome.done
        outcome.lock.unlock()
        if done {
            finishedAt = Date()
            break
        }
    }
    let ms = finishedAt.map { String(Int($0.timeIntervalSince(start) * 1000)) } ?? "none"
    var err = "none"
    if let e = outcome.error as NSError? { err = "\(e.domain):\(e.code)" }
    print("completion_ms=\(ms) error=\(err) agent_windows_max=\(agentMax)")
case "windows" where args.count == 3:
    let pid = Int32(args[2]) ?? 0
    let onscreen = appWindows(pid, [.optionOnScreenOnly, .excludeDesktopElements])
    let any = appWindows(pid, [.optionAll])
    print("onscreen=\(onscreen) any=\(any)")
case "agent":
    print(agentWindows())
default:
    exit(2)
}
EOF
    if swiftc -O -o "$LSTOOL" "$WORK/lstool.swift" >"$WORK/swiftc.log" 2>&1; then
        HAVE_LSTOOL=1
    else
        log "could not build the handler tool"
        tail -n 5 "$WORK/swiftc.log" >&2 || true
    fi
}

build_probe_app() {
    mkdir -p "$PROBE_APP/Contents/MacOS"
    cat >"$PROBE_APP/Contents/Info.plist" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>test.sessionmux.probe</string>
    <key>CFBundleName</key><string>SessionmuxProbe</string>
    <key>CFBundleExecutable</key><string>SessionmuxProbe</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleVersion</key><string>1</string>
    <key>CFBundleShortVersionString</key><string>1.0</string>
    <key>LSUIElement</key><true/>
    <key>CFBundleURLTypes</key>
    <array>
        <dict>
            <key>CFBundleURLName</key><string>Probe link</string>
            <key>CFBundleURLSchemes</key><array><string>claude</string></array>
        </dict>
    </array>
</dict>
</plist>
EOF
    printf 'int main(void) { return 0; }\n' >"$WORK/noop.c"
    if ! cc -O1 -o "$PROBE_APP/Contents/MacOS/SessionmuxProbe" "$WORK/noop.c" >/dev/null 2>&1; then
        printf '#!/bin/sh\nexit 0\n' >"$PROBE_APP/Contents/MacOS/SessionmuxProbe"
        chmod 755 "$PROBE_APP/Contents/MacOS/SessionmuxProbe"
    fi
    codesign --force --sign - "$PROBE_APP" >/dev/null 2>&1 || true
    if [ -x "$LSREGISTER" ] && "$LSREGISTER" -f "$PROBE_APP" >/dev/null 2>&1; then
        HAVE_PROBE_APP=1
    else
        log "could not register the test app"
    fi
}

# ---------------------------------------------------------------- Claude processes

# Prints "<kind> <folder>" for a pid, where kind is main, helper or other and folder is main,
# test or other. Uses KERN_PROCARGS2 when the reader was built, else `ps`.
classify() {
    pid="$1"
    if [ "$HAVE_PROCARGS" = "1" ]; then
        info="$("$PROCARGS" "$pid" 2>/dev/null)" || info=""
        st="$(printf '%s\n' "$info" | sed -n 's/^status=//p')"
        [ "$st" = "ok" ] || { echo "other other"; return; }
        exe="$(printf '%s\n' "$info" | sed -n 's/^exec=//p')"
        udd="$(printf '%s\n' "$info" | sed -n 's/^udd=//p')"
        typ="$(printf '%s\n' "$info" | sed -n 's/^type=//p')"
    else
        args="$(ps -o args= -p "$pid" 2>/dev/null)" || args=""
        case "$args" in
            "$CLAUDE_EXE"*) exe="$CLAUDE_EXE" ;;
            *) exe="" ;;
        esac
        udd=""
        case "$args" in *"--user-data-dir=$TEST_DIR"*) udd="$TEST_DIR" ;; *"--user-data-dir="*) udd="?" ;; esac
        typ=0
        case "$args" in *"--type="*) typ=1 ;; esac
    fi
    if [ "$exe" != "$CLAUDE_EXE" ]; then
        echo "other other"
        return
    fi
    kind=main
    [ "$typ" = "1" ] && kind=helper
    case "$udd" in
        "") folder=main ;;
        "$TEST_DIR") folder=test ;;
        *) folder=other ;;
    esac
    echo "$kind $folder"
}

# Pids started from Claude's main executable (helpers live in other bundles), one per line.
claude_candidates() {
    ps -axww -o pid=,args= 2>/dev/null | awk -v exe="$CLAUDE_EXE" '
        {
            pid = $1
            sub(/^ *[0-9]+ /, "")
            if (index($0, exe) == 1) {
                rest = substr($0, length(exe) + 1)
                if (rest == "" || substr(rest, 1, 1) == " ") print pid
            }
        }'
}

# Main-process pids for one folder ("main" or "test"), space-separated.
main_pids() {
    want="$1"
    found=""
    for p in $(claude_candidates); do
        if [ "$(classify "$p")" = "main $want" ]; then
            found="$found $p"
        fi
    done
    echo "$found" | sed 's/^ *//'
}

count_words() { echo $#; }

# Waits up to $2 seconds for a main process on folder $1; prints its pids.
wait_main() {
    want="$1"
    limit="$2"
    t0=$SECONDS
    while [ $((SECONDS - t0)) -lt "$limit" ]; do
        pids="$(main_pids "$want")"
        if [ -n "$pids" ]; then
            echo "$pids"
            return 0
        fi
        sleep 0.5
    done
    echo ""
    return 1
}

# True while the pid runs; an exited child that was not reaped yet counts as gone.
alive() {
    kill -0 "$1" 2>/dev/null || return 1
    case "$(ps -o stat= -p "$1" 2>/dev/null)" in
        "" | Z*) return 1 ;;
    esac
    return 0
}

quit_all_claude() {
    pids="$(claude_candidates)"
    [ -n "$pids" ] || return 0
    for p in $pids; do kill -TERM "$p" 2>/dev/null || true; done
    t0=$SECONDS
    while [ $((SECONDS - t0)) -lt 8 ]; do
        left=""
        for p in $pids; do alive "$p" && left="$left $p"; done
        [ -n "$left" ] || return 0
        sleep 0.5
    done
    for p in $pids; do kill -KILL "$p" 2>/dev/null || true; done
    pkill -KILL -f "$CLAUDE_APP/Contents/" 2>/dev/null || true
}

window_summary() {
    out=""
    for p in $1; do
        if [ "$HAVE_LSTOOL" = "1" ]; then
            out="$out pid $p: $("$LSTOOL" windows "$p" 2>/dev/null || echo "windows unknown");"
        fi
    done
    echo "$out" | sed 's/^ *//; s/;$//'
}

lock_state() {
    dir="$1"
    if [ -L "$dir/SingletonLock" ]; then
        echo "symlink"
    elif [ -e "$dir/SingletonLock" ]; then
        echo "file"
    else
        echo "absent"
    fi
}

# Names the current claude:// handler without printing its path.
handler_label() {
    [ "$HAVE_LSTOOL" = "1" ] || { echo "unknown"; return; }
    h="$("$LSTOOL" current claude 2>/dev/null)" || h=""
    case "$h" in
        "" ) echo "unknown" ;;
        none) echo "none" ;;
        */SessionmuxProbe.app) echo "test app" ;;
        */Claude.app) echo "Claude" ;;
        *) echo "another app" ;;
    esac
}

now_ms() { perl -MTime::HiRes=time -e 'printf "%d
", time() * 1000'; }

# Polls the handler for up to $2 ms until it is $1; prints the elapsed ms or "never".
wait_handler() {
    want="$1"
    limit_ms="$2"
    t_start="$(now_ms)"
    while :; do
        waited=$(($(now_ms) - t_start))
        if [ "$(handler_label)" = "$want" ]; then
            echo "$waited"
            return 0
        fi
        [ "$waited" -le "$limit_ms" ] || break
        sleep 0.1
    done
    echo "never"
    return 1
}

# ---------------------------------------------------------------- context

MACOS_VERSION="$(sw_vers -productVersion 2>/dev/null || echo unknown)"
MACOS_BUILD="$(sw_vers -buildVersion 2>/dev/null || echo unknown)"
ARCH="$(uname -m)"
row "C1" "macOS version" "info" "$MACOS_VERSION ($MACOS_BUILD), $ARCH, image ${ImageOS:-unknown} ${ImageVersion:-}"

if [ -x "$CLAUDE_EXE" ]; then
    CLAUDE_VERSION="$(defaults read "$CLAUDE_APP/Contents/Info.plist" CFBundleShortVersionString 2>/dev/null || echo unknown)"
    CLAUDE_ID="$(defaults read "$CLAUDE_APP/Contents/Info.plist" CFBundleIdentifier 2>/dev/null || echo unknown)"
    if xattr -p com.apple.quarantine "$CLAUDE_APP" >/dev/null 2>&1; then q="quarantined"; else q="not quarantined"; fi
    row "C2" "Claude Desktop installed" "yes" "version $CLAUDE_VERSION, bundle id $CLAUDE_ID, $q"
else
    row "C2" "Claude Desktop installed" "no" "Claude.app not found; every Claude question below is unknown"
    exit 0
fi

if [ "$(id -u)" = "0" ]; then
    row "C3" "probe runs as a normal user" "no" "running as root, so the argument check below proves nothing"
else
    row "C3" "probe runs as a normal user" "yes" "uid is not 0"
fi
if [ -u /bin/ps ]; then ps_note="ps is setuid root"; else ps_note="ps is not setuid"; fi

build_procargs
build_lstool
row "C4" "helper tools built" "info" "argument reader: $([ "$HAVE_PROCARGS" = 1 ] && echo yes || echo no), handler tool: $([ "$HAVE_LSTOOL" = 1 ] && echo yes || echo no)"

quit_all_claude
rm -rf "$TEST_DIR"

IFS=. read -r v_major v_minor _ <<EOF
$MACOS_VERSION
EOF
v_major="${v_major:-0}"
v_minor="${v_minor:-0}"
case "$v_major$v_minor" in *[!0-9]*) v_major=0; v_minor=0 ;; esac
if [ "$v_major" -gt 26 ] || { [ "$v_major" -eq 26 ] && [ "$v_minor" -ge 4 ]; }; then
    HANDLER_SKIP_REASON="macOS $MACOS_VERSION is 26.4 or later: a handler change waits for a person to answer Use / Keep"
elif [ "$v_major" -eq 0 ]; then
    HANDLER_SKIP_REASON="macOS version unreadable, so a handler change might wait for a person"
elif [ "$HAVE_LSTOOL" != "1" ]; then
    HANDLER_SKIP_REASON="handler tool could not be built"
else
    build_probe_app
    if [ "$HAVE_PROBE_APP" = "1" ]; then
        HANDLER_STEPS=1
    else
        HANDLER_SKIP_REASON="test app could not be registered"
    fi
fi
[ -n "$HANDLER_SKIP_REASON" ] && log "handler steps skipped: $HANDLER_SKIP_REASON"

row "H0" "claude:// handler before any Claude start" "$(handler_label)" "read through Launch Services"

# ---------------------------------------------------------------- handler round trip

if [ "$HANDLER_STEPS" = "1" ]; then
    take="$("$LSTOOL" set claude "$PROBE_APP" 20 2>/dev/null || echo "tool failed")"
    take_seen="$(wait_handler "test app" 2000)"
    restore="$("$LSTOOL" set claude "$CLAUDE_APP" 20 2>/dev/null || echo "tool failed")"
    restore_seen="$(wait_handler "Claude" 2000)"
    answer="unknown"
    case "$take $restore" in
        *"agent_windows_max=0"*"agent_windows_max=0"*)
            if [ "$take_seen" != "never" ] && [ "$restore_seen" != "never" ] &&
                ! printf '%s' "$take $restore" | grep -Eq 'completion_ms=none|error=[^n]'; then
                answer="yes"
            fi
            ;;
        *"agent_windows_max="*)
            printf '%s' "$take $restore" | grep -Eq 'agent_windows_max=[1-9]' && answer="no"
            ;;
    esac
    row "R1" "handler take and restore need no prompt (below 26.4)" "$answer" \
        "take: $take, seen after ${take_seen} ms; restore: $restore, seen after ${restore_seen} ms"
else
    row "R1" "handler take and restore need no prompt (below 26.4)" "not run (needs a person)" "$HANDLER_SKIP_REASON"
fi

# ---------------------------------------------------------------- Claude takes the handler back

if [ "$HANDLER_STEPS" = "1" ]; then
    take="$("$LSTOOL" set claude "$PROBE_APP" 20 2>/dev/null || echo "tool failed")"
    before="$(handler_label)"
    open -n -a "$CLAUDE_APP" 2>/dev/null || log "open failed for the default folder"
    mpids="$(wait_main main 90)"
    if [ -z "$mpids" ]; then
        row "R2" "Claude makes itself the claude:// handler when it starts" "unknown" \
            "handler before start: $before; Claude did not start within 90 s"
    else
        back="$(wait_handler "Claude" 30000)"
        if [ "$back" != "never" ]; then
            row "R2" "Claude makes itself the claude:// handler when it starts" "yes" \
                "handler before start: $before; Claude again after ${back} ms from its process appearing"
        else
            row "R2" "Claude makes itself the claude:// handler when it starts" "no" \
                "handler before start: $before; still $(handler_label) 30 s after Claude started"
        fi
        agent="$("$LSTOOL" agent 2>/dev/null || echo unknown)"
        row "R2w" "a Use / Keep window is open after Claude started" "info" "CoreServicesUIAgent on-screen windows: $agent"
    fi
    if [ "$(handler_label)" != "Claude" ]; then
        "$LSTOOL" set claude "$CLAUDE_APP" 20 >/dev/null 2>&1 || true
    fi
else
    row "R2" "Claude makes itself the claude:// handler when it starts" "not run (needs a person)" "$HANDLER_SKIP_REASON"
fi

# ---------------------------------------------------------------- two folders at once

mpids="$(main_pids main)"
if [ -z "$mpids" ]; then
    open -n -a "$CLAUDE_APP" 2>/dev/null || log "open failed for the default folder"
    mpids="$(wait_main main 90)"
fi
if [ "$HANDLER_STEPS" != "1" ]; then
    row "H1" "claude:// handler after Claude started" "$(handler_label)" "read without changing anything"
    if [ "$HAVE_LSTOOL" = "1" ]; then
        row "H1w" "a Use / Keep window is open after Claude started" "info" \
            "CoreServicesUIAgent on-screen windows: $("$LSTOOL" agent 2>/dev/null || echo unknown)"
    fi
fi
open -n -a "$CLAUDE_APP" --args "--user-data-dir=$TEST_DIR" 2>/dev/null || log "open failed for the test folder"
tpids="$(wait_main test 90)"
sleep 15
mpids="$(main_pids main)"
tpids="$(main_pids test)"
mcount="$(count_words $mpids)"
tcount="$(count_words $tpids)"
if [ "$mcount" -ge 1 ] && [ "$tcount" -ge 1 ]; then
    both_run="yes"
elif [ "$mcount" -ge 1 ] || [ "$tcount" -ge 1 ]; then
    both_run="no"
else
    both_run="unknown"
fi
row "P1" "two data folders run at the same time" "$both_run" \
    "main processes 15 s after both starts: default folder $mcount (${mpids:-none}), test folder $tcount (${tpids:-none})"
row "P1w" "windows of each main process" "info" "default: $(window_summary "$mpids"); test: $(window_summary "$tpids")"
row "P1l" "Electron single-instance lock per folder" "info" \
    "SingletonLock in default folder: $(lock_state "$MAIN_DIR"), in test folder: $(lock_state "$TEST_DIR")"

# ---------------------------------------------------------------- arguments readable

if [ "$HAVE_PROCARGS" = "1" ]; then
    total=0
    ok=0
    errs=""
    for p in $(claude_candidates); do
        total=$((total + 1))
        info="$("$PROCARGS" "$p" 2>/dev/null)" || info=""
        if printf '%s\n' "$info" | grep -qx 'status=ok'; then
            ok=$((ok + 1))
        else
            errs="$errs $(printf '%s\n' "$info" | sed -n 's/^errno=//p')"
        fi
    done
    test_udd="no"
    [ -n "$tpids" ] && test_udd="yes"
    main_clean="no"
    [ -n "$mpids" ] && main_clean="yes"
    if [ "$total" -eq 0 ]; then
        argv="unknown"
    elif [ "$ok" -eq "$total" ] && [ "$test_udd" = "yes" ] && [ "$(id -u)" != "0" ]; then
        argv="yes"
    elif [ "$ok" -eq 0 ]; then
        argv="no"
    else
        argv="unknown"
    fi
    row "A1" "KERN_PROCARGS2 reads Claude's arguments without root" "$argv" \
        "read $ok of $total Claude processes${errs:+ (errno$errs)}; --user-data-dir seen on the test instance: $test_udd; default instance found without it: $main_clean"
else
    row "A1" "KERN_PROCARGS2 reads Claude's arguments without root" "unknown" "argument reader could not be built"
fi
ps_seen="no"
for p in $tpids; do
    ps -o args= -p "$p" 2>/dev/null | grep -q -- '--user-data-dir=' && ps_seen="yes"
done
row "A2" "ps shows --user-data-dir on the test instance" "$ps_seen" "$ps_note"

# ---------------------------------------------------------------- same folder again

if [ -n "$tpids" ]; then
    before_all=" $(claude_candidates | tr '\n' ' ') "
    before_test="$tpids"
    open -n -a "$CLAUDE_APP" --args "--user-data-dir=$TEST_DIR" 2>/dev/null || log "open failed for the third start"
    new_seen=""
    checked=" "
    t0=$SECONDS
    while [ $((SECONDS - t0)) -lt 20 ]; do
        for p in $(claude_candidates); do
            case "$before_all$checked" in *" $p "*) continue ;; esac
            checked="$checked$p "
            if [ "$(classify "$p")" = "main test" ]; then
                new_seen="$new_seen $p"
            fi
        done
        sleep 0.2
    done
    new_seen="$(echo "$new_seen" | sed 's/^ *//')"
    after_test="$(main_pids test)"
    new_left=""
    for p in $new_seen; do alive "$p" && new_left="$new_left $p"; done
    first_alive="yes"
    for p in $before_test; do alive "$p" || first_alive="no"; done
    bc="$(count_words $before_test)"
    ac="$(count_words $after_test)"
    if [ "$ac" -gt "$bc" ] || [ -n "$new_left" ]; then
        again="no"
    elif [ "$ac" -eq "$bc" ] && [ -n "$new_seen" ] && [ "$first_alive" = "yes" ]; then
        again="yes"
    elif [ "$ac" -eq "$bc" ] && [ "$first_alive" = "yes" ]; then
        again="unknown"
    else
        again="no"
    fi
    row "S1" "a second start on a running folder exits (open -n)" "$again" \
        "test-folder main processes before $bc, 20 s after $ac; new process seen: ${new_seen:-none}, still running: ${new_left:-none}; first instance alive: $first_alive"

    # The same check with the executable started directly, which gives an exit code.
    "$CLAUDE_EXE" "--user-data-dir=$TEST_DIR" >/dev/null 2>&1 &
    dpid=$!
    t0=$SECONDS
    while [ $((SECONDS - t0)) -lt 20 ] && alive "$dpid"; do sleep 0.2; done
    if alive "$dpid"; then
        kill -TERM "$dpid" 2>/dev/null || true
        sleep 2
        alive "$dpid" && kill -KILL "$dpid" 2>/dev/null
        wait "$dpid" 2>/dev/null
        row "S2" "a second start on a running folder exits (direct start)" "no" "still running after 20 s"
    else
        wait "$dpid" 2>/dev/null
        code=$?
        took=$((SECONDS - t0))
        first_alive="yes"
        for p in $before_test; do alive "$p" || first_alive="no"; done
        after_test="$(main_pids test)"
        if [ "$code" -eq 0 ] && [ "$first_alive" = "yes" ] && [ "$(count_words $after_test)" -eq "$bc" ]; then
            direct="yes"
        else
            direct="unknown"
        fi
        row "S2" "a second start on a running folder exits (direct start)" "$direct" \
            "exited with code $code within ${took} s; first instance alive: $first_alive; test-folder main processes after: $(count_words $after_test)"
    fi
else
    row "S1" "a second start on a running folder exits (open -n)" "unknown" "the test-folder instance was not running"
    row "S2" "a second start on a running folder exits (direct start)" "unknown" "the test-folder instance was not running"
fi

if [ "$HANDLER_STEPS" = "1" ]; then
    row "H2" "claude:// handler at the end, before cleanup" "$(handler_label)" "read through Launch Services"
fi

exit 0
