import { useState } from "react";
import { pairBrowser } from "../lib/discovery";

/**
 * Shown when this browser reaches a machine that does not know it yet.
 *
 * The gap this closes: opening your own relay hostname in a browser used to load
 * the whole app, fail to authenticate, and sit on "offline — retrying…" with
 * nothing on screen suggesting that pairing was the missing step. It looked
 * broken, and the thing that would have fixed it — a code on the desktop, ten
 * seconds away — was never mentioned.
 *
 * Deliberately *not* an error screen. Nothing has gone wrong: this is the first
 * step of setup, so it reads as an instruction rather than a failure.
 *
 * On success the page reloads instead of threading a session through React state.
 * The cookie is set by then, so a reload re-runs discovery and boots normally —
 * one code path for "arrived with a session" instead of two.
 */
export function PairBrowser() {
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Hyphens and case are cosmetic — the desktop displays `ABCDE-FGHIJ` and the
  // server compares without the hyphen, so someone typing exactly what they see
  // must succeed.
  const cleaned = code.replace(/[\s-]/g, "").toUpperCase();
  const ready = cleaned.length >= 8 && !busy;

  const submit = () => {
    if (!ready) return;
    setBusy(true);
    setError(null);
    void pairBrowser(cleaned)
      .then(() => window.location.reload())
      .catch((e: unknown) => {
        setError(e instanceof Error ? e.message : String(e));
        setBusy(false);
      });
  };

  return (
    <div className="pair-browser">
      <div className="pair-browser-card">
        <div className="pair-browser-brand">
          <span className="brand-word">THREADKNOT</span>
        </div>

        <h1>Pair this browser</h1>
        <p className="pair-browser-lede">
          This machine does not recognise this browser yet. Nothing is wrong — a browser reaching a
          machine over the internet has to be let in once, with a code from the machine itself.
        </p>

        <ol className="pair-browser-steps">
          <li>
            On the machine you are connecting to, open <strong>Settings</strong> →{" "}
            <strong>pair a phone</strong>.
          </li>
          <li>
            Choose <strong>from anywhere</strong>.
          </li>
          <li>Type the code it shows below. It is good for one use and expires in three minutes.</li>
        </ol>

        <input
          className="pair-browser-input"
          value={code}
          onChange={(e) => setCode(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") submit();
          }}
          placeholder="ABCDE-FGHIJ"
          spellCheck={false}
          autoComplete="off"
          autoCapitalize="characters"
          // A code is digits and letters; the numeric-friendly keyboard on a
          // phone makes it materially faster to enter.
          inputMode="text"
          aria-label="Pairing code"
          disabled={busy}
          autoFocus
        />

        {error && (
          <div className="pair-browser-error" role="alert">
            {error}
          </div>
        )}

        <button type="button" className="pair-browser-submit" disabled={!ready} onClick={submit}>
          {busy ? "Pairing…" : "Pair this browser"}
        </button>

        <p className="pair-browser-note">
          The code is not a password and is never reused. What this browser gets is a session
          confined to it, with exactly the permissions chosen on the machine when the code was made —
          you can revoke it there at any time.
        </p>
        <p className="pair-browser-note">
          On the same network you can skip this: open the LAN address from Settings, which carries
          its own access token.
        </p>
      </div>
    </div>
  );
}
