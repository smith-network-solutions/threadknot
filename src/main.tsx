// First, before react-dom: the render tracer installs the DevTools global hook
// that react-dom reads while its own module evaluates. It is inert unless armed
// (?tktrace=1). See docs/RENDER-FORENSICS.md.
import "./lib/renderTrace";
import { createRoot } from "react-dom/client";
import App from "./App";
// Instrument Sans: the Alcove skin's face, bundled so it needs no network.
import "@fontsource-variable/instrument-sans";
import "./styles.css";
// Alcove skin: every rule is scoped to :root[data-theme="alcove"], inert elsewhere.
import "./styles/alcove/index.css";
// Last in the cascade on purpose: no focus rings anywhere (see the file).
import "./styles/no-focus.css";
import { installGlassPress } from "./lib/glassPress";

// The Alcove press ripples out of the point clicked; CSS cannot know where
// that is, so one listener records it. No-op under every other skin.
installGlassPress();
import { applyAppearance } from "./lib/appearance";
import { installKeyboardInset } from "./lib/viewport";

// Apply the saved theme + zoom before the first paint to avoid a flash.
applyAppearance();

// Track the on-screen keyboard for the life of the page (see --kb-inset).
installKeyboardInset();

// No webview context menu. This is a desktop app, not a page: the browser's own
// menu offers Back/Reload/View source on a surface where none of that means
// anything, and it opens on top of the app's own right-click menus (sidebar
// rows, terminal, browser pane) instead of them. Those are React onContextMenu
// handlers, which still run — preventing the default only suppresses the menu
// the webview would have drawn.
//
// Capture phase so a handler that stops propagation cannot leak the native menu
// through. Text fields are included deliberately: this takes right-click paste
// and spell-check suggestions with it, which is the cost of the convention —
// Ctrl+V and the rest of the editing keys are untouched.
window.addEventListener("contextmenu", (event) => event.preventDefault(), true);

const rootEl = document.getElementById("root");
if (!rootEl) throw new Error("missing #root");

createRoot(rootEl).render(<App />);
