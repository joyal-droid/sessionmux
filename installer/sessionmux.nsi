; Copyright (c) 2026 joyal-droid. SPDX-License-Identifier: CPAL-1.0
;
; The contents of this file are subject to the Common Public Attribution License Version 1.0
; (the "License"); you may not use this file except in compliance with the License. You may
; obtain a copy of the License in the LICENSE file at the root of this repository or at
; https://opensource.org/license/cpal-1-0. The License is based on the Mozilla Public License
; Version 1.1 but Sections 14 and 15 have been added to cover use of software over a computer
; network and provide for limited attribution for the Original Developer. In addition, Exhibit A
; has been modified to be consistent with Exhibit B.
;
; Software distributed under the License is distributed on an "AS IS" basis, WITHOUT WARRANTY OF
; ANY KIND, either express or implied. See the License for the specific language governing rights
; and limitations under the License.
;
; The Original Code is Sessionmux. The Initial Developer of the Original Code is joyal-droid. All
; portions of the code written by joyal-droid are Copyright (c) 2026 joyal-droid. All Rights Reserved.
; Contributor(s): see the git history of this repository.
;
; Attribution (Exhibit B of the License): "Powered by Sessionmux", Copyright (c) 2026 joyal-droid,
; https://github.com/joyal-droid/sessionmux and the Sessionmux mark must stay visible in the user
; interface of every copy and derivative; see docs/attribution.md.

; Sessionmux - per-user, one-click NSIS installer.
;
; Build with installer\build.ps1, which stages Sessionmux.exe + README.md + LICENSE.txt +
; THIRD-PARTY.md and passes:
;   /DVERSION=1.0.0  /DSTAGE_DIR=<dir with those four files>  /DOUT_FILE=<setup exe>  /DICON_FILE=<icon>
;   /DFILE_VERSION=1.0.0.0 (the numeric x.y.z.w file version; a pre-release such as
;   1.1.0-preview.1 is given as 1.1.0.0)
; The defaults below let "makensis installer\sessionmux.nsi" work after a staging run too.
;
; Behaviour:
;   - no pages to click through; /S runs silently
;   - installs to %LOCALAPPDATA%\Programs\Sessionmux, no elevation (RequestExecutionLevel user)
;   - closes a running Sessionmux.exe first (WM_CLOSE, then taskkill without /T), so upgrades
;     never hit locked files; the Claude Desktop windows it started keep running
;   - Start-menu + desktop shortcuts, HKCU uninstall entry with EstimatedSize
;   - warns (does not bundle) when the WebView2 Runtime is missing
;   - starts Sessionmux when done
;   - the uninstaller first ends an active sign-in relay (Sessionmux.exe --end-relay, then drops a
;     claude:// handler value that still runs a Sessionmux relay) and removes Sessionmux's Claude
;     Code hook entries (Sessionmux.exe --remove-hooks), then removes exactly what Setup
;     installed (Sessionmux.exe, README.md, LICENSE.txt, THIRD-PARTY.md, the uninstaller), the
;     shortcuts, the Run value and the uninstall entry; it keeps %APPDATA%\Sessionmux

Unicode true
ManifestDPIAware true
; /DUNCOMPRESSED builds a plain copy whose contents can be searched byte for byte.
!ifdef UNCOMPRESSED
  SetCompress off
!else
  SetCompressor /SOLID lzma
  SetCompressorDictSize 16
!endif

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"
!include "StrFunc.nsh"
${UnStrStr}

; ---------------------------------------------------------------- defines
!define APP_NAME      "Sessionmux"
!define APP_EXE       "Sessionmux.exe"
!define PUBLISHER     "Sessionmux"
!ifndef VERSION
  !define VERSION     "1.0.0"
!endif
!ifndef FILE_VERSION
  !define FILE_VERSION "${VERSION}.0"
!endif
!ifndef STAGE_DIR
  !define STAGE_DIR   "${__FILEDIR__}\..\dist\stage"
!endif
!ifndef OUT_FILE
  !define OUT_FILE    "${__FILEDIR__}\..\dist\Sessionmux-Setup-${VERSION}.exe"
!endif
!ifndef ICON_FILE
  !define ICON_FILE   "${__FILEDIR__}\..\assets\icon.ico"
!endif

!define UNINST_ROOT   "Software\Microsoft\Windows\CurrentVersion\Uninstall"
!define UNINST_KEY    "${UNINST_ROOT}\${APP_NAME}"
!define RUN_KEY       "Software\Microsoft\Windows\CurrentVersion\Run"
!define WEBVIEW2_GUID "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
!define WEBVIEW2_URL  "https://developer.microsoft.com/microsoft-edge/webview2/"

Name "${APP_NAME}"
OutFile "${OUT_FILE}"
InstallDir "$LOCALAPPDATA\Programs\${APP_NAME}"
RequestExecutionLevel user
ShowInstDetails nevershow
ShowUninstDetails nevershow
AutoCloseWindow true
BrandingText "${APP_NAME} ${VERSION}"

VIProductVersion "${FILE_VERSION}"
VIAddVersionKey "ProductName"     "${APP_NAME}"
VIAddVersionKey "ProductVersion"  "${VERSION}"
VIAddVersionKey "FileVersion"     "${VERSION}"
VIAddVersionKey "CompanyName"     "${PUBLISHER}"
VIAddVersionKey "LegalCopyright"  "Copyright (c) 2026 joyal-droid. CPAL-1.0."
VIAddVersionKey "FileDescription" "${APP_NAME} Setup - switch between Claude Desktop accounts (unofficial, not affiliated with Anthropic)"

; ---------------------------------------------------------------- UI (install only)
!define MUI_ICON   "${ICON_FILE}"
!define MUI_UNICON "${ICON_FILE}"
!define MUI_UNCONFIRMPAGE_TEXT_TOP "Sessionmux will be removed from this PC. Your accounts, their Claude data folders and Sessionmux's settings in %APPDATA%\Sessionmux are kept."

!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

; ---------------------------------------------------------------- helpers
; Close a running Sessionmux.exe without touching the Claude Desktop windows it started. First
; WM_CLOSE (0x0010) to its message-only window (looked up under HWND_MESSAGE = -3), which makes
; it shut down cleanly; wait up to 5 s. Then taskkill /F for anything still running, never with
; /T, so the process tree (the Claude windows) is left alone. taskkill exits non-zero when
; nothing runs; ignored.
!macro CloseSessionmux
  FindWindow $0 "SessionmuxMsgWindow" "" -3
  ${If} $0 <> 0
    SendMessage $0 0x0010 0 0 /TIMEOUT=3000
    ${For} $1 1 20
      Sleep 250
      FindWindow $0 "SessionmuxMsgWindow" "" -3
      ${IfThen} $0 = 0 ${|} ${Break} ${|}
    ${Next}
  ${EndIf}
  nsExec::Exec 'taskkill /F /IM "${APP_EXE}"'
  Pop $0
  Sleep 500
!macroend

; WebView2 Runtime (the panel needs it; Windows 11 ships it). Registered per machine
; (WOW6432Node) or per user. "pv" is the installed version; empty or 0.0.0.0 = absent.
Function CheckWebView2
  ReadRegStr $0 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\${WEBVIEW2_GUID}" "pv"
  ${If} $0 != ""
  ${AndIf} $0 != "0.0.0.0"
    Return
  ${EndIf}
  ReadRegStr $0 HKCU "Software\Microsoft\EdgeUpdate\Clients\${WEBVIEW2_GUID}" "pv"
  ${If} $0 != ""
  ${AndIf} $0 != "0.0.0.0"
    Return
  ${EndIf}
  MessageBox MB_YESNO|MB_ICONINFORMATION "Sessionmux's panel needs the Microsoft Edge WebView2 Runtime, which is not installed on this PC.$\r$\n$\r$\nDownload it (free, from Microsoft) at:$\r$\n${WEBVIEW2_URL}$\r$\n$\r$\nOpen the download page now?" /SD IDNO IDNO wv2_done
    ExecShell "open" "${WEBVIEW2_URL}"
  wv2_done:
FunctionEnd

; ---------------------------------------------------------------- install
Section "Install"
  SetShellVarContext current
  !insertmacro CloseSessionmux

  SetOutPath "$INSTDIR"
  SetOverwrite on
  File "${STAGE_DIR}\${APP_EXE}"
  File "${STAGE_DIR}\README.md"
  File "${STAGE_DIR}\LICENSE.txt"      ; CPAL-1.0
  File "${STAGE_DIR}\THIRD-PARTY.md"   ; copyright notices of the bundled crates
  WriteUninstaller "$INSTDIR\Uninstall ${APP_NAME}.exe"

  CreateShortCut "$SMPROGRAMS\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}" "" "$INSTDIR\${APP_EXE}" 0
  CreateShortCut "$DESKTOP\${APP_NAME}.lnk"    "$INSTDIR\${APP_EXE}" "" "$INSTDIR\${APP_EXE}" 0

  WriteRegStr   HKCU "${UNINST_KEY}" "DisplayName"          "${APP_NAME}"
  WriteRegStr   HKCU "${UNINST_KEY}" "DisplayVersion"       "${VERSION}"
  WriteRegStr   HKCU "${UNINST_KEY}" "Publisher"            "${PUBLISHER}"
  WriteRegStr   HKCU "${UNINST_KEY}" "DisplayIcon"          '"$INSTDIR\${APP_EXE}",0'
  WriteRegStr   HKCU "${UNINST_KEY}" "InstallLocation"      "$INSTDIR"
  WriteRegStr   HKCU "${UNINST_KEY}" "UninstallString"      '"$INSTDIR\Uninstall ${APP_NAME}.exe"'
  WriteRegStr   HKCU "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall ${APP_NAME}.exe" /S'
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" "$0"

  Call CheckWebView2

  ; Run after finish (as the current user; this installer is never elevated), except on a
  ; silent (/S) install.
  ${IfNot} ${Silent}
    Exec '"$INSTDIR\${APP_EXE}"'
  ${EndIf}
SectionEnd

; ---------------------------------------------------------------- uninstall
; Mirror of the install section: the same four files + the uninstaller, the same two
; shortcuts, the Run value the app writes, the uninstall key. Nothing else.
Section "Uninstall"
  SetShellVarContext current
  !insertmacro CloseSessionmux

  ; If the clean close timed out, taskkill skipped Sessionmux's own shutdown, so a sign-in relay may still own the
  ; claude:// handler. Let the exe end it (restores Claude's saved command), then, as a
  ; fallback, drop a value that still runs a Sessionmux relay: Claude re-registers itself on its
  ; next start, and a dangling handler to a deleted exe would break every link meanwhile.
  ${If} ${FileExists} "$INSTDIR\${APP_EXE}"
    nsExec::Exec '"$INSTDIR\${APP_EXE}" --end-relay'
    Pop $0
    ; Take Sessionmux's hook entries out of %USERPROFILE%\.claude\settings.json while the exe
    ; still exists, so Claude Code is not left calling a deleted program; the rest of the file
    ; stays as it was.
    nsExec::Exec '"$INSTDIR\${APP_EXE}" --remove-hooks'
    Pop $0
  ${EndIf}
  ReadRegStr $0 HKCU "Software\Classes\claude\shell\open\command" ""
  ${UnStrStr} $1 $0 "${APP_EXE}"
  ${UnStrStr} $2 $0 "--relay"
  ${If} $1 != ""
  ${AndIf} $2 != ""
    DeleteRegValue HKCU "Software\Classes\claude\shell\open\command" ""
  ${EndIf}

  Delete "$INSTDIR\${APP_EXE}"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\LICENSE.txt"
  Delete "$INSTDIR\THIRD-PARTY.md"
  Delete "$INSTDIR\Uninstall ${APP_NAME}.exe"
  RMDir "$INSTDIR"   ; only when empty: never removes anything we did not install

  Delete "$SMPROGRAMS\${APP_NAME}.lnk"
  Delete "$DESKTOP\${APP_NAME}.lnk"

  DeleteRegValue HKCU "${RUN_KEY}" "${APP_NAME}"
  DeleteRegKey HKCU "${UNINST_KEY}"
  ; Kept: %APPDATA%\Sessionmux (sessionmux.json, sessionmux.log, relay.json, WebView2 cache), the
  ; settings.json.sessionmux-bak backup and every Claude data folder.
SectionEnd
