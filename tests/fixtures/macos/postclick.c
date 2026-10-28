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
 *
 * Posts one real left click at a point on the screen, as a mouse would, for the focus check
 * (scripts/macos-focus-check.sh).
 *
 * Usage: postclick <x> <y>   (points from the top left of the primary display)
 *
 * Moves the pointer there first, then presses and releases the left button. Prints one JSON line:
 * {"posted":true|false,"detail":"..."}. "posted" is false only when an event could not be made
 * (exit code 1). The system may still drop posted events silently when the process that started
 * this one may not post input events; "detail" then says "allowed=false", and the receiving app
 * has to tell whether the click arrived.
 */

#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

#include <ApplicationServices/ApplicationServices.h>

static bool post(CGEventType type, CGPoint at) {
    CGEventRef ev = CGEventCreateMouseEvent(NULL, type, at, kCGMouseButtonLeft);
    if (ev == NULL) {
        return false;
    }
    CGEventPost(kCGHIDEventTap, ev);
    CFRelease(ev);
    return true;
}

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: postclick <x> <y>\n");
        return 2;
    }
    CGPoint at = CGPointMake(strtod(argv[1], NULL), strtod(argv[2], NULL));
    const char *allowed = CGPreflightPostEventAccess() ? "true" : "false";
    bool ok = post(kCGEventMouseMoved, at);
    usleep(250000);
    ok = ok && post(kCGEventLeftMouseDown, at);
    usleep(80000);
    ok = ok && post(kCGEventLeftMouseUp, at);
    if (!ok) {
        printf("{\"posted\":false,\"detail\":\"a mouse event could not be made (allowed=%s)\"}\n", allowed);
        return 1;
    }
    printf("{\"posted\":true,\"detail\":\"left click at %.0f,%.0f (allowed=%s)\"}\n", at.x, at.y, allowed);
    return 0;
}
