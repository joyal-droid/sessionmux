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
 */

/*
 * Sessionmux — window.sessionmux over WebView2 postMessage.
 *
 * Injected by ui::panel as the WebView initialization script (main frame only), so it runs
 * before panel.js. API surface:
 *
 *   Invoke (Promise): listAccounts, openAccount, openAll, closeAccount, addAccount, renameAccount,
 *     removeAccount, reorderAccounts, listSessions, transferSession, deleteSession,
 *     openSessionFolder, getSettings, setSettings, listDiscovered, adoptProfile, ignoreProfile,
 *     togglePanel, hidePanel, pinPanel, quit, openExternal,
 *     getAppInfo, openLicense, getSignIn, cancelSignIn, restoreLinkHandler, removeChanges,
 *     openSystemSettings, listApprovals, decideApproval, listWaiting, handOverSession,
 *     getHooksStatus, previewHooks, installHooks, removeHooks.
 *   Events (each onX(cb) returns an unsubscribe fn): onAccountsChanged, onActiveAccount,
 *     onSettings, onShowTab, onDiscoveredChanged, onSignInChanged ('signin-changed': {active, accountId, accountName, since} or
 *     {active:false, ended?, accountName?}), onHandover ('handover-done': an automatic hand-over
 *     finished; the transfer result plus toName, fromName, opened, restarted, restartNeeded,
 *     windowLeftOpen), onOpenFailed ('openFailed': an open from the bubble or the menu bar menu
 *     failed; {id, name, reason, error}).
 *
 * Transport
 *   page → host : chrome.webview.postMessage(JSON.stringify({id, name, args}))
 *   host → page : window.__sessionmuxResolve(id, ok, payload)   (reply to request `id`)
 *                 window.__sessionmuxEmit(event, payload)       (event = 'accounts-changed' | …
 *                                                              | 'signin-changed' | 'handover-done'
 *                                                              | 'openFailed')
 *   Internal messages (id 0): {name:'__ready'} once the DOM is ready,
 *                             {name:'__error', args:[message, source, line, col, stack]}.
 *
 * ok === false: a payload shaped {ok:false, error, reason?} resolves as-is (exactly what
 * panel.js's call() would have produced from a rejection, plus `reason`); anything else rejects.
 */
(function () {
  'use strict';
  if (window.top !== window || window.sessionmux) return;

  // ------------------------------------------------------------------ transport
  var webview = window.chrome && window.chrome.webview;

  function post(msg) {
    try {
      var text = JSON.stringify(msg);
      if (webview && typeof webview.postMessage === 'function') webview.postMessage(text);
      else if (window.ipc && typeof window.ipc.postMessage === 'function') window.ipc.postMessage(text);
      else return false;
      return true;
    } catch (_) {
      return false;
    }
  }

  var seq = 0;
  var pending = new Map();

  function invoke(name, args) {
    return new Promise(function (resolve, reject) {
      seq += 1;
      var id = seq;
      pending.set(id, { resolve: resolve, reject: reject });
      if (!post({ id: id, name: name, args: args })) {
        pending.delete(id);
        reject(new Error('Sessionmux is not reachable'));
      }
    });
  }

  function resolveFromHost(id, ok, payload) {
    var p = pending.get(id);
    if (!p) return;
    pending.delete(id);
    if (ok) {
      p.resolve(payload);
    } else if (payload && typeof payload === 'object' && payload.ok === false) {
      p.resolve(payload);
    } else {
      var msg = payload && typeof payload === 'object' && payload.error ? payload.error : payload;
      p.reject(new Error(String(msg || 'Request failed')));
    }
  }

  // ------------------------------------------------------------------ events
  var listeners = new Map(); // event name → Set<cb>

  function subscribe(name, cb) {
    if (typeof cb !== 'function') return function () {};
    var set = listeners.get(name);
    if (!set) {
      set = new Set();
      listeners.set(name, set);
    }
    // Wrap so the same cb can subscribe twice and each unsubscribe removes one.
    var entry = function (payload) { cb(payload); };
    set.add(entry);
    return function () { set.delete(entry); };
  }

  function emitFromHost(name, payload) {
    var set = listeners.get(String(name));
    if (!set) return;
    Array.from(set).forEach(function (fn) {
      try {
        fn(payload);
      } catch (err) {
        // Never let a page callback break the event channel.
        console.error('sessionmux ' + name + ' handler failed', err);
      }
    });
  }

  function lock(obj, key, value) {
    Object.defineProperty(obj, key, { value: value, writable: false, configurable: false, enumerable: false });
  }

  lock(window, '__sessionmuxResolve', resolveFromHost);
  lock(window, '__sessionmuxEmit', emitFromHost);

  // ------------------------------------------------------------------ window.sessionmux
  function method(name) {
    return function () { return invoke(name, Array.prototype.slice.call(arguments)); };
  }

  var api = {
    // accounts
    listAccounts: method('listAccounts'),
    openAccount: method('openAccount'),
    openAll: method('openAll'),
    closeAccount: method('closeAccount'),
    addAccount: method('addAccount'),
    renameAccount: method('renameAccount'),
    removeAccount: method('removeAccount'),
    reorderAccounts: method('reorderAccounts'),
    // found %APPDATA%\Claude-* profile folders
    listDiscovered: method('listDiscovered'),
    adoptProfile: method('adoptProfile'),
    ignoreProfile: method('ignoreProfile'),
    // sessions
    listSessions: method('listSessions'),
    transferSession: method('transferSession'),
    deleteSession: method('deleteSession'),
    openSessionFolder: method('openSessionFolder'),
    handOverSession: method('handOverSession'),
    // sessions waiting for the user, approve requests from the Claude Code hooks
    listApprovals: method('listApprovals'),
    decideApproval: method('decideApproval'),
    listWaiting: method('listWaiting'),
    // the hooks in the user's Claude Code settings
    getHooksStatus: method('getHooksStatus'),
    previewHooks: method('previewHooks'),
    installHooks: method('installHooks'),
    removeHooks: method('removeHooks'),
    // settings
    getSettings: method('getSettings'),
    setSettings: method('setSettings'),
    // ui
    togglePanel: method('togglePanel'),
    hidePanel: method('hidePanel'),
    pinPanel: function (pinned) { return invoke('pinPanel', [!!pinned]); },
    quit: method('quit'),
    openExternal: method('openExternal'),
    // product info
    getAppInfo: method('getAppInfo'),
    openLicense: method('openLicense'),
    // sign-in routing
    getSignIn: method('getSignIn'),
    cancelSignIn: method('cancelSignIn'),
    restoreLinkHandler: method('restoreLinkHandler'),
    // everything Sessionmux changed outside its own folder (hooks, link handler, login item)
    removeChanges: method('removeChanges'),
    // a page of the system settings: 'loginItems' | 'accessibility' | 'automation' (macOS)
    openSystemSettings: method('openSystemSettings'),
    // events
    onAccountsChanged: function (cb) { return subscribe('accounts-changed', cb); },
    onActiveAccount: function (cb) { return subscribe('active-account', cb); },
    onSettings: function (cb) { return subscribe('settings', cb); },
    onShowTab: function (cb) { return subscribe('show-tab', cb); },
    onDiscoveredChanged: function (cb) { return subscribe('discovered-changed', cb); },
    onSignInChanged: function (cb) { return subscribe('signin-changed', cb); },
    onHandover: function (cb) { return subscribe('handover-done', cb); },
    // an open from the bubble or the menu bar menu failed: { id, name, reason, error }
    onOpenFailed: function (cb) { return subscribe('openFailed', cb); },
  };
  lock(window, 'sessionmux', Object.freeze(api));

  // ------------------------------------------------------------------ errors → host log
  var errorBudget = 25;
  function report(message, source, line, col, stack) {
    if (errorBudget <= 0) return;
    errorBudget -= 1;
    post({
      id: 0,
      name: '__error',
      args: [String(message || 'error').slice(0, 2000), String(source || ''), Number(line) || 0, Number(col) || 0, String(stack || '').slice(0, 4000)],
    });
  }

  window.addEventListener('error', function (e) {
    var err = e && e.error;
    report(e && e.message, e && e.filename, e && e.lineno, e && e.colno, err && err.stack);
  });
  window.addEventListener('unhandledrejection', function (e) {
    var r = e && e.reason;
    report('Unhandled rejection: ' + ((r && r.message) || r), '', 0, 0, r && r.stack);
  });
  var consoleError = console.error;
  console.error = function () {
    try {
      var parts = Array.prototype.map.call(arguments, function (a) {
        if (a && a.stack) return String(a.stack);
        if (typeof a === 'object') { try { return JSON.stringify(a); } catch (_) { return String(a); } }
        return String(a);
      });
      report('console.error: ' + parts.join(' '), '', 0, 0, '');
    } catch (_) { /* never throw from console */ }
    return consoleError.apply(console, arguments);
  };

  // ------------------------------------------------------------------ background + ready
  function onDomReady() {
    try {
      var style = document.createElement('style');
      style.textContent = 'html{background:var(--bg-solid)}';
      (document.head || document.documentElement).appendChild(style);
    } catch (_) { /* cosmetic only */ }
    // panel.js (end of <body>) has subscribed by now; tell the host it may emit.
    setTimeout(function () { post({ id: 0, name: '__ready', args: [] }); }, 0);
  }
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', onDomReady, { once: true });
  else onDomReady();
})();
