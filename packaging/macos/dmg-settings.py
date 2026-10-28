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

# Layout of the disk image window, read by dmgbuild (scripts/bundle-macos.sh) and by
# packaging/macos/check-dmg.py, which checks the finished disk image against it.
#
# The window is 660 x 400 points with no toolbar, sidebar, path bar or status bar, in icon view
# with 128-point icons and 13-point labels. Sessionmux.app sits on the left and the link to
# /Applications on the right, at the same height; dmg-background.png (and its @2x twin, joined
# into one HiDPI TIFF while the image is built) draws the arrow between them, a mid-grey plate
# under each label and the line "Drag Sessionmux to Applications to install". Icon positions are
# the centres of the icons, in points from the top left of the window's content.
#
# Labels: Finder shows a window that has a background picture in its light appearance, so it
# draws the labels in black in both light and dark mode, and they cannot be given another colour.
# The plates under them are #767676, on which black and white text both reach a contrast of 4.5:1,
# so the names stay readable even where Finder draws them in white. The background itself is
# white, the same as an empty Finder window, so a window drawn a little larger than the picture
# shows no edge.
#
# dmgbuild passes the values given with -D on its command line in `defines`:
#   app         path of the signed Sessionmux.app
#   icon        path of the volume icon (the app's Sessionmux.icns)
#   background  path of dmg-background.png

APP_NAME = "Sessionmux.app"
LINK_NAME = "Applications"
APP_POS = (165, 148)
LINK_POS = (495, 148)

files = [(defines["app"], APP_NAME)]  # noqa: F821 (provided by dmgbuild)
symlinks = {LINK_NAME: "/Applications"}
background = defines["background"]  # noqa: F821
VOLUME_ICON = defines["icon"]  # noqa: F821

format = "UDZO"
filesystem = "HFS+"

window_rect = ((200, 120), (660, 400))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
show_icon_preview = False
show_item_info = False
include_icon_view_settings = True
include_list_view_settings = False
arrange_by = None
label_pos = "bottom"
icon_size = 128
text_size = 13

icon_locations = {APP_NAME: APP_POS, LINK_NAME: LINK_POS}


def create_hook(mount_point, options):
    """Gives the volume the app's icon: .VolumeIcon.icns plus the volume's custom icon flag.

    SetFile sets the flag where the developer tools provide it; otherwise the flag (0x0400 in the
    Finder flags, bytes 8-9 of the Finder info) is written directly.
    """
    import os
    import shutil
    import subprocess

    shutil.copyfile(VOLUME_ICON, os.path.join(mount_point, ".VolumeIcon.icns"))
    setfile = "/usr/bin/SetFile"
    if os.path.exists(setfile) and subprocess.call([setfile, "-a", "C", mount_point]) == 0:
        return
    info = bytearray(32)
    info[8] = 0x04
    subprocess.check_call(["/usr/bin/xattr", "-wx", "com.apple.FinderInfo", info.hex(), mount_point])
