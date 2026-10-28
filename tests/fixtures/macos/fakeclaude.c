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
 * A stand-in for Claude Desktop, built into a temporary Claude.app by the macOS tests.
 *
 * Main process (no --type= argument):
 * - writes its arguments, one per line, to <folder of Claude.app>/argv-<pid>.txt;
 * - starts two helper processes with --type=renderer (and the same --user-data-dir=);
 * - runs as a regular app with one window; starts without a window when its data folder holds a
 *   file named "start-windowless";
 * - when asked to show itself (the reopen Apple Event, as a click on its Dock icon sends), appends
 *   a line to <folder of Claude.app>/reopen-<pid>.txt, opens its window again (creating it when
 *   there is none) and activates itself;
 * - appends every link it is asked to open to <folder of Claude.app>/url-<pid>.txt;
 * - quits when asked to (Quit Apple Event), and 0.5 s after SIGTERM, ending its helpers;
 * - ignores both when its data folder holds a file named "ignore-quit";
 * - also starts its own executable as a tool (with --sessionmux-test-tool and no --type=) when its
 *   data folder holds a file named "spawn-tool".
 * Helper and tool processes: wait until their parent is gone, then exit.
 */

#include <limits.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

#include <CoreGraphics/CGGeometry.h>
#include <dispatch/dispatch.h>
#include <mach-o/dyld.h>
#include <objc/message.h>
#include <objc/runtime.h>

#define UDD "--user-data-dir="
#define TOOL_FLAG "--sessionmux-test-tool"
#define TERMINATE_CANCEL 0UL
#define TERMINATE_NOW 1UL

static char g_root[PATH_MAX];
static int g_ignore_quit;
static pid_t g_helpers[3];
static int g_nhelpers;
static id g_win;

static id msg0(id obj, const char *sel) {
    return ((id (*)(id, SEL))objc_msgSend)(obj, sel_registerName(sel));
}

/* Drop the last `levels` parts of an absolute path. */
static void parent_dir(char *p, int levels) {
    while (levels-- > 0) {
        char *s = strrchr(p, '/');
        if (s == NULL) {
            return;
        }
        if (s == p) {
            p[1] = '\0';
            return;
        }
        *s = '\0';
    }
}

/* Resolve the own executable into `exe` (PATH_MAX bytes) and the folder that holds Claude.app,
 * four levels above .../Claude.app/Contents/MacOS/Claude, into g_root. */
static int find_root(char *exe) {
    char buf[PATH_MAX];
    uint32_t n = sizeof buf;
    if (_NSGetExecutablePath(buf, &n) != 0 || realpath(buf, exe) == NULL) {
        return -1;
    }
    strncpy(g_root, exe, sizeof g_root - 1);
    parent_dir(g_root, 4);
    return 0;
}

static const char *udd_arg(int argc, char **argv) {
    for (int i = 1; i < argc; i++) {
        if (strncmp(argv[i], UDD, strlen(UDD)) == 0) {
            return argv[i];
        }
    }
    return NULL;
}

static int is_helper(int argc, char **argv) {
    for (int i = 1; i < argc; i++) {
        if (strncmp(argv[i], "--type=", 7) == 0 || strcmp(argv[i], TOOL_FLAG) == 0) {
            return 1;
        }
    }
    return 0;
}

/* True when the data folder named by `udd` (or NULL) holds a file called `name`. */
static int has_marker(const char *udd, const char *name) {
    if (udd == NULL) {
        return 0;
    }
    char marker[PATH_MAX];
    struct stat st;
    snprintf(marker, sizeof marker, "%s/%s", udd + strlen(UDD), name);
    return stat(marker, &st) == 0;
}

static void record_args(int argc, char **argv) {
    char path[PATH_MAX], tmp[PATH_MAX + 8];
    snprintf(path, sizeof path, "%s/argv-%d.txt", g_root, (int)getpid());
    snprintf(tmp, sizeof tmp, "%s.tmp", path);
    FILE *f = fopen(tmp, "w");
    if (f == NULL) {
        return;
    }
    for (int i = 0; i < argc; i++) {
        fprintf(f, "%s\n", argv[i]);
    }
    fclose(f);
    rename(tmp, path);
}

static void record_url(const char *url) {
    char path[PATH_MAX];
    snprintf(path, sizeof path, "%s/url-%d.txt", g_root, (int)getpid());
    FILE *f = fopen(path, "a");
    if (f == NULL) {
        return;
    }
    fprintf(f, "%s\n", url);
    fclose(f);
}

static void spawn_helpers(const char *exe, const char *udd, int with_tool) {
    for (int i = 0; i < 2 + with_tool; i++) {
        pid_t p = fork();
        if (p == 0) {
            char *args[4];
            int n = 0;
            args[n++] = (char *)exe;
            args[n++] = i < 2 ? "--type=renderer" : TOOL_FLAG;
            if (udd != NULL) {
                args[n++] = (char *)udd;
            }
            args[n] = NULL;
            execv(exe, args);
            _exit(127);
        }
        if (p > 0) {
            g_helpers[g_nhelpers++] = p;
        }
    }
}

static void stop_helpers(void) {
    for (int i = 0; i < g_nhelpers; i++) {
        kill(g_helpers[i], SIGTERM);
    }
}

static int run_helper(void) {
    for (;;) {
        if (getppid() == 1) {
            return 0;
        }
        usleep(100 * 1000);
    }
}

static void quit_now(void *ctx) {
    (void)ctx;
    stop_helpers();
    exit(0);
}

static void on_sigterm(void *ctx) {
    (void)ctx;
    if (g_ignore_quit) {
        return;
    }
    dispatch_after_f(dispatch_time(DISPATCH_TIME_NOW, 500 * NSEC_PER_MSEC), dispatch_get_main_queue(), NULL, quit_now);
}

static unsigned long should_terminate(id self, SEL cmd, id sender) {
    (void)self;
    (void)cmd;
    (void)sender;
    if (g_ignore_quit) {
        return TERMINATE_CANCEL;
    }
    stop_helpers();
    return TERMINATE_NOW;
}

static void open_urls(id self, SEL cmd, id app, id urls) {
    (void)self;
    (void)cmd;
    (void)app;
    unsigned long n = ((unsigned long (*)(id, SEL))objc_msgSend)(urls, sel_registerName("count"));
    for (unsigned long i = 0; i < n; i++) {
        id url = ((id (*)(id, SEL, unsigned long))objc_msgSend)(urls, sel_registerName("objectAtIndex:"), i);
        id s = msg0(url, "absoluteString");
        const char *c = ((const char *(*)(id, SEL))objc_msgSend)(s, sel_registerName("UTF8String"));
        if (c != NULL) {
            record_url(c);
        }
    }
}

static void record_reopen(void) {
    char path[PATH_MAX];
    snprintf(path, sizeof path, "%s/reopen-%d.txt", g_root, (int)getpid());
    FILE *f = fopen(path, "a");
    if (f == NULL) {
        return;
    }
    fprintf(f, "reopen\n");
    fclose(f);
}

static id make_window(void) {
    CGRect frame = CGRectMake(120, 120, 480, 360);
    id win = msg0((id)objc_getClass("NSWindow"), "alloc");
    win = ((id (*)(id, SEL, CGRect, unsigned long, unsigned long, BOOL))objc_msgSend)(
        win, sel_registerName("initWithContentRect:styleMask:backing:defer:"), frame, 15UL, 2UL, NO);
    ((void (*)(id, SEL, BOOL))objc_msgSend)(win, sel_registerName("setReleasedWhenClosed:"), NO);
    return win;
}

static BOOL reopen(id self, SEL cmd, id app, BOOL visible) {
    (void)self;
    (void)cmd;
    (void)visible;
    record_reopen();
    if (g_win == nil) {
        g_win = make_window();
    }
    ((void (*)(id, SEL, id))objc_msgSend)(g_win, sel_registerName("makeKeyAndOrderFront:"), nil);
    ((void (*)(id, SEL, BOOL))objc_msgSend)(app, sel_registerName("activateIgnoringOtherApps:"), YES);
    return NO;
}

int main(int argc, char **argv) {
    if (is_helper(argc, argv)) {
        return run_helper();
    }
    char exe[PATH_MAX];
    if (find_root(exe) != 0) {
        return 2;
    }
    signal(SIGCHLD, SIG_IGN); /* ended helpers are reaped at once */
    record_args(argc, argv);

    const char *udd = udd_arg(argc, argv);
    g_ignore_quit = has_marker(udd, "ignore-quit");
    spawn_helpers(exe, udd, has_marker(udd, "spawn-tool"));

    signal(SIGTERM, SIG_IGN);
    dispatch_source_t term = dispatch_source_create(DISPATCH_SOURCE_TYPE_SIGNAL, SIGTERM, 0, dispatch_get_main_queue());
    dispatch_source_set_event_handler_f(term, on_sigterm);
    dispatch_resume(term);

    Class cls = objc_allocateClassPair(objc_getClass("NSObject"), "FakeClaudeDelegate", 0);
    class_addMethod(cls, sel_registerName("applicationShouldTerminate:"), (IMP)should_terminate, "Q@:@");
    class_addMethod(cls, sel_registerName("application:openURLs:"), (IMP)open_urls, "v@:@@");
    class_addMethod(cls, sel_registerName("applicationShouldHandleReopen:hasVisibleWindows:"), (IMP)reopen, "c@:@c");
    objc_registerClassPair(cls);
    id delegate = msg0(msg0((id)cls, "alloc"), "init");

    id app = msg0((id)objc_getClass("NSApplication"), "sharedApplication");
    ((void (*)(id, SEL, long))objc_msgSend)(app, sel_registerName("setActivationPolicy:"), 0L);
    ((void (*)(id, SEL, id))objc_msgSend)(app, sel_registerName("setDelegate:"), delegate);

    if (!has_marker(udd, "start-windowless")) {
        g_win = make_window();
        ((void (*)(id, SEL, id))objc_msgSend)(g_win, sel_registerName("makeKeyAndOrderFront:"), nil);
    }

    ((void (*)(id, SEL))objc_msgSend)(app, sel_registerName("run"));
    return 0;
}
