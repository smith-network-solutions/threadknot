// Presentation overlay for recorded walkthroughs.
//
// Injected via Page.addScriptToEvaluateOnNewDocument so it reinstalls itself on
// every document, including after a navigation mid-recording. Everything lives
// in a shadow root so page CSS cannot restyle it and the page cannot
// accidentally select it; the host is pointer-events:none so nothing here can
// intercept the synthetic input the driver is dispatching underneath.
//
// Headless Chrome renders no pointer for synthetic mouse events, so the cursor
// drawn here IS the cursor in the recording. Rust dispatches the real CDP moves
// on the same eased path; this side owns the visuals and animates locally with
// rAF so motion stays smooth without a CDP round trip per frame.
(() => {
  if (window.__threadknotOverlay) return;

  const HOST_ID = "__threadknot_overlay__";
  const state = {
    x: 0,
    y: 0,
    scale: 1,
    anim: null,
    visible: false,
    // The element a spotlight/blur is currently framing, plus the rAF handle
    // watching it. Focus effects are painted as absolute rects, so once the
    // subject unmounts they would otherwise keep ringing empty space until the
    // driver's next explicit clear — most visibly when a click closes the modal
    // the subject lived in.
    subject: null,
    watchRaf: null,
  };
  let host = null;
  let root = null;
  let els = null;

  const easeInOutCubic = (t) =>
    t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;

  function build() {
    if (host && host.isConnected) return true;
    const parent = document.body || document.documentElement;
    if (!parent) return false;

    host = document.createElement("div");
    host.id = HOST_ID;
    host.setAttribute("aria-hidden", "true");
    // Max z-index and fixed to the visual viewport: the overlay must sit above
    // any page chrome without participating in its layout.
    host.style.cssText =
      "position:fixed;inset:0;z-index:2147483647;pointer-events:none;" +
      "border:0;margin:0;padding:0;background:transparent;";
    root = host.attachShadow({ mode: "open" });
    root.innerHTML = `
      <style>
        :host { all: initial; }
        .layer { position: fixed; inset: 0; pointer-events: none; }
        .spot {
          position: fixed; border-radius: 6px; opacity: 0;
          border: 2px solid rgba(111, 213, 189, 0.95);
          /* One huge spread shadow dims everything except the cut-out rect —
             cheaper and sharper than compositing an SVG mask each frame. */
          box-shadow: 0 0 0 9999px rgba(4, 10, 14, 0.55),
                      0 0 22px rgba(111, 213, 189, 0.5);
          transition: opacity 260ms ease, left 260ms ease, top 260ms ease,
                      width 260ms ease, height 260ms ease;
        }
        /* Depth-of-field focus: four backdrop-filter bands framing the subject
           rather than one masked overlay. A CSS mask cut-out would need the
           blur to be re-rasterised against a changing shape every frame; four
           plain rects animate on the compositor and stay sharp at the edges. */
        .blur {
          position: fixed; opacity: 0;
          backdrop-filter: blur(7px) saturate(0.85);
          -webkit-backdrop-filter: blur(7px) saturate(0.85);
          background: rgba(6, 12, 17, 0.32);
          transition: opacity 320ms ease, left 320ms ease, top 320ms ease,
                      width 320ms ease, height 320ms ease;
        }
        .cursor {
          position: fixed; left: 0; top: 0; width: 26px; height: 26px;
          opacity: 0; transition: opacity 200ms ease;
          will-change: transform;
        }
        .cursor svg { display: block; filter: drop-shadow(0 2px 4px rgba(0,0,0,.55)); }
        .ripple {
          position: fixed; width: 14px; height: 14px; margin: -7px 0 0 -7px;
          border-radius: 50%; background: rgba(111, 213, 189, 0.55);
          border: 2px solid rgba(111, 213, 189, 0.95);
          animation: ripple 520ms ease-out forwards;
        }
        @keyframes ripple {
          from { transform: scale(0.4); opacity: 1; }
          to   { transform: scale(4.2); opacity: 0; }
        }
        .caption {
          /* left/top/transform are driven from JS against the visual viewport
             (see paintCursor) so the caption survives a pinch zoom. */
          position: fixed; left: 50%; top: 0; transform: translate(-50%, -100%);
          padding: 14px 22px; border-radius: 12px;
          background: rgba(8, 14, 19, 0.92);
          border: 1px solid rgba(111, 213, 189, 0.35);
          box-shadow: 0 10px 34px rgba(0, 0, 0, 0.45);
          color: #eaf6f2; font: 500 19px/1.4 ui-sans-serif, system-ui, -apple-system,
                 "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
          text-align: center; opacity: 0;
          transition: opacity 240ms ease, transform 240ms ease;
        }
        .caption.on { opacity: 1; }
      </style>
      <div class="layer">
        <div class="blur b-top"></div>
        <div class="blur b-bottom"></div>
        <div class="blur b-left"></div>
        <div class="blur b-right"></div>
        <div class="spot"></div>
        <div class="cursor">
          <svg viewBox="0 0 24 24" width="26" height="26">
            <path d="M5 2.5 L5 20.2 L9.4 16.1 L12.2 22 L15.1 20.6 L12.4 15 L18.3 15 Z"
                  fill="#ffffff" stroke="#0d1b22" stroke-width="1.4"
                  stroke-linejoin="round"/>
          </svg>
        </div>
        <div class="caption"></div>
      </div>`;
    parent.appendChild(host);
    els = {
      layer: root.querySelector(".layer"),
      spot: root.querySelector(".spot"),
      cursor: root.querySelector(".cursor"),
      caption: root.querySelector(".caption"),
      blur: {
        top: root.querySelector(".b-top"),
        bottom: root.querySelector(".b-bottom"),
        left: root.querySelector(".b-left"),
        right: root.querySelector(".b-right"),
      },
    };
    paintCursor();
    return true;
  }

  /** The region the viewer can actually see. Under pinch zoom this is a
   *  sub-rectangle of the layout viewport, and `position: fixed` still resolves
   *  against the LAYOUT viewport — so chrome pinned the naive way slides off
   *  screen the moment the driver zooms in. */
  function view() {
    const v = window.visualViewport;
    return v
      ? { x: v.offsetLeft, y: v.offsetTop, w: v.width, h: v.height, scale: v.scale || 1 }
      : { x: 0, y: 0, w: innerWidth, h: innerHeight, scale: state.scale || 1 };
  }

  function paintCursor() {
    if (!els) return;
    const v = view();
    // Counter-scale so the cursor and caption keep a constant on-screen size
    // while the page itself is pinch-zoomed for a close-up.
    const inv = 1 / (v.scale || 1);
    els.cursor.style.transform =
      `translate(${state.x}px, ${state.y}px) scale(${inv})`;

    // Anchor the caption to the bottom-centre of the VISIBLE region rather than
    // of the document, so it stays put through a zoom.
    els.caption.style.left = `${v.x + v.w / 2}px`;
    els.caption.style.top = `${v.y + v.h - 40 * inv}px`;
    els.caption.style.bottom = "auto";
    els.caption.style.maxWidth = `${v.w * 0.76}px`;
    els.caption.style.transform = `translate(-50%, -100%) scale(${inv})`;
  }

  /** Is `el` still something a viewer could see? Covers the three ways a
   *  subject stops being on screen: unmounted, laid out to nothing / hidden,
   *  or scrolled entirely outside the visible region. */
  function onScreen(el) {
    if (!el || !el.isConnected) return false;
    // An element with no client rects is display:none or in a collapsed
    // subtree; getBoundingClientRect alone would report a zero box instead.
    if (!el.getClientRects().length) return false;
    const r = el.getBoundingClientRect();
    if (r.width < 1 || r.height < 1) return false;
    const v = view();
    if (r.bottom <= v.y || r.top >= v.y + v.h) return false;
    if (r.right <= v.x || r.left >= v.x + v.w) return false;
    const cs = getComputedStyle(el);
    if (cs.visibility === "hidden" || cs.display === "none") return false;
    if (parseFloat(cs.opacity) === 0) return false;
    return true;
  }

  function stopWatch() {
    if (state.watchRaf) cancelAnimationFrame(state.watchRaf);
    state.watchRaf = null;
    state.subject = null;
  }

  const api = {
    show(on) {
      if (!build()) return;
      state.visible = !!on;
      els.cursor.style.opacity = on ? "1" : "0";
      if (!on) api.spotlight(null), api.caption(null), api.blur(null);
    },
    /** Bind the focus effects to the element they are framing so they can
     *  clear themselves the frame it stops being rendered, rather than hanging
     *  over the space it used to occupy until the next step. Passing null just
     *  stops watching and leaves the current effects up. */
    watch(el) {
      if (!build()) return;
      stopWatch();
      if (!el || !(el instanceof Element)) return;
      state.subject = el;
      const tick = () => {
        // Another call already cleared the effects: nothing left to guard.
        if (!state.subject) return void (state.watchRaf = null);
        if (!onScreen(state.subject)) {
          stopWatch();
          // Only clear what was actually framing this element.
          if (els.spot.style.opacity === "1") api.spotlight(null);
          if (els.blur.top.style.opacity === "1") api.blur(null);
          return;
        }
        state.watchRaf = requestAnimationFrame(tick);
      };
      state.watchRaf = requestAnimationFrame(tick);
    },
    /** Hide just the pointer, leaving captions and focus effects in place. */
    cursorVisible(on) {
      if (!build()) return;
      els.cursor.style.opacity = on && state.visible ? "1" : "0";
    },
    at(x, y) {
      if (!build()) return;
      if (state.anim) cancelAnimationFrame(state.anim), (state.anim = null);
      state.x = x;
      state.y = y;
      paintCursor();
    },
    /** Ease the pointer to (x,y) over ms, mirroring the path Rust dispatches. */
    glide(x, y, ms) {
      if (!build()) return;
      if (state.anim) cancelAnimationFrame(state.anim);
      const fromX = state.x;
      const fromY = state.y;
      const dur = Math.max(1, ms | 0);
      const t0 = performance.now();
      const step = (now) => {
        const t = Math.min(1, (now - t0) / dur);
        const e = easeInOutCubic(t);
        state.x = fromX + (x - fromX) * e;
        state.y = fromY + (y - fromY) * e;
        paintCursor();
        if (t < 1) state.anim = requestAnimationFrame(step);
        else state.anim = null;
      };
      state.anim = requestAnimationFrame(step);
    },
    click(x, y) {
      if (!build()) return;
      const r = document.createElement("div");
      r.className = "ripple";
      r.style.left = `${x ?? state.x}px`;
      r.style.top = `${y ?? state.y}px`;
      els.layer.appendChild(r);
      setTimeout(() => r.remove(), 560);
    },
    caption(text) {
      if (!build()) return;
      if (!text) {
        els.caption.classList.remove("on");
        return;
      }
      els.caption.textContent = text;
      els.caption.classList.add("on");
    },
    spotlight(rect) {
      if (!build()) return;
      if (!rect) {
        els.spot.style.opacity = "0";
        if (els.blur.top.style.opacity !== "1") stopWatch();
        return;
      }
      const pad = 6;
      els.spot.style.left = `${rect.x - pad}px`;
      els.spot.style.top = `${rect.y - pad}px`;
      els.spot.style.width = `${rect.width + pad * 2}px`;
      els.spot.style.height = `${rect.height + pad * 2}px`;
      els.spot.style.opacity = "1";
    },
    /** Blur everything except `rect` — a depth-of-field focus on the subject.
     *  Passing null clears it. Combined with a zoom this is what makes a small
     *  detail (a price, a chart) read as the point of the shot. */
    blur(rect) {
      if (!build()) return;
      const b = els.blur;
      if (!rect) {
        for (const el of Object.values(b)) el.style.opacity = "0";
        if (els.spot.style.opacity !== "1") stopWatch();
        return;
      }
      const pad = 6;
      const x = Math.max(0, rect.x - pad);
      const y = Math.max(0, rect.y - pad);
      const w = rect.width + pad * 2;
      const h = rect.height + pad * 2;
      const px = (v) => `${Math.round(v)}px`;

      Object.assign(b.top.style, { left: "0px", top: "0px", width: "100%", height: px(y) });
      Object.assign(b.bottom.style, { left: "0px", top: px(y + h), width: "100%", bottom: "0px", height: "auto" });
      Object.assign(b.left.style, { left: "0px", top: px(y), width: px(x), height: px(h) });
      Object.assign(b.right.style, { left: px(x + w), top: px(y), right: "0px", width: "auto", height: px(h) });
      for (const el of Object.values(b)) el.style.opacity = "1";
    },
    /** Told by the driver whenever page scale changes, so chrome can stay put. */
    scale(s) {
      state.scale = s || 1;
      paintCursor();
    },
    /** Re-attach after a page wipes body (SPA route swaps do this). */
    ensure() {
      return build();
    },
  };

  window.__threadknotOverlay = api;

  // Keep chrome pinned while the driver eases a zoom in or out.
  if (window.visualViewport) {
    window.visualViewport.addEventListener("resize", paintCursor);
    window.visualViewport.addEventListener("scroll", paintCursor);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", () => build(), { once: true });
  } else {
    build();
  }
})();
