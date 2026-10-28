#!/bin/sh
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

# Checks that two macOS users on one Mac each get their own Sessionmux: both take their own
# instance lock and serve their own socket at the same time, neither can reach the other's
# socket, and a "show" request stays with the user who sent it.
#
# Usage: scripts/macos-two-user.sh [Info.plist]
#
#   1. checks the given Info.plist (default: packaging/macos/Info.plist.in) and fails if it holds
#      LSMultipleInstancesProhibited, which macOS applies across every logged-in user;
#   2. builds the test executable (cargo test --no-run) and copies it to a temporary folder both
#      users can read;
#   3. creates a throwaway second user with a random password that is never printed;
#   4. runs the two-user test as the current user and, at the same time, as the second user
#      (sudo -u <user> -H); each run leaves its socket path in a shared folder for the other;
#   5. deletes the second user and its home folder again, also when a step fails.
#
# It needs password-less sudo and creates a user account, so it refuses to run outside GitHub
# Actions unless SESSIONMUX_TWO_USER_ALLOW=1 is set. What two people logged in to the same Mac at
# the same time see through Launch Services can only be checked on a real Mac.

set -eu

fail() {
    echo "macos-two-user: $*" >&2
    exit 1
}

if [ "${GITHUB_ACTIONS:-}" != "true" ] && [ "${SESSIONMUX_TWO_USER_ALLOW:-}" != "1" ]; then
    echo "This check creates and deletes a macOS user account." >&2
    echo "It runs on GitHub Actions only; set SESSIONMUX_TWO_USER_ALLOW=1 to run it anyway." >&2
    exit 2
fi
[ "$(uname -s)" = "Darwin" ] || fail "this check runs on macOS only"

ROOT=$(cd "$(dirname "$0")/.." && pwd)
PLIST=${1:-$ROOT/packaging/macos/Info.plist.in}
TEST=platform::macos::instance::tests::two_users_keep_their_own_lock_and_socket
OTHER="smtwo$(od -An -N2 -tu2 /dev/urandom | tr -d ' ')"
# Next to the current user's home folder.
OTHER_HOME="$(dirname "$HOME")/$OTHER"
SHARED=$(mktemp -d /tmp/sessionmux-two-user.XXXXXX)
CREATED=0

cleanup() {
    status=$?
    trap - EXIT
    if [ "$CREATED" = "1" ]; then
        sudo pkill -u "$OTHER" >/dev/null 2>&1 || true
        if sudo sysadminctl -deleteUser "$OTHER" >/dev/null 2>&1; then
            echo "macos-two-user: deleted the test user"
        else
            echo "macos-two-user: could not delete the test user $OTHER" >&2
        fi
        sudo rm -rf "$OTHER_HOME" 2>/dev/null || true
    fi
    rm -rf "$SHARED" 2>/dev/null || sudo rm -rf "$SHARED" 2>/dev/null || true
    exit "$status"
}
trap cleanup EXIT

# ---------------------------------------------------------------- 1. Info.plist
[ -f "$PLIST" ] || fail "no Info.plist at $PLIST"
tr -d '\r' <"$PLIST" >"$SHARED/Info.plist"
plutil -lint "$SHARED/Info.plist" >/dev/null || fail "$(basename "$PLIST") is not a valid property list"
if plutil -extract LSMultipleInstancesProhibited raw "$SHARED/Info.plist" >/dev/null 2>&1; then
    fail "$(basename "$PLIST") contains LSMultipleInstancesProhibited, which would stop a second user from opening the app"
fi
echo "macos-two-user: $(basename "$PLIST") has no LSMultipleInstancesProhibited"

# ---------------------------------------------------------------- 2. test executable
echo "macos-two-user: building the tests"
EXE=$(cd "$ROOT" && cargo test --locked --no-run --message-format=json 2>/dev/null | perl -MJSON::PP -ne '
    my $m = eval { decode_json($_) } or next;
    next unless ($m->{reason} // "") eq "compiler-artifact" && $m->{executable};
    next unless $m->{profile}{test} && grep { $_ eq "bin" } @{ $m->{target}{kind} };
    print $m->{executable}, "\n";
' | tail -n 1)
[ -n "$EXE" ] && [ -x "$EXE" ] || fail "could not find the test executable of the app"
mkdir -p "$SHARED/bin" "$SHARED/meet"
cp "$EXE" "$SHARED/bin/tests"
chmod 755 "$SHARED" "$SHARED/bin" "$SHARED/bin/tests"
# Both users leave their socket path and a done marker here.
chmod 1777 "$SHARED/meet"

# ---------------------------------------------------------------- 3. second user
PASSWORD=$(openssl rand -hex 24)
echo "macos-two-user: creating a test user"
sudo sysadminctl -addUser "$OTHER" -fullName "Sessionmux two-user test" -password "$PASSWORD" \
    -home "$OTHER_HOME" >/dev/null 2>&1 || fail "could not create the test user"
CREATED=1
PASSWORD=""
if [ ! -d "$OTHER_HOME" ]; then
    sudo createhomedir -c -u "$OTHER" >/dev/null 2>&1 || true
fi
if [ ! -d "$OTHER_HOME" ]; then
    sudo mkdir -p "$OTHER_HOME"
fi
sudo chown "$OTHER":staff "$OTHER_HOME"
sudo chmod 700 "$OTHER_HOME"
id "$OTHER" >/dev/null || fail "the test user does not exist"

# ---------------------------------------------------------------- 4. both users at once
cd "$SHARED"
echo "macos-two-user: running the test as $(id -un) and as the test user"
SESSIONMUX_TWO_USER_DIR="$SHARED/meet" perl -e 'alarm shift; exec @ARGV or die' 300 \
    "$SHARED/bin/tests" --ignored --exact "$TEST" --nocapture --test-threads=1 >"$SHARED/first.log" 2>&1 &
first=$!
sudo -u "$OTHER" -H env SESSIONMUX_TWO_USER_DIR="$SHARED/meet" perl -e 'alarm shift; exec @ARGV or die' 300 \
    "$SHARED/bin/tests" --ignored --exact "$TEST" --nocapture --test-threads=1 >"$SHARED/second.log" 2>&1 &
second=$!
set +e
wait "$first"
first_code=$?
wait "$second"
second_code=$?
set -e

echo "---- current user (exit $first_code)"
cat "$SHARED/first.log"
echo "---- test user (exit $second_code)"
cat "$SHARED/second.log"
for log in "$SHARED/first.log" "$SHARED/second.log"; do
    grep -q 'test result: ok. 1 passed' "$log" || fail "the two-user test did not pass for both users"
done
[ "$first_code" -eq 0 ] && [ "$second_code" -eq 0 ] || fail "the two-user test did not pass for both users"
echo "macos-two-user: both users kept their own lock and socket"
