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

//! Open at login on macOS.
//!
//! - The app registers itself as a login item with `SMAppService.mainAppService`. The system may
//!   want the user to allow it in System Settings > General > Login Items first; that is reported
//!   as "requires approval", with a button that opens the page.
//! - When registration fails, a LaunchAgent `~/Library/LaunchAgents/<bundle id>.plist` starts the
//!   app with `--hidden` at the next login instead. It is written atomically and not loaded now.
//! - Turning it off removes both. The status is on while either is in place.
//! - Only a copy that runs as Sessionmux.app from a place it stays in can be started at login.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use objc2_foundation::NSBundle;
use objc2_service_management::{SMAppService, SMAppServiceStatus};

use crate::brand;
use crate::platform::{Autostart, AutostartStatus, Misc, Os, Paths};
use crate::util::log;

/// Escape text for a property list string.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// The LaunchAgent that starts `exe --hidden` when the user logs in to the desktop.
pub(crate) fn launch_agent_plist(label: &str, exe: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>--hidden</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>LimitLoadToSessionType</key>
  <string>Aqua</string>
  <key>ProcessType</key>
  <string>Interactive</string>
</dict>
</plist>
"#,
        label = xml_escape(label),
        exe = xml_escape(exe),
    )
}

/// `~/Library/LaunchAgents`.
fn agents_dir() -> PathBuf {
    <Os as Paths>::home().join("Library").join("LaunchAgents")
}

/// The LaunchAgent file in `dir`.
fn agent_file(dir: &Path) -> PathBuf {
    dir.join(format!("{}.plist", brand::LAUNCH_AGENT_LABEL))
}

/// Write the LaunchAgent for `exe` into `dir` (mode 0644), replacing any older one in one step.
fn write_agent(dir: &Path, exe: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let file = agent_file(dir);
    let tmp = dir.join(format!(".{}.plist.tmp", brand::LAUNCH_AGENT_LABEL));
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(launch_agent_plist(brand::LAUNCH_AGENT_LABEL, exe).as_bytes())?;
        f.sync_all()?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))?;
        std::fs::rename(&tmp, &file)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("could not write {}: {e}", file.display())
    })
}

/// Remove the LaunchAgent from `dir`; a missing one is fine.
fn remove_agent(dir: &Path) -> Result<(), String> {
    let file = agent_file(dir);
    match std::fs::remove_file(&file) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("could not remove {}: {e}", file.display())),
    }
}

fn agent_present(dir: &Path) -> bool {
    agent_file(dir).is_file()
}

/// The program inside this copy's Sessionmux.app; None for a build outside a bundle.
fn bundle_exe() -> Option<String> {
    let main = NSBundle::mainBundle();
    let path = main.bundlePath().to_string();
    let is_app = Path::new(&path).extension().is_some_and(|e| e.eq_ignore_ascii_case("app"));
    if !is_app || main.bundleIdentifier().is_none() {
        return None;
    }
    main.executablePath().map(|p| p.to_string())
}

/// Why this copy cannot be started at login; None when it can.
fn unavailable() -> Option<String> {
    if bundle_exe().is_none() {
        return Some("Open at login needs Sessionmux.app: run Sessionmux.app".into());
    }
    if <Os as Misc>::translocated() {
        return Some("Move Sessionmux to your Applications folder and open it from there".into());
    }
    None
}

/// The login item of the app itself.
fn service() -> objc2::rc::Retained<SMAppService> {
    // SAFETY: returns the shared service object of this app; it has no preconditions.
    unsafe { SMAppService::mainAppService() }
}

fn service_status() -> SMAppServiceStatus {
    // SAFETY: a plain query of the service object.
    unsafe { service().status() }
}

/// Register the app as a login item. Ok when the system now lists it (also when it already did).
fn register() -> Result<(), String> {
    // SAFETY: registering the app's own login item; errors come back as `NSError`.
    match unsafe { service().registerAndReturnError() } {
        Ok(()) => Ok(()),
        Err(e) => {
            let st = service_status();
            if st == SMAppServiceStatus::Enabled || st == SMAppServiceStatus::RequiresApproval {
                return Ok(());
            }
            Err(format!("SMAppService register: code {} ({})", e.code(), e.localizedDescription()))
        }
    }
}

/// Remove the app's login item. Ok when it is no longer registered.
fn unregister() -> Result<(), String> {
    let st = service_status();
    if st != SMAppServiceStatus::Enabled && st != SMAppServiceStatus::RequiresApproval {
        return Ok(());
    }
    // SAFETY: removing the app's own login item; errors come back as `NSError`.
    match unsafe { service().unregisterAndReturnError() } {
        Ok(()) => Ok(()),
        Err(e) => Err(format!("SMAppService unregister: code {} ({})", e.code(), e.localizedDescription())),
    }
}

/// The status from the login item's state, whether the LaunchAgent exists, and why login start
/// cannot work here (if it cannot).
fn combined_status(service: SMAppServiceStatus, agent: bool, unavailable: Option<String>) -> AutostartStatus {
    if service == SMAppServiceStatus::Enabled || agent {
        AutostartStatus::On
    } else if service == SMAppServiceStatus::RequiresApproval {
        AutostartStatus::RequiresApproval
    } else if let Some(why) = unavailable {
        AutostartStatus::Unavailable(why)
    } else {
        AutostartStatus::Off
    }
}

impl Autostart for Os {
    fn status() -> AutostartStatus {
        combined_status(service_status(), agent_present(&agents_dir()), unavailable())
    }

    /// On: the login item, else the LaunchAgent. Off: both removed.
    fn set(enabled: bool) -> Result<AutostartStatus, String> {
        let dir = agents_dir();
        if enabled {
            if let Some(why) = unavailable() {
                return Err(why);
            }
            match register() {
                Ok(()) => {
                    // One way only, so the app is not started twice at login.
                    if let Err(e) = remove_agent(&dir) {
                        log::warn(&format!("autostart: {e}"));
                    }
                    log::info("autostart: registered as a login item");
                }
                Err(e) => {
                    log::warn(&format!("autostart: {e}; using a LaunchAgent instead"));
                    let exe = bundle_exe().ok_or("Sessionmux.app was not found")?;
                    write_agent(&dir, &exe).map_err(|e2| format!("{e}; {e2}"))?;
                    log::info("autostart: LaunchAgent written; it takes effect at the next login");
                }
            }
        } else {
            let errors: Vec<String> = [unregister(), remove_agent(&dir)].into_iter().filter_map(Result::err).collect();
            if !errors.is_empty() {
                let st = Os::status();
                if st == AutostartStatus::On || st == AutostartStatus::RequiresApproval {
                    return Err(errors.join("; "));
                }
                log::warn(&format!("autostart: {}", errors.join("; ")));
            }
            log::info("autostart: off");
        }
        Ok(Os::status())
    }

    /// System Settings > General > Login Items.
    fn open_settings() {
        // SAFETY: opens a System Settings page; no arguments.
        unsafe { SMAppService::openSystemSettingsLoginItems() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_agent_text() {
        let text = launch_agent_plist(
            "io.github.joyal-droid.sessionmux",
            "/Applications/Sessionmux.app/Contents/MacOS/Sessionmux",
        );
        assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist"));
        assert!(text.contains("<key>Label</key>\n  <string>io.github.joyal-droid.sessionmux</string>"));
        assert!(text.contains(
            "<array>\n    <string>/Applications/Sessionmux.app/Contents/MacOS/Sessionmux</string>\n    \
             <string>--hidden</string>\n  </array>"
        ));
        assert!(text.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(text.contains("<key>LimitLoadToSessionType</key>\n  <string>Aqua</string>"));
        assert!(text.contains("<key>ProcessType</key>\n  <string>Interactive</string>"));
        assert!(text.ends_with("</dict>\n</plist>\n"));
    }

    #[test]
    fn launch_agent_text_escapes_paths() {
        let text = launch_agent_plist("a&b", "/Users/x/A <b> & 'c\".app/Contents/MacOS/S");
        assert!(text.contains("<string>a&amp;b</string>"));
        assert!(text.contains("<string>/Users/x/A &lt;b&gt; &amp; &apos;c&quot;.app/Contents/MacOS/S</string>"));
    }

    #[test]
    fn launch_agent_is_valid_for_the_system() {
        let d = tempfile::tempdir().unwrap();
        write_agent(d.path(), "/Applications/Sessionmux.app/Contents/MacOS/Sessionmux").unwrap();
        let out = std::process::Command::new("/usr/bin/plutil").arg("-lint").arg(agent_file(d.path())).output();
        match out {
            Ok(o) => assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout)),
            Err(e) => println!("skipped the plist check: {e}"),
        }
    }

    #[test]
    fn launch_agent_write_and_remove() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("Library").join("LaunchAgents");
        assert!(!agent_present(&dir));
        assert!(remove_agent(&dir).is_ok(), "removing a missing agent is fine");

        write_agent(&dir, "/A/Sessionmux.app/Contents/MacOS/Sessionmux").unwrap();
        let file = agent_file(&dir);
        assert!(agent_present(&dir));
        assert_eq!(file.file_name().unwrap().to_string_lossy(), format!("{}.plist", brand::LAUNCH_AGENT_LABEL));
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
        assert!(std::fs::read_to_string(&file).unwrap().contains("/A/Sessionmux.app/Contents/MacOS/Sessionmux"));

        // Replaced in one step; no temporary file is left.
        write_agent(&dir, "/B/Sessionmux.app/Contents/MacOS/Sessionmux").unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("/B/Sessionmux.app") && !text.contains("/A/Sessionmux.app"));
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "{names:?}");

        remove_agent(&dir).unwrap();
        assert!(!agent_present(&dir));
        assert!(!file.exists());
    }

    #[test]
    fn status_rules() {
        use SMAppServiceStatus as S;
        let why = || Some("why".to_string());
        assert_eq!(combined_status(S::Enabled, false, None), AutostartStatus::On);
        assert_eq!(combined_status(S::NotRegistered, true, None), AutostartStatus::On);
        assert_eq!(combined_status(S::NotFound, true, why()), AutostartStatus::On);
        assert_eq!(combined_status(S::RequiresApproval, false, None), AutostartStatus::RequiresApproval);
        assert_eq!(combined_status(S::NotRegistered, false, None), AutostartStatus::Off);
        assert_eq!(combined_status(S::NotFound, false, None), AutostartStatus::Off);
        assert_eq!(combined_status(S::NotRegistered, false, why()), AutostartStatus::Unavailable("why".into()));
    }

    #[test]
    fn the_system_status_can_be_read() {
        // Registration itself is not asserted: it depends on the machine.
        let _ = service_status();
        let _ = Os::status();
    }

    #[test]
    fn a_build_outside_the_app_is_not_started_at_login() {
        // The test program is not inside Sessionmux.app; nothing is registered or written.
        assert!(bundle_exe().is_none());
        assert!(unavailable().is_some());
        assert!(Os::set(true).is_err());
    }

    #[test]
    fn the_agent_lives_in_the_users_launch_agents() {
        let dir = agents_dir();
        assert!(dir.ends_with("Library/LaunchAgents"));
        assert!(agent_file(&dir).to_string_lossy().ends_with("LaunchAgents/io.github.joyal-droid.sessionmux.plist"));
    }
}
