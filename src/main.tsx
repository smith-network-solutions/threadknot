// First, before react-dom: the render tracer installs the DevTools global hook
// that react-dom reads while its own module evaluates. It is inert unless armed
// (?tktrace=1). See docs/RENDER-FORENSICS.md.
import "./lib/renderTrace";
import { createRoot } from "react-dom/client";
import App from "./App";
import "./styles.css";
import { applyAppearance } from "./lib/appearance";
import { installKeyboardInset } from "./lib/viewport";

// Apply the saved theme + zoom before the first paint to avoid a flash.
applyAppearance();

// Track the on-screen keyboard for the life of the page (see --kb-inset).
installKeyboardInset();

const rootEl = document.getElementById("root");
if (!rootEl) throw new Error("missing #root");

createRoot(rootEl).render(<App />);
