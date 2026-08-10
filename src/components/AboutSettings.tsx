// Settings -> About: what this program is, what build you are running, who
// made it, and what it is licensed under.
//
// It is also the last screen in the nav, which is the traditional place to
// leave something for anyone who reads all the way to the end. The entry
// sequence is only live once the content is scrolled to its foot; the rules
// themselves live in lib/legacyCircuit.ts, and this file only decides when to
// listen and what to show while someone is typing.

import { useEffect, useRef, useState } from "react";
import { useStore } from "../state/store";
// The dedication is deliberately NOT imported here. It names the circuit, and
// a line about a hidden circuit sitting in plain sight on the About screen
// announces the secret to everyone who never goes looking. It belongs on the
// cabinet's own landing screen, which only someone who found the way in sees.
import {
  advanceCircuit,
  CIRCUIT_LENGTH,
  CIRCUIT_SEQUENCE,
  CREST_NAME,
} from "../lib/legacyCircuit";
import { Crest, useLegacyAward } from "./legacy/Crest";

const REPO_URL = "https://github.com/smith-network-solutions/threadknot";
const LICENSE_URL = "https://github.com/smith-network-solutions/threadknot/blob/master/LICENSE";
const SECURITY_URL = "https://github.com/smith-network-solutions/threadknot/blob/master/SECURITY.md";
const NOTICES_URL =
  "https://github.com/smith-network-solutions/threadknot/blob/master/THIRD-PARTY-NOTICES.md";
const SITE_URL = "https://threadknot.vercel.app/";

/** "Aug 9, 2026" from YYYY-MM-DD, parsed by hand so it cannot slip a day west
 *  of UTC (the same reason VersionBadge does it this way). */
function fmtDate(iso: string): string {
  const [y, m, d] = iso.split("-").map(Number);
  if (!y || !m || !d) return iso;
  return new Date(y, m - 1, d).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    year: "numeric",
  });
}

/** Typing into a field should never be read as a chord, even a chord nothing
 *  else on this screen uses. */
function typingSomewhere(): boolean {
  const el = document.activeElement;
  if (!(el instanceof HTMLElement)) return false;
  if (el.isContentEditable) return true;
  const tag = el.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
}

export function AboutSettings({ onUnlock }: { onUnlock: () => void }) {
  const { state } = useStore();
  const hello = state.hello;
  const award = useLegacyAward();

  const footRef = useRef<HTMLDivElement | null>(null);
  const [atFoot, setAtFoot] = useState(false);
  const [progress, setProgress] = useState(0);
  const [rejected, setRejected] = useState(false);

  // "Scrolled to the bottom" as a question the browser answers: a 1px marker at
  // the very foot, fully in view. Measuring scrollTop against scrollHeight
  // instead would have to be re-tuned every time something above it grows.
  useEffect(() => {
    const marker = footRef.current;
    if (!marker) return;
    const root = marker.closest(".ss-content");
    const io = new IntersectionObserver(
      (entries) => setAtFoot(entries.some((e) => e.isIntersecting)),
      { root: root instanceof Element ? root : null, threshold: 1 },
    );
    io.observe(marker);
    return () => io.disconnect();
  }, []);

  // Progress is dropped the moment the foot leaves view, so a half-entered
  // sequence cannot be finished after scrolling away and back.
  useEffect(() => {
    if (!atFoot) setProgress(0);
  }, [atFoot]);

  useEffect(() => {
    if (!atFoot) return;
    function onKey(e: KeyboardEvent) {
      if (typingSomewhere()) return;
      const verdict = advanceCircuit(progress, e);
      if (verdict.kind === "ignored") return;
      if (verdict.kind === "abandoned") {
        // The attempt is over but the keystroke is not ours: let Tab tab.
        setProgress(0);
        return;
      }
      // Everything below was a Ctrl+Shift chord aimed at this screen, so
      // consuming it cannot swallow a shortcut belonging to anything else.
      e.preventDefault();
      if (verdict.kind === "unlocked") {
        setProgress(CIRCUIT_LENGTH);
        onUnlock();
      } else if (verdict.kind === "advanced") {
        setProgress(verdict.progress);
      } else {
        setProgress(verdict.progress);
        setRejected(true);
      }
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [atFoot, progress, onUnlock]);

  // Clear the miss tint on its own, so a fumbled entry does not leave the strip
  // sitting red until the next keypress.
  useEffect(() => {
    if (!rejected) return;
    const t = window.setTimeout(() => setRejected(false), 320);
    return () => window.clearTimeout(t);
  }, [rejected]);

  return (
    <>
      <div className="settings-block about-block">
        <div className="settings-label">about threadknot</div>
        <p className="about-lede">
          Threadknot puts every coding agent you use on one thread. It drives
          Claude Code, Codex, Kimi Code and remote gateways over their own wire
          protocols, keeps the whole conversation as replayable events, and
          serves the same interface to your desktop, your phone and every
          machine on your network.
        </p>
        <p className="about-copy">
          It runs on your own machine and signs in with the command-line tools
          you already have, so there are no API keys to hold and, out of the
          box, nothing leaves your network.
        </p>
      </div>

      <div className="settings-block about-block">
        <div className="settings-label">this build</div>
        <dl className="about-facts">
          <div className="about-fact">
            <dt>version</dt>
            <dd>{hello ? `v${hello.version}` : "…"}</dd>
          </div>
          {hello?.buildDate && (
            <div className="about-fact">
              <dt>built</dt>
              <dd>{fmtDate(hello.buildDate)}</dd>
            </div>
          )}
          {hello?.gitHash && (
            <div className="about-fact">
              <dt>commit</dt>
              <dd className="about-mono">{hello.gitHash}</dd>
            </div>
          )}
          {hello?.friendlyName && (
            <div className="about-fact">
              <dt>machine</dt>
              <dd>{hello.friendlyName}</dd>
            </div>
          )}
        </dl>
        <div className="settings-hint">
          The version number counts commits, so every shipped change moves it.
          Click it in the sidebar footer for the update notes.
        </div>
      </div>

      <div className="settings-block about-block">
        <div className="settings-label">open source</div>
        <p className="about-copy">
          Threadknot is free and open source under the Apache License 2.0. The
          desktop app and the mobile companion are the whole product: the paid
          tier is an optional hosted service around them, and it is not what you
          are running here.
        </p>
        <div className="about-links">
          <a href={REPO_URL} target="_blank" rel="noopener noreferrer">
            source repository
          </a>
          <a href={LICENSE_URL} target="_blank" rel="noopener noreferrer">
            licence
          </a>
          <a href={NOTICES_URL} target="_blank" rel="noopener noreferrer">
            third-party notices
          </a>
          <a href={SECURITY_URL} target="_blank" rel="noopener noreferrer">
            security policy
          </a>
          <a href={SITE_URL} target="_blank" rel="noopener noreferrer">
            threadknot.dev
          </a>
        </div>
      </div>

      <div className="settings-block about-block">
        <div className="settings-label">credits</div>
        <p className="about-copy">
          Built by Spencer Smith at Smith Network Solutions, with everyone who
          has opened an issue, filed a patch or told us what was broken. It
          stands on a long stack of other people's work: Rust, Tauri, React,
          axum, xterm.js and the rest, all named in the third-party notices.
        </p>

        {award.earned && (
          <div className="about-earned">
            <Crest size={22} title={CREST_NAME} />
            <span className="about-earned-text">
              <strong>{CREST_NAME}</strong>
              <span>
                Awarded on this machine. You know the way back in.
              </span>
            </span>
          </div>
        )}

        {/* Reads as a dotted rule until someone starts entering something, at
            which point it becomes the only feedback they get. aria-hidden: it
            is ornament to everyone who is not already typing a sequence. */}
        <div
          className={`about-contacts${progress > 0 ? " live" : ""}${rejected ? " miss" : ""}`}
          aria-hidden="true"
        >
          {CIRCUIT_SEQUENCE.map((step, i) => (
            <span key={i} className={`about-contact${i < progress ? " lit" : ""}`}>
              {i < progress ? step.glyph : ""}
            </span>
          ))}
        </div>

        <div ref={footRef} className="about-foot-marker" aria-hidden="true" />
      </div>
    </>
  );
}
