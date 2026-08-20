// Injected into every page of a DISPOSABLE browser session (see wire_page in
// browser.rs). Its one job: notice when the human signs into a site, so
// Threadknot can offer to keep that session as a saved login.
//
// It signals the backend through the `__tkLoginSubmitted` CDP binding — a real
// function on `window`, installed by Runtime.addBinding, whose every call fires
// Runtime.bindingCalled. That path is chosen over a sentinel console.log
// because the backend's console listener drops background tabs, and a login in
// a popup window would be lost.
//
// It sends only the HOST, never any field value: whether you logged in, not
// what you typed. The decision to store anything is the human's, in the pane.
(function () {
  if (window.__tkLoginWatch) return; // one install per document
  window.__tkLoginWatch = true;

  var signal = function () {
    try {
      if (typeof window.__tkLoginSubmitted !== "function") return;
      window.__tkLoginSubmitted(location.host || "");
    } catch (e) {
      /* binding not attached (non-disposable session) — nothing to do */
    }
  };

  // A login is a form that carries a password field. Checked at submit time,
  // not install time, because SPAs mount the form later.
  var looksLikeLogin = function (form) {
    if (!form) return false;
    try {
      return !!form.querySelector('input[type="password"]');
    } catch (e) {
      return false;
    }
  };

  // Classic form posts.
  document.addEventListener(
    "submit",
    function (ev) {
      if (looksLikeLogin(ev.target)) signal();
    },
    true, // capture: fire even if the page stops propagation on its own handler
  );

  // SPA logins that never submit a form — a click on a button inside a block
  // that contains a filled password field. Deliberately loose: a false
  // positive only offers to save a session the user can decline, while a miss
  // means the whole feature silently does nothing.
  document.addEventListener(
    "click",
    function (ev) {
      try {
        var el = ev.target;
        if (!el || !el.closest) return;
        var btn = el.closest(
          'button, [type="submit"], [role="button"], a',
        );
        if (!btn) return;
        var scope = btn.closest("form") || document;
        var pw = scope.querySelector('input[type="password"]');
        if (pw && pw.value) signal();
      } catch (e) {
        /* ignore */
      }
    },
    true,
  );
})();
