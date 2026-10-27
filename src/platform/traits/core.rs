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

//! The stateless system concerns: folders and path rules, finding and controlling Claude
//! Desktop, login start, the `claude://` handler, opening links, and small process facts.
//!
//! Each concern is a trait with associated functions only; the backend of the target system
//! implements every one of them for [`Os`]. Callers name the trait they need
//! (`<Os as Processes>::alive(pid)`), so both systems compile against the same contract.

// Some functions are part of the contract before every caller uses them.
#![allow(dead_code)]

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::core::claude_app::{Located, Proc};
use crate::platform::{Pid, WindowId};

/// The running system's backend. Every trait of this module is implemented for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Os;

/// Outcome of bringing an account's Claude window to the front.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FocusResult {
    /// The window is in front now.
    Focused,
    /// The window could not be raised directly; launching the same data folder again makes
    /// Claude show it.
    NeedsHandOff,
    Failed,
}

/// Whether Sessionmux starts when the user logs in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AutostartStatus {
    Off,
    On,
    /// Registered, but the user has to allow it in the system settings first.
    RequiresApproval,
    /// Login start cannot work here (the reason is shown to the user).
    Unavailable(String),
}

impl AutostartStatus {
    /// The name the panel reads (`autostartStatus`).
    pub fn as_str(&self) -> &'static str {
        match self {
            AutostartStatus::Off => "off",
            AutostartStatus::On => "on",
            AutostartStatus::RequiresApproval => "requiresApproval",
            AutostartStatus::Unavailable(_) => "unavailable",
        }
    }
}

/// A system settings page where the user grants Sessionmux a permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivacyPane {
    /// Sending links to Claude (macOS Automation).
    Automation,
    /// Raising other apps' windows (macOS Accessibility).
    Accessibility,
}

/// Why a link could not be delivered to a Claude instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForwardError {
    /// The user has not allowed Sessionmux to send links to Claude.
    NotPermitted,
    Failed(String),
}

impl fmt::Display for ForwardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ForwardError::NotPermitted => f.write_str("not allowed to send links to Claude"),
            ForwardError::Failed(e) => f.write_str(e),
        }
    }
}

impl From<String> for ForwardError {
    fn from(e: String) -> ForwardError {
        ForwardError::Failed(e)
    }
}

/// The program that opens links of a scheme: the open command on Windows, the handler app's path
/// on macOS. Stored as this string in `relay.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandlerId(pub String);

/// Outcome of a handler change. Never asked for on the UI thread: it may wait for a system prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HandlerChange {
    /// The handler now reads as requested.
    Done,
    /// No answer yet (a system prompt is still open, or the system is slow to report the change).
    /// Not a failure: the caller keeps reading `current` and treats a later match as done.
    Pending,
    /// The user chose to keep the other handler. A clean refusal, never asked again in the same
    /// sign-in.
    Declined,
    Failed(String),
}

/// Well-known folders and the system's path comparison rules.
pub trait Paths {
    /// The user's home folder.
    fn home() -> PathBuf;
    /// Per-user settings root: `%APPDATA%` | `~/Library/Application Support`.
    fn config_root() -> PathBuf;
    /// Per-user local data root: `%LOCALAPPDATA%` | `~/Library/Application Support`.
    fn local_root() -> PathBuf;
    /// Where the `Claude-<name>` profile folders live.
    fn profiles_root() -> PathBuf;
    /// Full path of the running executable.
    fn current_exe() -> String;
    /// A path normalised for comparisons. Empty input gives an empty string.
    fn norm_path(p: &str) -> String;
    /// True if two paths name the same location. Empty paths never match.
    fn same_path(a: &str, b: &str) -> bool;
    /// True if `child` is `dir` itself or lies inside it.
    fn is_under(child: &str, dir: &str) -> bool;
}

/// Finding the Claude Desktop installation.
pub trait ClaudeInstall {
    /// Where Claude Desktop is installed and where Main keeps its data (not cached).
    fn locate() -> Result<Located, String>;
}

/// Reading processes.
pub trait Processes {
    /// Running Claude Desktop main processes (helpers excluded), each with its data folder and
    /// main window. `l` is the located installation, if any.
    fn list_claude(l: Option<&Located>) -> Vec<Proc>;
    /// True while `pid` exists (also when it belongs to someone we may not inspect).
    fn alive(pid: Pid) -> bool;
    /// When `pid` started, in microseconds since the Unix epoch; guards against pid reuse.
    fn start_time(pid: Pid) -> Option<u64>;
    fn parent(pid: Pid) -> Option<Pid>;
    /// Full path of the process image.
    fn exe(pid: Pid) -> Option<String>;
    fn argv(pid: Pid) -> Option<Vec<String>>;
    /// Any process whose executable lives under `dir` or whose arguments name it, as
    /// (pid, image name). Read-only.
    fn processes_under(dir: &str) -> Vec<(Pid, String)>;
}

/// Other programs' windows.
pub trait Windows {
    /// The visible top-level window of `pid`, if it has one.
    fn main_window(pid: Pid) -> Option<WindowId>;
    /// The process owning the window the user works in.
    fn foreground_pid() -> Option<Pid>;
}

/// Starting Claude Desktop.
pub trait Launcher {
    /// Start the Main account (no data folder). The pid when the system reports it.
    fn launch_main(l: &Located) -> Result<Option<Pid>, String>;
    /// Start Claude Desktop on `data_dir`. Returns the new pid.
    fn launch(l: &Located, data_dir: &str) -> Result<Pid, String>;
    /// Start Claude Desktop with a link, on `data_dir` (None = Main). Claude hands the link to its
    /// running instance of that folder, if any.
    fn launch_url(l: &Located, data_dir: Option<&str>, url: &str) -> Result<(), String>;
}

/// Bringing a Claude window to the front.
pub trait Focus {
    fn focus(pid: Pid) -> FocusResult;
    /// Whether Sessionmux may raise other apps' windows; with `prompt`, the system is asked to
    /// show its grant prompt when it may not yet. None where no such permission exists.
    fn accessibility_trusted(prompt: bool) -> Option<bool>;
}

/// Closing Claude Desktop.
pub trait Closer {
    /// Ask each process to quit, wait up to 3 s, then end the remaining Claude process trees.
    fn close(pids: &[Pid]) -> Result<(), String>;
}

/// Starting Sessionmux at login.
pub trait Autostart {
    fn status() -> AutostartStatus;
    fn set(enabled: bool) -> Result<AutostartStatus, String>;
    /// Open the system settings page where the user allows login items (no-op where none exists).
    fn open_settings();
}

/// The per-user handler of a link scheme, and delivering links to Claude.
pub trait UrlHandler {
    /// The program that opens `scheme` links now, if any.
    fn current(scheme: &str) -> Option<HandlerId>;
    /// Make Sessionmux the handler of `scheme`.
    fn take(scheme: &str) -> HandlerChange;
    /// Give `scheme` back to `prev`; when nothing usable was saved, to the located Claude; with
    /// neither, leave the handler unset.
    fn restore(scheme: &str, prev: Option<&HandlerId>, l: Option<&Located>) -> HandlerChange;
    /// True when `h` is Sessionmux (this copy or another install of it).
    fn is_us(h: &HandlerId) -> bool;
    /// True where each handler change asks the user. Sign-in routing is off by default there.
    fn changes_need_consent() -> bool;
    /// How often one sign-in takes the handler back after Claude claimed it again: `None` = every
    /// time. Where changes need consent, never.
    fn retake_limit() -> Option<u32>;
    /// Why sign-in routing cannot work in this copy of Sessionmux (shown next to the setting);
    /// None where it can.
    fn unavailable() -> Option<String>;
    /// Deliver a link to the running instance of `data_dir` (None = Main); `pid` is that
    /// instance's main process when known.
    fn forward(l: &Located, data_dir: Option<&str>, pid: Option<Pid>, url: &str) -> Result<(), ForwardError>;
    /// Tell a running Sessionmux that a refused link carried injected arguments, so it plays its
    /// tamper gesture. Never blocks; does nothing when Sessionmux is not running.
    fn signal_bad_link();
}

/// Opening links and folders in the system's own programs.
pub trait Shell {
    /// Open an already validated http(s) link or a local file.
    fn open_url(url: &str) -> Result<(), String>;
    /// Show `path` selected in the file manager.
    fn reveal(path: &Path) -> Result<(), String>;
    /// Open the system settings page where the user grants `pane`; an error where the system has
    /// no such page.
    fn open_privacy_settings(pane: PrivacyPane) -> Result<(), String>;
}

/// The tamper gesture's debugger check.
pub trait Debugger {
    /// True while a debugger is attached to this process.
    fn present() -> bool;
}

/// Small facts the neutral code needs from the system.
pub trait Misc {
    /// The machine's offset from UTC at `now` (Unix seconds), in seconds; 0 when unknown.
    fn local_offset_secs(now: u64) -> i64;
    /// Create a symbolic link at `link` pointing to `target`.
    fn symlink(target: &Path, link: &Path, is_dir: bool) -> io::Result<()>;
    /// True when the error means another process holds the file open.
    fn is_sharing_violation(e: &io::Error) -> bool;
    /// Hand memory pages back to the system after the panel closed.
    fn trim_memory();
    /// True when two spellings name the same executable.
    fn same_exe(a: &str, b: &str) -> bool;
    /// True when this copy runs from a place it cannot stay in (a disk image or a copy the system
    /// moved aside), so nothing may record its path.
    fn translocated() -> bool;
    /// Prepare the calling thread to run the UI, before any window exists. True when
    /// [`Misc::uninit_ui_thread`] has something to undo.
    fn init_ui_thread() -> bool;
    /// Undo [`Misc::init_ui_thread`], on the same thread, after the UI ended.
    fn uninit_ui_thread();
}
