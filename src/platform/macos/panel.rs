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

//! The panel window. Not built yet: creating it fails with a clear message, so no web view is
//! ever built on macOS in this version.

use std::sync::Arc;

use raw_window_handle::{HandleError, HasWindowHandle, WindowHandle};
use wry::{WebView, WebViewBuilder};

use super::unsupported;
use crate::platform::{PanelHost, Rect, WindowId};
use crate::ui::ipc::IpcHandler;

/// The panel window (never created in this version).
pub struct PanelWindow {
    id: WindowId,
}

impl HasWindowHandle for PanelWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        Err(HandleError::Unavailable)
    }
}

impl PanelHost for PanelWindow {
    const ENGINE: &'static str = "WKWebView";

    fn create(_ipc: Arc<dyn IpcHandler>, _near: Option<(Rect, &str)>) -> Result<Self, String> {
        Err(unsupported("The panel"))
    }

    fn configure(builder: WebViewBuilder<'_>) -> WebViewBuilder<'_> {
        builder
    }

    fn after_build(&self, _webview: &WebView) {}

    fn show_near(&self, _anchor: Rect, _side: &str) {}

    fn show_inactive_at(&self, _x: i32, _y: i32) {}

    fn activate(&self) {}

    fn hide(&self) {}

    fn is_visible(&self) -> bool {
        false
    }

    fn window(&self) -> WindowId {
        self.id
    }

    fn destroy(self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::ipc::IpcRequest;

    struct Nothing;

    impl IpcHandler for Nothing {
        fn handle(&self, _req: IpcRequest) {}
    }

    #[test]
    fn creation_fails_cleanly() {
        assert!(PanelWindow::create(Arc::new(Nothing), None).is_err());
        let w = PanelWindow { id: WindowId(0) };
        assert!(w.window_handle().is_err());
        assert!(!w.is_visible());
        assert_eq!(w.window(), WindowId(0));
        w.destroy();
    }
}
