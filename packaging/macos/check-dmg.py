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

"""Checks the window layout of a mounted Sessionmux disk image.

usage: check-dmg.py <mount-point> <dmg-settings.py> <Sessionmux.icns>

Reads the layout from packaging/macos/dmg-settings.py and compares the volume with it: the app,
the Applications link, the background picture (a TIFF with a 1x and a 2x image), the volume icon
(the app's icon, with the custom icon flag on the volume) and .DS_Store, whose window bounds,
hidden bars, icon view, icon and text size, background picture and icon positions must match.
Prints a short summary; any difference is reported on a line starting with "bundle-macos: " and
the exit code is 1. Needs ds_store and mac_alias (packaging/macos/dmg-requirements.txt).
"""

import os
import struct
import subprocess
import sys

from ds_store import DSStore
from mac_alias import Alias

problems = []


def check(ok, message):
    if not ok:
        problems.append(message)
    return ok


def load_layout(path):
    names = {"defines": {"app": "Sessionmux.app", "icon": "Sessionmux.icns", "background": "dmg-background.png"}}
    with open(path, encoding="utf-8") as f:
        exec(compile(f.read(), path, "exec"), names, names)
    return names


def tiff_sizes(path):
    """Returns the (width, height) of every image in a TIFF file."""
    with open(path, "rb") as f:
        data = f.read()
    if data[:4] not in (b"II*\0", b"MM\0*"):
        return []
    e = "<" if data[:2] == b"II" else ">"
    (offset,) = struct.unpack_from(e + "I", data, 4)
    sizes = []
    seen = set()
    while offset and offset not in seen and offset + 2 <= len(data):
        seen.add(offset)
        (count,) = struct.unpack_from(e + "H", data, offset)
        width = height = None
        for i in range(count):
            tag, kind, _n, raw = struct.unpack_from(e + "HHI4s", data, offset + 2 + 12 * i)
            value = struct.unpack_from(e + ("H" if kind == 3 else "I"), raw)[0]
            if tag == 256:
                width = value
            elif tag == 257:
                height = value
        sizes.append((width, height))
        (offset,) = struct.unpack_from(e + "I", data, offset + 2 + 12 * count)
    return sizes


def finder_flags(path):
    """Returns the Finder flags of a file or folder (0 when it has no Finder info)."""
    out = subprocess.run(
        ["/usr/bin/xattr", "-px", "com.apple.FinderInfo", path], capture_output=True, text=True
    )
    if out.returncode != 0:
        return 0
    raw = bytes.fromhex("".join(out.stdout.split()))
    return struct.unpack_from(">H", raw, 8)[0] if len(raw) >= 10 else 0


def main():
    if len(sys.argv) != 4:
        print("usage: check-dmg.py <mount-point> <dmg-settings.py> <Sessionmux.icns>", file=sys.stderr)
        return 2
    mnt, settings, icns = sys.argv[1:]
    lay = load_layout(settings)
    app, link = lay["APP_NAME"], lay["LINK_NAME"]

    # Files on the volume.
    visible = sorted(n for n in os.listdir(mnt) if not n.startswith("."))
    check(visible == sorted([app, link]), f"the volume shows {visible}, expected only {app} and {link}")
    check(os.path.isdir(os.path.join(mnt, app)), f"{app} is missing")
    check(
        os.path.islink(os.path.join(mnt, link)) and os.readlink(os.path.join(mnt, link)) == "/Applications",
        f"{link} is not a link to /Applications",
    )
    bg_path = os.path.join(mnt, ".background.tiff")
    sizes = tiff_sizes(bg_path) if os.path.isfile(bg_path) else []
    (w, h) = lay["window_rect"][1]
    check(os.path.isfile(bg_path), ".background.tiff is missing")
    check(sorted(sizes) == [(w, h), (2 * w, 2 * h)], f".background.tiff holds images {sizes}, expected {w}x{h} and {2 * w}x{2 * h}")
    vol_icon = os.path.join(mnt, ".VolumeIcon.icns")
    if check(os.path.isfile(vol_icon), ".VolumeIcon.icns is missing"):
        with open(vol_icon, "rb") as a, open(icns, "rb") as b:
            check(a.read() == b.read(), ".VolumeIcon.icns is not the app's icon")
    check(finder_flags(mnt) & 0x0400, "the volume has no custom icon flag, so Finder shows the default disk icon")
    store_path = os.path.join(mnt, ".DS_Store")
    if not check(os.path.isfile(store_path), ".DS_Store is missing"):
        return report(None)

    # Window settings in .DS_Store.
    with DSStore.open(store_path, "r") as d:
        bwsp = d["."]["bwsp"]
        icvp = d["."]["icvp"]
        icvl = d["."]["icvl"]
        app_pos = tuple(d[app]["Iloc"])
        link_pos = tuple(d[link]["Iloc"])

    (x, y), (w, h) = lay["window_rect"]
    bounds = f"{{{{{x}, {y}}}, {{{w}, {h}}}}}"
    check(bwsp.get("WindowBounds") == bounds, f"window bounds are {bwsp.get('WindowBounds')}, expected {bounds}")
    for key in ("ShowToolbar", "ShowSidebar", "ShowStatusBar", "ShowPathbar", "ShowTabView"):
        check(bwsp.get(key) is False, f"{key} is {bwsp.get(key)}, expected False")
    view = icvl[1] if isinstance(icvl, tuple) else icvl
    check(view == b"icnv", f"the window opens in view {view!r}, expected icon view (icnv)")
    check(icvp.get("iconSize") == float(lay["icon_size"]), f"icon size is {icvp.get('iconSize')}, expected {lay['icon_size']}")
    check(icvp.get("textSize") == float(lay["text_size"]), f"text size is {icvp.get('textSize')}, expected {lay['text_size']}")
    check(icvp.get("arrangeBy") == "none", f"icons are arranged by {icvp.get('arrangeBy')}, expected none")
    check(icvp.get("labelOnBottom") is True, "labels are not below the icons")
    check(icvp.get("backgroundType") == 2, f"background type is {icvp.get('backgroundType')}, expected 2 (picture)")
    target = None
    raw_alias = icvp.get("backgroundImageAlias")
    if check(raw_alias is not None, "no background picture is set"):
        try:
            target = Alias.from_bytes(raw_alias).target.filename
        except Exception as e:  # a damaged alias is reported, not raised
            target = f"(unreadable: {e})"
        check(target == ".background.tiff", f"the background picture points to {target}, expected .background.tiff")
    check(app_pos == tuple(lay["APP_POS"]), f"{app} is at {app_pos}, expected {lay['APP_POS']}")
    check(link_pos == tuple(lay["LINK_POS"]), f"{link} is at {link_pos}, expected {lay['LINK_POS']}")
    check(app_pos[0] < link_pos[0] and app_pos[1] == link_pos[1], f"{app} is not left of {link} at the same height")

    return report(
        [
            f"window: {bounds}, icon view, no toolbar, sidebar, path bar or status bar",
            f"icons: {int(icvp.get('iconSize', 0))} pt, labels {int(icvp.get('textSize', 0))} pt; {app} at {app_pos}, {link} at {link_pos}",
            f"background: {target} ({', '.join(f'{a}x{b}' for a, b in sizes)})",
            "volume icon: .VolumeIcon.icns (the app's icon), custom icon flag set",
        ]
    )


def report(summary):
    if problems:
        for p in problems:
            print(f"bundle-macos: disk image window: {p}", file=sys.stderr)
        return 1
    for line in summary or []:
        print(f"disk image window: {line}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
