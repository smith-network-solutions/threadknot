import { createRoot } from "react-dom/client";
import App from "./App";
import "./styles.css";
import { applyAppearance } from "./lib/appearance";

// Apply the saved theme + zoom before the first paint to avoid a flash.
applyAppearance();

const rootEl = document.getElementById("root");
if (!rootEl) throw new Error("missing #root");

createRoot(rootEl).render(<App />);
