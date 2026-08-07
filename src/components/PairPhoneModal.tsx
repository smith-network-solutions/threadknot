import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { MobileDeviceInfo, PairingQr } from "../lib/protocol";
import { useStore } from "../state/store";
import { XIcon } from "./icons";

/** How often we ask the server whether a phone finished pairing. The scan
 *  itself is instant; this is only the desktop noticing. */
const POLL_MS = 1500;

/** How long the success state stays up before the dialog closes itself. */
const DONE_MS = 1400;

/**
 * "Pair a phone": shows a QR the Threadknot mobile app scans.
 *
 * The QR encodes a one-time code, NOT this machine's master token — a screen
 * showing a QR is passively readable by anyone in the room (or in a
 * screenshot, or on a shared display), and a master token leaked that way is
 * permanent. The code is single-use, expires on its own, and is invalidated
 * outright the moment this dialog closes.
 */
export function PairPhoneModal({
  knownDeviceIds,
  onPaired,
  onClose,
}: {
  /** Devices already paired when the dialog opened — anything new is the scan. */
  knownDeviceIds: string[];
  onPaired: (device: MobileDeviceInfo) => void;
  onClose: () => void;
}) {
  const { actions } = useStore();
  const [qr, setQr] = useState<PairingQr | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [remaining, setRemaining] = useState(0);
  const [paired, setPaired] = useState<MobileDeviceInfo | null>(null);
  const knownRef = useRef(new Set(knownDeviceIds));

  const mint = useCallback(() => {
    setError(null);
    setQr(null);
    void actions
      .beginMobilePairing()
      .then((next) => {
        setQr(next);
        setRemaining(next.ttlSeconds);
      })
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)));
  }, [actions]);

  useEffect(mint, [mint]);

  // Escape closes only this dialog, leaving settings open underneath.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      onClose();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);

  // Whatever is on screen stops working when the dialog goes away — including
  // an unmount from the settings screen closing, not just the X button.
  useEffect(() => {
    return () => {
      void actions.cancelMobilePairing().catch(() => undefined);
    };
  }, [actions]);

  // Countdown. Expiry is enforced server-side; this only keeps the UI honest
  // about a code that has already stopped working.
  useEffect(() => {
    if (!qr || paired || remaining <= 0) return;
    const t = setTimeout(() => setRemaining((r) => Math.max(0, r - 1)), 1000);
    return () => clearTimeout(t);
  }, [qr, paired, remaining]);

  // The phone talks to the server, not to us, so the desktop finds out by
  // asking. Only while the QR is live and unredeemed.
  useEffect(() => {
    if (!qr || paired || remaining <= 0) return;
    let cancelled = false;
    const timer = setInterval(() => {
      void actions
        .listMobileDevices()
        .then((devices) => {
          if (cancelled) return;
          const fresh = devices.find((d) => !knownRef.current.has(d.id));
          if (fresh) setPaired(fresh);
        })
        .catch(() => undefined);
    }, POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [actions, qr, paired, remaining]);

  useEffect(() => {
    if (!paired) return;
    onPaired(paired);
    const t = setTimeout(onClose, DONE_MS);
    return () => clearTimeout(t);
  }, [paired, onPaired, onClose]);

  const expired = !paired && remaining <= 0 && qr !== null;

  return createPortal(
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal pair-phone-modal"
        role="dialog"
        aria-label="Pair a phone"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal-head">
          <span>Pair a phone</span>
          <button className="icon-btn" aria-label="Close" onClick={onClose}>
            <XIcon size={14} />
          </button>
        </div>

        {paired ? (
          <div className="pair-phone-done">
            <div className="pair-phone-check" aria-hidden="true">
              ✓
            </div>
            <div>
              <strong>{paired.name}</strong> is paired.
            </div>
            <div className="pair-phone-note">
              Revoke it any time from the paired-phones list.
            </div>
          </div>
        ) : (
          <>
            <div className="pair-phone-body">
              In the Threadknot app, tap <strong>Add server → Scan QR code</strong>.
            </div>

            <div className="pair-phone-stage">
              {error && <div className="modal-error">{error}</div>}
              {!error && !qr && <div className="pair-phone-note">Generating code…</div>}
              {qr && (
                <>
                  <div className={`pair-phone-qr${expired ? " expired" : ""}`}>
                    <img
                      src={`data:image/svg+xml,${encodeURIComponent(qr.qrSvg)}`}
                      alt={`Pairing QR code ${qr.code}`}
                      width={240}
                      height={240}
                    />
                    {expired && <div className="pair-phone-expired">expired</div>}
                  </div>
                  <div className="pair-phone-meta">
                    <code className="settings-value">{qr.url}</code>
                    <code className="pair-phone-code">{qr.code}</code>
                  </div>
                </>
              )}
            </div>

            <div className="pair-phone-note">
              {expired
                ? "This code has expired."
                : qr
                  ? `Single use — expires in ${remaining}s. The QR carries a one-time code, never this machine's token.`
                  : ""}
            </div>

            {(expired || error) && (
              <div className="modal-actions">
                <button className="btn" onClick={mint}>
                  Show a new code
                </button>
              </div>
            )}
          </>
        )}
      </div>
    </div>,
    document.body,
  );
}
