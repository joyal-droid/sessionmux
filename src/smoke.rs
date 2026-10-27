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

//! `Sessionmux.exe --smoke` — headless self-check that captures screenshots and prints one JSON
//! line.
//!
//! Contract: `pub fn run() -> i32` (process exit code). `app::SMOKE` is set before this runs, so
//! launch / focus / kill / MCP sync / start-with-Windows / shell opens are short-circuited.
//!
//! Steps: attach to the parent console; user data → `<cwd>\smoke\userData`, seeded with a copy of
//! `%APPDATA%\Sessionmux\sessionmux.json` when present (the source is only read) so the real
//! accounts render; full app init (bubble, no tray icon, no hotkey, no poll thread); bubble (icon) →
//! `smoke\bubble.png`; mini tray → `smoke\bubble-tray.png`; panel (off-screen, inactive) →
//! `smoke\panel-<tab>.png` for accounts / sessions / settings, `smoke\panel-about.png` (Settings
//! scrolled to the About card), with the demo fixture also `smoke\panel-approvals.png` (the
//! Sessions tab with two approve requests waiting); RSS with the panel open
//! (Sessionmux + its WebView2 processes) and after destroying it (idle). With
//! `SESSIONMUX_SMOKE_SIGNIN` set, also `smoke\signin-notice.png`:
//! the Accounts tab with a synthetic `signin-changed` event (no relay, no registry). The JSON
//! carries `"signIn": {"active": false}` (smoke never begins a sign-in relay). With
//! `SESSIONMUX_SMOKE_GESTURE=1`, the held frame of the tamper gesture is rendered to
//! `smoke\gesture.png` (no window is shown; the gesture never plays in smoke mode). Prints
//! `{"ok":true,...}` and exits 0, or `{"ok":false,"error"}` and exits 1; a watchdog fails the
//! run after 32 s.
//!
//! Demo fixture (`SESSIONMUX_SMOKE_FIXTURE=1`, used for the README screenshots and in CI): the real
//! store and the real Claude Desktop installation are never read. `smoke\userData\fixture\` is
//! rebuilt from scratch with a Roaming, Local and home folder that replace the real ones inside
//! this process (`paths::set_folders`; the environment of child processes is unchanged), and
//! Claude Desktop discovery returns a fixed demo installation with no running processes. The store holds the accounts Work (the Main profile),
//! Personal and Client with the dark theme; Work has two Code sessions with transcripts, the
//! first of them waiting for a permission, and Client has hit its session limit. Two approve
//! requests (with no hook helper behind them) are added before the approvals capture. The
//! sign-in notice is always captured in this mode.

use crate::app;
use crate::brand;
use crate::platform::{Capture, Footprint, GestureHost, Os};
use crate::util::{log, paths};
use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};

const WATCHDOG: Duration = Duration::from_secs(32);
/// Wait before the approvals capture: the panel's next poll, the arming of Allow and the end of
/// the arrival toast.
const APPROVALS_SETTLE_MS: u64 = 10_800;

/// Run the smoke test; returns the process exit code.
pub fn run() -> i32 {
    <Os as Capture>::attach_console();
    let cwd = std::env::current_dir().unwrap_or_default();
    let out_dir = cwd.join("smoke");
    let user_data = out_dir.join("userData");
    if let Err(e) = std::fs::create_dir_all(&user_data) {
        return fail(&format!("cannot create {}: {e}", user_data.display()));
    }
    let seeded = if fixture_enabled() {
        if let Err(e) = seed_fixture(&user_data) {
            return fail(&format!("demo fixture: {e}"));
        }
        "demo fixture"
    } else {
        // Seed with a copy of the real store (read-only source) so the real accounts render.
        let real_store = paths::app_data().join(brand::DATA_DIR_NAME).join(brand::STORE_FILE);
        if real_store.is_file() && std::fs::copy(&real_store, user_data.join(brand::STORE_FILE)).is_ok() {
            "real store"
        } else {
            "existing store"
        }
    };
    paths::set_user_data(Some(user_data));
    log::init(&paths::log_file());
    log::info(&format!("smoke: start ({seeded})"));

    std::thread::spawn(|| {
        std::thread::sleep(WATCHDOG);
        println!("{}", json!({ "ok": false, "error": format!("smoke timed out after {} s", WATCHDOG.as_secs()) }));
        log::error("smoke timed out");
        std::process::exit(1);
    });

    let code = match steps(&out_dir) {
        Ok(v) => {
            println!("{v}");
            log::info(&format!("smoke ok {v}"));
            0
        }
        Err(e) => fail(&e),
    };
    app::shutdown();
    code
}

fn steps(out_dir: &Path) -> Result<serde_json::Value, String> {
    let t0 = Instant::now();
    app::init(true)?;
    if fixture_enabled() {
        app::smoke_seed_feature_fixture(&demo_sessions(), DEMO_LIMITED_ACCOUNT);
    }
    let (accounts, discovered, loc, procs) = app::smoke_counts().ok_or("app state unavailable")?;
    let rss_start = <Os as Capture>::footprint(false).resident;

    // A previous, interrupted run (or the real store) may be in tray mode.
    if app::smoke_bubble_mode() != "icon" {
        app::smoke_set_settings(json!({ "bubbleMode": "icon" }))?;
    }
    pump(500);
    app::smoke_bubble_png(&out_dir.join("bubble.png"))?;

    app::smoke_set_settings(json!({ "bubbleMode": "tray" }))?;
    pump(700);
    let (r, scale) = app::smoke_bubble_rect().ok_or("no bubble window")?;
    let tray_window = json!({ "width": r.w, "height": r.h });
    let n = app::smoke_tray_accounts() as i32;
    let dip_w = 16 + 14 + n * 44 + (n - 1) * 6 + 8 + 32 + 16;
    let tray_expected =
        json!({ "width": (dip_w as f32 * scale).round() as i32, "height": (72.0 * scale).round() as i32 });
    if tray_window != tray_expected {
        log::warn(&format!("smoke: tray window is {tray_window}, expected {tray_expected}"));
    }
    app::smoke_bubble_png(&out_dir.join("bubble-tray.png"))?;
    app::smoke_set_settings(json!({ "bubbleMode": "icon" }))?;
    pump(300);
    let gesture = env_flag("SESSIONMUX_SMOKE_GESTURE");
    if gesture {
        <Os as GestureHost>::render_to_png(&out_dir.join("gesture.png"))?;
    }

    // Panel: create, wait for __ready, show off-screen without activation, capture each tab.
    let tp = Instant::now();
    app::smoke_show_panel_offscreen()?;
    let deadline = Instant::now() + Duration::from_secs(8);
    while !app::smoke_panel_ready() && Instant::now() < deadline {
        pump(25);
    }
    if !app::smoke_panel_ready() {
        return Err("panel never reported __ready".into());
    }
    let ready_ms = tp.elapsed().as_millis() as u64;
    let panel = app::smoke_panel_window().ok_or("no panel window")?;
    let capture = |path: &Path| <Os as Capture>::panel_png(panel, path);
    // Off-screen, Chromium only produces frames when asked: a throw-away capture starts the
    // card-in entrance animation, which has finished by the next capture.
    let _ = capture(&std::env::temp_dir().join("sessionmux-smoke-warmup.png"));
    pump(700);
    for tab in app::smoke_tabs() {
        app::smoke_show_tab(tab);
        pump(if tab == "sessions" { 1200 } else { 800 });
        capture(&out_dir.join(format!("panel-{tab}.png")))?;
    }
    // Settings scrolled to the About card (version, links, license attribution).
    app::smoke_show_tab("about");
    pump(500);
    capture(&out_dir.join("panel-about.png"))?;
    // Opt-in renderer check for the sign-in notice card: a synthetic page event
    // only — no relay begins and the registry is never touched.
    if fixture_enabled() || std::env::var_os("SESSIONMUX_SMOKE_SIGNIN").is_some() {
        let (id, name) = app::smoke_extra_account().unwrap_or_else(|| ("smoke".into(), "Work".into()));
        app::smoke_show_tab("accounts");
        app::smoke_emit_sign_in(Some(json!({
            "active": true, "accountId": id, "accountName": name, "since": crate::util::date::iso_now(),
        })));
        pump(400);
        capture(&out_dir.join("signin-notice.png"))?;
        app::smoke_emit_sign_in(None);
        pump(200);
    }
    // The Sessions tab once two approve requests wait: the panel picks them up on its next poll
    // (every 2 s), arms Allow 600 ms after a card appears and shows an arrival toast for 8 s,
    // which has gone by the capture.
    if fixture_enabled() {
        app::smoke_seed_approvals(&demo_sessions());
        app::smoke_show_tab("sessions");
        pump(APPROVALS_SETTLE_MS);
        capture(&out_dir.join("panel-approvals.png"))?;
    }
    let Footprint { resident: rss_panel, private: private_panel, helpers: webview_procs } =
        <Os as Capture>::footprint(true);

    app::smoke_destroy_panel();
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        pump(200);
        if <Os as Capture>::footprint(true).helpers == 0 || Instant::now() >= deadline {
            break;
        }
    }
    // Settle as the idle app would: a refresh (accounts, bubble) after the trim, then measure.
    let _ = app::smoke_counts();
    pump(1000);
    let webview_after = <Os as Capture>::footprint(true).helpers;
    let Footprint { resident: rss_idle, private: private_idle, .. } = <Os as Capture>::footprint(false);
    let page_errors = app::smoke_page_errors();
    if !page_errors.is_empty() {
        log::warn(&format!("smoke: page errors: {page_errors:?}"));
    }

    let mb = |b: u64| ((b as f64 / (1024.0 * 1024.0)) * 10.0).round() / 10.0;
    Ok(json!({
        "ok": true,
        "accounts": accounts,
        "discovered": discovered,
        "locate": { "kind": loc.as_ref().map(|l| l.0.clone()), "exe": loc.as_ref().map(|l| l.1.clone()) },
        "procs": procs,
        "bubbleMode": app::smoke_bubble_mode(),
        "trayWindow": tray_window,
        "trayExpected": tray_expected,
        "trayAccounts": n,
        "panelReadyMs": ready_ms,
        "pageErrors": page_errors.len(),
        "signIn": app::smoke_sign_in(),
        "approvals": app::smoke_approvals(),
        "gesture": gesture,
        "rssMb": mb(rss_start),
        "rssPanelMb": mb(rss_panel),
        "privatePanelMb": mb(private_panel),
        "webviewProcs": webview_procs,
        "rssIdleMb": mb(rss_idle),
        "privateIdleMb": mb(private_idle),
        "webviewProcsAfterDestroy": webview_after,
        "elapsedMs": t0.elapsed().as_millis() as u64,
    }))
}

/// True when `SESSIONMUX_SMOKE_FIXTURE` is set to anything but "" or "0".
fn fixture_enabled() -> bool {
    env_flag("SESSIONMUX_SMOKE_FIXTURE")
}

/// True when the environment variable `name` is set to anything but "" or "0".
fn env_flag(name: &str) -> bool {
    std::env::var(name).map(|v| !v.is_empty() && v != "0").unwrap_or(false)
}

/// Demo account uuids (`lastKnownAccountUuid`) and the demo organisation.
const DEMO_WORK_UUID: &str = "00000000-0000-4000-8000-000000000001";
const DEMO_PERSONAL_UUID: &str = "00000000-0000-4000-8000-000000000002";
const DEMO_CLIENT_UUID: &str = "00000000-0000-4000-8000-000000000003";
const DEMO_ORG_UUID: &str = "00000000-0000-4000-8000-0000000000a1";

/// The demo account that has hit its plan limit (Client).
const DEMO_LIMITED_ACCOUNT: &str = "00000000-0000-4000-9000-000000000003";

/// The demo sessions as the app's session index holds them (Work is the Main account).
fn demo_sessions() -> Vec<app::SmokeSession<'static>> {
    DEMO_SESSIONS.iter().map(|(id, cli, title, ..)| ("main", *id, *cli, *title)).collect()
}

/// Demo Code sessions of the Work account: (record id, CLI session id, title, project folder,
/// model, minutes since the last activity).
const DEMO_SESSIONS: [(&str, &str, &str, &str, &str, u64); 2] = [
    (
        "local_1b7c2d4e-5f60-4a71-8b92-a3b4c5d6e7f8",
        "2c8d3e5f-6071-4b82-9ca3-b4c5d6e7f809",
        "Landing page redesign",
        "landing-page",
        "claude-sonnet-4-5",
        7,
    ),
    (
        "local_3d9e4f60-7182-4c93-ad04-c5d6e7f8091a",
        "4e0f5071-8293-4da4-be15-d6e7f8091a2b",
        "Quarterly report",
        "quarterly-report",
        "claude-haiku-4-5",
        26 * 60,
    ),
];

fn mkdir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))
}

fn write_text(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        mkdir(dir)?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn write_json(path: &Path, v: &serde_json::Value) -> Result<(), String> {
    write_text(path, &serde_json::to_string_pretty(v).map_err(|e| e.to_string())?)
}

/// Build the demo data under `<user_data>\fixture\` and the demo store, then point this process
/// at it (see the module docs). Nothing outside `user_data` is read or written.
fn seed_fixture(user_data: &Path) -> Result<(), String> {
    let root = user_data.join("fixture");
    let roaming = root.join("Roaming");
    let local = root.join("Local");
    let home = root.join("Home");
    let projects = root.join("Projects");
    // Start from scratch; a scanner or indexer may hold a file of the previous run for a moment.
    let mut attempt = 0;
    while root.exists() {
        match std::fs::remove_dir_all(&root) {
            Ok(()) => break,
            Err(e) if attempt >= 10 => return Err(format!("{}: {e}", root.display())),
            Err(_) => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
    for d in [&roaming, &local, &home, &projects] {
        mkdir(d)?;
    }
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let work_dir = roaming.join("Claude");
    let personal_dir = roaming.join("Claude-Personal");
    let client_dir = roaming.join("Claude-Client");
    for (dir, uuid) in
        [(&work_dir, DEMO_WORK_UUID), (&personal_dir, DEMO_PERSONAL_UUID), (&client_dir, DEMO_CLIENT_UUID)]
    {
        write_json(&dir.join("config.json"), &json!({ "lastKnownAccountUuid": uuid, "oauth:tokenCache": "demo" }))?;
        mkdir(&dir.join("claude-code-sessions").join(uuid).join(DEMO_ORG_UUID))?;
    }

    let now_ms =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let records = work_dir.join("claude-code-sessions").join(DEMO_WORK_UUID).join(DEMO_ORG_UUID);
    for (id, cli, title, folder, model, minutes) in DEMO_SESSIONS {
        let cwd = projects.join(folder);
        mkdir(&cwd)?;
        let last = now_ms.saturating_sub(minutes * 60_000);
        write_json(
            &records.join(format!("{id}.json")),
            &json!({
                "sessionId": id, "cliSessionId": cli, "cwd": s(&cwd), "originCwd": s(&cwd), "title": title,
                "lastActivityAt": last, "createdAt": last.saturating_sub(45 * 60_000), "model": model,
                "effort": "medium", "isArchived": false, "completedTurns": 6,
            }),
        )?;
        let transcript = home.join(".claude").join("projects").join(paths::slug(&s(&cwd))).join(format!("{cli}.jsonl"));
        let line = json!({
            "type": "user", "sessionId": cli, "cwd": s(&cwd), "timestamp": crate::util::date::iso_now(),
            "message": { "role": "user", "content": title },
        });
        write_text(&transcript, &format!("{line}\n"))?;
    }

    write_json(
        &user_data.join(brand::STORE_FILE),
        &json!({
            "version": 2,
            "accounts": [
                { "id": "main", "name": "Work", "dataDir": null, "color": "#5B9CF6", "label": null, "order": 0 },
                { "id": "00000000-0000-4000-9000-000000000002", "name": "Personal", "dataDir": s(&personal_dir),
                  "color": "#F08A5D", "label": null, "order": 1 },
                { "id": "00000000-0000-4000-9000-000000000003", "name": "Client", "dataDir": s(&client_dir),
                  "color": "#3DD6A1", "label": null, "order": 2 },
            ],
            "settings": {
                "startWithWindows": false, "shareMcpConfig": true, "showBubble": true, "bubblePosition": null,
                "theme": "dark", "hotkey": "Ctrl+Shift+Space", "panelSide": "auto", "bubbleMode": "tray",
                "signInRouting": true,
            },
            "ignoredDataDirs": [],
        }),
    )?;

    // From here on the well-known folders resolve inside the fixture. The environment is left
    // alone, so the WebView2 processes keep the real profile folders for their own caches.
    paths::set_folders(Some((home, roaming, local.clone())));
    let install_dir = local.join("AnthropicClaude");
    crate::core::claude_app::set_fixture(Some(crate::core::claude_app::Located {
        kind: "installer",
        exe: s(&install_dir.join("claude.exe")),
        install_dir: s(&install_dir),
        aumid: None,
        family: None,
        main_data_dir: s(&work_dir),
        main_config_candidates: Vec::new(),
    }));
    Ok(())
}

fn fail(error: &str) -> i32 {
    log::error(error);
    println!("{}", json!({ "ok": false, "error": error }));
    1
}

/// Run UI events for `ms` milliseconds.
fn pump(ms: u64) {
    <Os as Capture>::pump(ms);
}
