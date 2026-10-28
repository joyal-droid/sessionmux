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

//! Opening links, files and System Settings pages, and showing files in Finder, through
//! `NSWorkspace`. Requests are checked on the calling thread; the `NSWorkspace` call itself
//! always runs on the main thread.

use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSArray, NSString, NSURL};

use crate::platform::{EventLoop, Os, PrivacyPane, Shell};
use crate::util::log;

/// What [`Shell::open_url`] may open.
#[derive(Debug, PartialEq, Eq)]
enum Target {
    /// An http(s) link, opened in the default browser.
    Web(String),
    /// A local file, opened in its default app.
    File(PathBuf),
}

/// Sort a request into a link or a file; anything else (other schemes, relative paths) is refused.
fn target(url: &str) -> Result<Target, String> {
    let u = url.trim();
    let lower = u.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        return Ok(Target::Web(u.to_string()));
    }
    let p = Path::new(u);
    if p.is_absolute() {
        return Ok(Target::File(p.to_path_buf()));
    }
    Err("only web links and local files can be opened".into())
}

/// The System Settings page for `pane` (Privacy & Security).
fn privacy_url(pane: PrivacyPane) -> &'static str {
    match pane {
        PrivacyPane::Automation => "x-apple.systempreferences:com.apple.preference.security?Privacy_Automation",
        PrivacyPane::Accessibility => "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
    }
}

fn file_url(path: &Path) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
}

/// Run `f` on the main thread: now when called there, else posted to it. A failure after a post
/// is logged, since the caller has already been answered.
fn on_main(f: impl FnOnce() -> Result<(), String> + Send + 'static) -> Result<(), String> {
    if MainThreadMarker::new().is_some() {
        return f();
    }
    <Os as EventLoop>::post(Box::new(move || {
        if let Err(e) = f() {
            log::warn(&format!("shell: {e}"));
        }
    }));
    Ok(())
}

/// A link for `NSURL`; checked on the calling thread so a bad one is reported to the caller.
fn web_url(u: &str, invalid: &str) -> Result<String, String> {
    match NSURL::URLWithString(&NSString::from_str(u)) {
        Some(_) => Ok(u.to_string()),
        None => Err(invalid.to_string()),
    }
}

/// Open `url` with the app the system picks for it. Main thread only.
fn open(url: &NSURL, what: &str) -> Result<(), String> {
    if NSWorkspace::sharedWorkspace().openURL(url) {
        Ok(())
    } else {
        Err(format!("NSWorkspace openURL: {what} could not be opened"))
    }
}

/// Open the link `u` (already checked by [`web_url`]) on the main thread.
fn open_web(u: String, what: &'static str) -> Result<(), String> {
    on_main(move || {
        let ns = NSURL::URLWithString(&NSString::from_str(&u)).ok_or("the link is not a valid URL")?;
        open(&ns, what)
    })
}

impl Shell for Os {
    fn open_url(url: &str) -> Result<(), String> {
        match target(url)? {
            Target::Web(u) => open_web(web_url(&u, "the link is not a valid URL")?, "the link"),
            Target::File(p) => {
                if !p.exists() {
                    return Err(format!("{} does not exist", p.display()));
                }
                on_main(move || open(&file_url(&p), "the file"))
            }
        }
    }

    /// Show `path` selected in a Finder window.
    fn reveal(path: &Path) -> Result<(), String> {
        if !path.is_absolute() || !path.exists() {
            return Err(format!("{} does not exist", path.display()));
        }
        let path = path.to_path_buf();
        on_main(move || {
            let urls = NSArray::from_retained_slice(&[file_url(&path)]);
            NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&urls);
            Ok(())
        })
    }

    fn open_privacy_settings(pane: PrivacyPane) -> Result<(), String> {
        open_web(web_url(privacy_url(pane), "System Settings link")?, "System Settings")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets() {
        assert_eq!(target("https://example.com/a"), Ok(Target::Web("https://example.com/a".into())));
        assert_eq!(target(" HTTP://example.com/ "), Ok(Target::Web("HTTP://example.com/".into())));
        assert_eq!(
            target("/Applications/Sessionmux.app/LICENSE.txt"),
            Ok(Target::File("/Applications/Sessionmux.app/LICENSE.txt".into()))
        );
        for bad in [
            "",
            "file:///etc/passwd",
            "claude://x",
            "javascript:alert(1)",
            "LICENSE.txt",
            "x-apple.systempreferences:x",
        ] {
            assert!(target(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn privacy_pages() {
        assert!(privacy_url(PrivacyPane::Automation).ends_with("?Privacy_Automation"));
        assert!(privacy_url(PrivacyPane::Accessibility).ends_with("?Privacy_Accessibility"));
        for p in [PrivacyPane::Automation, PrivacyPane::Accessibility] {
            assert!(NSURL::URLWithString(&NSString::from_str(privacy_url(p))).is_some());
        }
    }

    #[test]
    fn missing_files_are_refused_without_opening_anything() {
        assert!(Os::open_url("/nonexistent/sessionmux-test/file.txt").is_err());
        assert!(Os::reveal(Path::new("/nonexistent/sessionmux-test")).is_err());
        assert!(Os::reveal(Path::new("relative")).is_err());
        assert!(Os::open_url("ftp://example.com/").is_err());
    }
}
