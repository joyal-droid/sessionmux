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

# Builds Sessionmux.app for macOS and packs it as a disk image and a zip.
#
# Usage: scripts/bundle-macos.sh <version> <out-dir>
#
#   1. sets MACOSX_DEPLOYMENT_TARGET=13.0 and ZERO_AR_DATE=1, takes SOURCE_DATE_EPOCH and
#      SESSIONMUX_BUILD_DATE from the checked-out commit (each unless already set) and adds
#      --remap-path-prefix flags to RUSTFLAGS, so no absolute path of this machine ends up in the
#      binary:  $HOME=/home  $CARGO_HOME=/cargo  $RUSTUP_HOME=/rustc  <repository>=/src
#   2. cargo build --release --locked for aarch64-apple-darwin and x86_64-apple-darwin
#   3. joins both into one universal binary (lipo) at Sessionmux.app/Contents/MacOS/Sessionmux
#   4. writes Contents/Info.plist from packaging/macos/Info.plist.in and checks it (plutil); the
#      build stops if LSMultipleInstancesProhibited is present
#   5. Contents/Resources/Sessionmux.icns from assets/macos/Sessionmux.iconset (iconutil)
#   6. signs the app ad hoc (no certificate, no timestamp) and verifies the signature
#   7. Sessionmux-<version>-macOS.zip (ditto, keeps the signature) and
#      Sessionmux-<version>-macOS.dmg, plus byte-identical copies named Sessionmux-macOS.zip and
#      Sessionmux-macOS.dmg. The disk image opens as a styled window: the app on the left, a link
#      to /Applications on the right and an arrow between them, drawn by
#      packaging/macos/dmg-background.png (and @2x) with the layout in
#      packaging/macos/dmg-settings.py. dmgbuild writes the window settings (.DS_Store), the
#      background picture and the volume icon without opening Finder; it is installed with pip
#      --require-hashes from packaging/macos/dmg-requirements.txt into a throwaway virtual
#      environment under the target folder. It needs Python 3.10 or later: python3, or the
#      interpreter named in SESSIONMUX_PYTHON
#   8. runs the bundled binary once: the self-check with the demo data (--smoke) and a hook
#      helper call with no app running (set SESSIONMUX_BUNDLE_SKIP_RUN=1 to leave this out)
#   9. checks the outputs: the disk image is UDZO with an HFS+ volume named Sessionmux, mounts
#      and holds the app and the Applications link; packaging/macos/check-dmg.py then checks the
#      window: background picture (1x and 2x), volume icon, and in .DS_Store the window bounds,
#      hidden bars, icon view, icon and text size, background picture and both icon positions.
#      The zip extracts to an app with a valid signature, and Gatekeeper's assessment is printed
#      (an ad-hoc signed app is expected to be rejected there until it is opened once with Open
#      Anyway)
#  10. writes BUILDINFO-macOS.txt (versions of the tools used) and SHA256SUMS-macos
#
# Everything is written to <out-dir>; Sessionmux.app stays there as well. The binary is the file
# to compare after rebuilding the same commit: its SHA-256 is in SHA256SUMS-macos. The zip and the
# disk image carry timestamps and are not reproducible.

set -eu

fail() {
    echo "bundle-macos: $*" >&2
    exit 1
}
step() { echo "==> $*"; }

[ "$#" -eq 2 ] || fail "usage: scripts/bundle-macos.sh <version> <out-dir>"
VERSION=$1
OUT=$2
[ "$(uname -s)" = "Darwin" ] || fail "this script runs on macOS only"
if ! printf '%s\n' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'; then
    fail "version must look like 1.2.3 or 1.2.3-preview.1 (got '$VERSION')"
fi
# CFBundleVersion takes numbers only: 1.1.0-preview.1 -> 1.1.0.
BUNDLE_VERSION=${VERSION%%-*}

ROOT=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
NAME=Sessionmux
BUNDLE_ID=io.github.joyal-droid.sessionmux
APP="$OUT/$NAME.app"
BIN="$APP/Contents/MacOS/$NAME"
ZIP="$NAME-$VERSION-macOS.zip"
DMG="$NAME-$VERSION-macOS.dmg"
WORK=$(mktemp -d "${TMPDIR:-/tmp}/sessionmux-bundle.XXXXXX")
MOUNTED=""
VENV=""

cleanup() {
    status=$?
    trap - EXIT
    if [ -n "$MOUNTED" ]; then hdiutil detach "$MOUNTED" -force >/dev/null 2>&1 || true; fi
    rm -rf "$WORK"
    if [ -n "${VENV:-}" ]; then rm -rf "$VENV"; fi
    exit "$status"
}
trap cleanup EXIT

# ---------------------------------------------------------------- 1. environment
export MACOSX_DEPLOYMENT_TARGET=13.0
export ZERO_AR_DATE=1
if [ -z "${SOURCE_DATE_EPOCH:-}" ] && git -C "$ROOT" rev-parse --show-toplevel >/dev/null 2>&1; then
    top=$(cd "$(git -C "$ROOT" rev-parse --show-toplevel)" && pwd -P)
    if [ "$top" = "$(cd "$ROOT" && pwd -P)" ]; then
        SOURCE_DATE_EPOCH=$(git -C "$ROOT" log -1 --format=%ct)
    fi
fi
if [ -n "${SOURCE_DATE_EPOCH:-}" ]; then
    printf '%s\n' "$SOURCE_DATE_EPOCH" | grep -Eq '^[0-9]+$' || fail "SOURCE_DATE_EPOCH must be a number of seconds"
    export SOURCE_DATE_EPOCH
fi
if [ -z "${SESSIONMUX_BUILD_DATE:-}" ]; then
    if [ -n "${SOURCE_DATE_EPOCH:-}" ]; then
        SESSIONMUX_BUILD_DATE=$(date -u -r "$SOURCE_DATE_EPOCH" +%Y-%m-%d)
    else
        SESSIONMUX_BUILD_DATE=$(date -u +%Y-%m-%d)
    fi
fi
export SESSIONMUX_BUILD_DATE

[ -z "${CARGO_ENCODED_RUSTFLAGS:-}" ] || fail "CARGO_ENCODED_RUSTFLAGS is set and would override the path remapping; use RUSTFLAGS"
CARGO_HOME_DIR=${CARGO_HOME:-$HOME/.cargo}
RUSTUP_HOME_DIR=${RUSTUP_HOME:-$HOME/.rustup}
remap=""
# The last matching prefix wins, so the more specific ones come last.
for pair in "$HOME=/home" "$CARGO_HOME_DIR=/cargo" "$RUSTUP_HOME_DIR=/rustc" "$ROOT=/src"; do
    from=${pair%=*}
    to=${pair##*=}
    case "$from" in
        *[[:space:]]*) fail "cannot remap a path that contains spaces through RUSTFLAGS: $from" ;;
    esac
    from=${from%/}
    remap="$remap --remap-path-prefix=$from=$to"
done
RUSTFLAGS="${RUSTFLAGS:-}$remap"
RUSTFLAGS=${RUSTFLAGS# }
export RUSTFLAGS
echo "RUSTFLAGS: $RUSTFLAGS"
echo "SOURCE_DATE_EPOCH: ${SOURCE_DATE_EPOCH:-unset}, SESSIONMUX_BUILD_DATE: $SESSIONMUX_BUILD_DATE"

TARGET_DIR=${CARGO_TARGET_DIR:-$ROOT/target}
case "$TARGET_DIR" in
    /*) ;;
    *) TARGET_DIR="$ROOT/$TARGET_DIR" ;;
esac

# The disk image tools need Python 3.10 or later; checked here so a missing one stops the build
# before the long compile.
PYTHON=${SESSIONMUX_PYTHON:-python3}
command -v "$PYTHON" >/dev/null 2>&1 || fail "$PYTHON not found (Python 3.10 or later writes the disk image window; set SESSIONMUX_PYTHON)"
"$PYTHON" -c 'import sys; sys.exit(sys.version_info < (3, 10))' \
    || fail "$PYTHON is $("$PYTHON" -V 2>&1); the disk image window needs Python 3.10 or later (set SESSIONMUX_PYTHON)"
echo "Python for the disk image: $("$PYTHON" -V 2>&1)"

# ---------------------------------------------------------------- 2. build both slices
for target in aarch64-apple-darwin x86_64-apple-darwin; do
    step "cargo build --release --locked --target $target"
    (cd "$ROOT" && cargo build --release --locked --target "$target")
    [ -f "$TARGET_DIR/$target/release/$NAME" ] || fail "$TARGET_DIR/$target/release/$NAME was not built"
done

# ---------------------------------------------------------------- 3. universal binary
step "Sessionmux.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create -output "$BIN" \
    "$TARGET_DIR/aarch64-apple-darwin/release/$NAME" "$TARGET_DIR/x86_64-apple-darwin/release/$NAME"
lipo "$BIN" -verify_arch arm64 x86_64 || fail "the binary is not universal (arm64 + x86_64)"
lipo -info "$BIN"

# ---------------------------------------------------------------- 4. Info.plist
PLIST="$APP/Contents/Info.plist"
tr -d '\r' <"$ROOT/packaging/macos/Info.plist.in" \
    | sed -e "s/@VERSION@/$VERSION/g" -e "s/@BUNDLE_VERSION@/$BUNDLE_VERSION/g" >"$PLIST"
plutil -lint "$PLIST"
if grep -q '@[A-Z_]*@' "$PLIST"; then fail "Info.plist still holds a placeholder"; fi
if plutil -extract LSMultipleInstancesProhibited raw "$PLIST" >/dev/null 2>&1; then
    fail "Info.plist must not contain LSMultipleInstancesProhibited (it would stop a second macOS user from opening the app)"
fi
[ "$(plutil -extract CFBundleShortVersionString raw "$PLIST")" = "$VERSION" ] || fail "CFBundleShortVersionString is not $VERSION"
[ "$(plutil -extract CFBundleIdentifier raw "$PLIST")" = "$BUNDLE_ID" ] || fail "CFBundleIdentifier is not $BUNDLE_ID"
printf 'APPL????' >"$APP/Contents/PkgInfo"

# ---------------------------------------------------------------- 5. icon
iconutil -c icns "$ROOT/assets/macos/$NAME.iconset" -o "$APP/Contents/Resources/$NAME.icns"

# ---------------------------------------------------------------- 6. ad-hoc signature
step "codesign (ad hoc)"
xattr -cr "$APP"
codesign --force --sign - --timestamp=none --identifier "$BUNDLE_ID" "$APP"
codesign --verify --strict --all-architectures -vv "$APP"
codesign -dv "$APP" 2>&1

# ---------------------------------------------------------------- 7. zip and disk image
step "$ZIP"
rm -f "$OUT/$ZIP" "$OUT/$NAME-macOS.zip" "$OUT/$DMG" "$OUT/$NAME-macOS.dmg"
(cd "$OUT" && ditto -c -k --sequesterRsrc --keepParent "$NAME.app" "$ZIP")

step "$DMG"
# dmgbuild and the two libraries it uses, pinned by version and SHA-256, in a fresh virtual
# environment (pip ignores any user configuration and installs wheels only).
VENV="$TARGET_DIR/dmgbuild-venv"
rm -rf "$VENV"
"$PYTHON" -m venv "$VENV"
"$VENV/bin/python" -m pip install --isolated --disable-pip-version-check --no-input --quiet \
    --require-hashes --only-binary=:all: -r "$ROOT/packaging/macos/dmg-requirements.txt" \
    || fail "could not install the disk image tools from packaging/macos/dmg-requirements.txt"
DMGBUILD_VERSION=$("$VENV/bin/python" -m pip list --isolated --disable-pip-version-check --format=freeze \
    | grep -E '^(dmgbuild|ds[-_]store|mac[-_]alias)==' | tr '\n' ' ' | sed 's/ *$//')
echo "disk image tools: $DMGBUILD_VERSION"
# The layout is written straight into .DS_Store, so Finder is never asked to arrange the window.
# hdiutil sometimes reports "Resource busy" on a busy machine; a second try usually works.
tries=0
until "$VENV/bin/python" -m dmgbuild -s "$ROOT/packaging/macos/dmg-settings.py" \
    -D "app=$APP" -D "icon=$APP/Contents/Resources/$NAME.icns" \
    -D "background=$ROOT/packaging/macos/dmg-background.png" \
    "$NAME" "$OUT/$DMG"; do
    tries=$((tries + 1))
    [ "$tries" -lt 4 ] || fail "dmgbuild failed"
    echo "dmgbuild failed, trying again in 5 s"
    sleep 5
done
cp "$OUT/$ZIP" "$OUT/$NAME-macOS.zip"
cp "$OUT/$DMG" "$OUT/$NAME-macOS.dmg"

# ---------------------------------------------------------------- 8. run the bundled binary
if [ "${SESSIONMUX_BUNDLE_SKIP_RUN:-}" != "1" ]; then
    step "self-check of the bundled binary"
    RUN="$WORK/run"
    mkdir -p "$RUN/home"
    set +e
    (cd "$RUN" && SESSIONMUX_SMOKE_FIXTURE=1 perl -e 'alarm shift; exec @ARGV or die' 90 "$BIN" --smoke) >"$RUN/smoke.log" 2>&1
    code=$?
    set -e
    cat "$RUN/smoke.log"
    [ "$code" -eq 0 ] || fail "--smoke exited with $code"
    grep -q '"ok":true' "$RUN/smoke.log" || fail '--smoke did not report {"ok":true}'
    echo '{}' | HOME="$RUN/home" "$BIN" --notify-hook || fail "--notify-hook exited with $?"
    echo "--smoke and --notify-hook: exit 0"
fi

# ---------------------------------------------------------------- 9. check the outputs
step "check the disk image"
hdiutil imageinfo "$OUT/$DMG" >"$WORK/imageinfo.txt" || fail "hdiutil imageinfo failed"
grep -Eq '^Format: UDZO$' "$WORK/imageinfo.txt" || fail "the disk image is not UDZO"
MNT="$WORK/mnt"
mkdir -p "$MNT"
hdiutil attach "$OUT/$DMG" -readonly -nobrowse -noverify -mountpoint "$MNT" >/dev/null
MOUNTED=$MNT
volname=$(diskutil info "$MNT" | sed -n 's/^ *Volume Name: *//p')
[ "$volname" = "$NAME" ] || fail "the volume is named '$volname', expected $NAME"
fstype=$(diskutil info "$MNT" | sed -n 's/^ *Type (Bundle): *//p')
[ "$fstype" = "hfs" ] || fail "the volume is '$fstype', expected HFS+ (hfs)"
[ -x "$MNT/$NAME.app/Contents/MacOS/$NAME" ] || fail "the disk image holds no $NAME.app"
[ -L "$MNT/Applications" ] && [ "$(readlink "$MNT/Applications")" = "/Applications" ] \
    || fail "the disk image holds no link to /Applications"
codesign --verify --strict --all-architectures "$MNT/$NAME.app" || fail "the app in the disk image has no valid signature"
cmp -s "$MNT/$NAME.app/Contents/MacOS/$NAME" "$BIN" || fail "the binary in the disk image differs from the built one"
for f in .background.tiff .VolumeIcon.icns .DS_Store; do
    [ -f "$MNT/$f" ] || fail "the disk image holds no $f"
done
"$VENV/bin/python" "$ROOT/packaging/macos/check-dmg.py" "$MNT" "$ROOT/packaging/macos/dmg-settings.py" \
    "$APP/Contents/Resources/$NAME.icns" || fail "the disk image window does not match packaging/macos/dmg-settings.py"
hdiutil detach "$MNT" >/dev/null
MOUNTED=""
echo "disk image: $NAME.app and Applications -> /Applications, signature valid, window as laid out"

step "check the zip"
UNZIP="$WORK/unzip"
mkdir -p "$UNZIP"
ditto -x -k "$OUT/$ZIP" "$UNZIP"
[ -d "$UNZIP/$NAME.app" ] || fail "the zip holds no $NAME.app"
plutil -lint "$UNZIP/$NAME.app/Contents/Info.plist" >/dev/null || fail "the Info.plist in the zip is not valid"
codesign --verify --strict --all-architectures "$UNZIP/$NAME.app" || fail "the app in the zip has no valid signature"
cmp -s "$UNZIP/$NAME.app/Contents/MacOS/$NAME" "$BIN" || fail "the binary in the zip differs from the built one"
echo "zip: $NAME.app, signature valid"

step "Gatekeeper assessment (an ad-hoc signed app is expected to be rejected)"
if spctl --assess --type execute -vv "$APP" 2>&1; then
    echo "Gatekeeper accepted the app (unexpected for an ad-hoc signature; nothing depends on it)"
else
    echo "Gatekeeper rejected the app, as expected: users choose Open Anyway in System Settings > Privacy & Security once"
fi

# ---------------------------------------------------------------- 10. BUILDINFO and SHA256SUMS
step "BUILDINFO-macOS.txt and SHA256SUMS-macos"
commit=$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo unknown)
{
    echo "$NAME $VERSION (macOS, universal: arm64 + x86_64)"
    echo "commit: $commit"
    echo "SOURCE_DATE_EPOCH: ${SOURCE_DATE_EPOCH:-unset}"
    echo "MACOSX_DEPLOYMENT_TARGET: $MACOSX_DEPLOYMENT_TARGET"
    echo
    rustc -vV
    cargo -V
    xcodebuild -version 2>/dev/null | tr '\n' ' ' | sed 's/ *$//'
    echo
    echo "macOS SDK: $(xcrun --show-sdk-version 2>/dev/null || echo unknown)"
    echo "disk image: $("$PYTHON" -V 2>&1), $DMGBUILD_VERSION"
    echo "$(sw_vers -productName) $(sw_vers -productVersion) ($(sw_vers -buildVersion)), $(uname -m)"
    if [ -n "${ImageOS:-}" ]; then echo "runner image: $ImageOS ${ImageVersion:-}"; fi
} >"$OUT/BUILDINFO-macOS.txt"
cat "$OUT/BUILDINFO-macOS.txt"

(
    cd "$OUT"
    shasum -a 256 "$DMG" "$NAME-macOS.dmg" "$ZIP" "$NAME-macOS.zip" "$NAME.app/Contents/MacOS/$NAME" BUILDINFO-macOS.txt
) >"$OUT/SHA256SUMS-macos"
cat "$OUT/SHA256SUMS-macos"
echo "bundle-macos: done, files in $OUT"
