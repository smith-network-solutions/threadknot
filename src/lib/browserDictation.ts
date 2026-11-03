// Dictation in the webview, for installs where the server has none.
//
// Threadknot's own dictation records on the SERVER and transcribes with the
// faster-whisper sidecar (src-tauri/src/dictation.rs, voice/stt.rs). That is
// the better path when it exists — offline, no Google, and it hears the
// machine's own microphone. But on a machine without the sidecar the hello
// frame reports `dictation.available: false` and the mic button had nothing
// behind it but an error toast.
//
// This is the fallback: capture through getUserMedia and transcribe with the
// webview's SpeechRecognition, which is what the Alcove design prototype did.
// It deliberately presents the same four-state machine the server path does
// (idle -> starting -> recording -> transcribing), so every composer's UI —
// the waveform, the clock, the stop button — drives it without knowing which
// engine is underneath.
//
// Two things stop it from being the only path. SpeechRecognition is a Chromium
// service that not every embedded webview ships, and `navigator.mediaDevices`
// is absent outside a secure context, which is exactly what a phone on the LAN
// over plain http is. `supportsBrowserDictation()` answers both before any UI
// offers the button.

/** The slice of the SpeechRecognition API this uses, typed locally: the DOM
 *  lib does not declare it, and it is still vendor-prefixed in most engines. */
type RecognitionAlternative = { transcript: string };
type RecognitionResult = { isFinal: boolean; 0: RecognitionAlternative };
type RecognitionEvent = {
  results: { length: number; [index: number]: RecognitionResult };
};
type RecognitionErrorEvent = { error?: string };
type Recognition = {
  continuous: boolean;
  interimResults: boolean;
  lang: string;
  onresult: ((event: RecognitionEvent) => void) | null;
  onerror: ((event: RecognitionErrorEvent) => void) | null;
  onend: (() => void) | null;
  start: () => void;
  stop: () => void;
  abort: () => void;
};
type RecognitionConstructor = new () => Recognition;

function recognitionConstructor(): RecognitionConstructor | undefined {
  const w = window as typeof window & {
    SpeechRecognition?: RecognitionConstructor;
    webkitSpeechRecognition?: RecognitionConstructor;
  };
  return w.SpeechRecognition ?? w.webkitSpeechRecognition;
}

/** Whether this webview can dictate on its own. Both halves are required: the
 *  microphone (absent outside a secure context) and the recogniser (absent in
 *  webviews that do not carry Chromium's speech service). */
export function supportsBrowserDictation(): boolean {
  return !!navigator.mediaDevices?.getUserMedia && !!recognitionConstructor();
}

/** The live capture, published so a visualiser can draw the very stream being
 *  transcribed rather than opening a second one. Windows will hand out a
 *  second microphone consumer, but it is a second permission surface and a
 *  second device open for no reason. Null whenever nothing is recording. */
let liveStream: MediaStream | null = null;
export function liveDictationStream(): MediaStream | null {
  return liveStream;
}

export type BrowserDictation = {
  /** Stop recording and resolve with everything heard, "" if that is nothing.
   *  Safe to call twice; the second call resolves with the same transcript. */
  stop: () => Promise<string>;
  /** Drop the clip and release the microphone. */
  cancel: () => void;
};

/** How long to keep listening after stop() for the recogniser's final result.
 *  It emits one shortly after the audio ends; without the wait the last few
 *  words are lost, and with too long a wait the composer looks hung. */
const FINALISE_MS = 1400;
/** Chromium ends a recognition session on a silent interval even when told to
 *  be continuous. While the person still holds the session open, restart it. */
const RESTART_MS = 80;

/**
 * Open the microphone and start transcribing.
 *
 * Rejects if the microphone is refused or missing — the caller shows that,
 * since what to say about a denied permission belongs with the UI. `onTrouble`
 * reports a recogniser fault *during* a session, which is not a rejection: the
 * audio may still be fine and the person is mid-sentence.
 */
export async function startBrowserDictation(
  onTrouble?: (message: string) => void,
): Promise<BrowserDictation> {
  const Recognition = recognitionConstructor();
  if (!Recognition) throw new Error("This webview has no speech recognition.");

  // Throws NotAllowedError when refused, NotFoundError with no input device.
  // Both are reported by the caller verbatim enough to act on.
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
  });
  if (!stream.getAudioTracks().length) {
    stream.getTracks().forEach((t) => t.stop());
    throw new Error("No microphone track is available.");
  }
  liveStream = stream;

  let transcript = "";
  let state: "recording" | "stopping" | "done" = "recording";
  let restartTimer: number | undefined;
  let finaliseTimer: number | undefined;
  let settle: ((spoken: string) => void) | null = null;

  const release = () => {
    window.clearTimeout(restartTimer);
    window.clearTimeout(finaliseTimer);
    restartTimer = undefined;
    finaliseTimer = undefined;
    stream.getTracks().forEach((track) => track.stop());
    if (liveStream === stream) liveStream = null;
  };

  const finish = () => {
    if (state === "done") return;
    state = "done";
    release();
    const spoken = transcript.replace(/\s+/g, " ").trim();
    settle?.(spoken);
    settle = null;
  };

  const recognition = new Recognition();
  recognition.continuous = true;
  recognition.interimResults = true;
  recognition.lang = navigator.language || "en-US";
  // The whole session, not the delta: with interim results on, each event
  // carries every result so far, so joining them is the running transcript.
  recognition.onresult = (event) => {
    const parts: string[] = [];
    for (let i = 0; i < event.results.length; i += 1) {
      parts.push(event.results[i][0]?.transcript ?? "");
    }
    transcript = parts.join(" ");
  };
  recognition.onerror = (event) => {
    // "aborted" is our own cancel()/stop() coming back round. "no-speech" is
    // a silent stretch, which is not a fault while the session is still open —
    // onend restarts it below.
    if (event.error === "aborted") return;
    if (event.error === "no-speech" && state === "recording") return;
    onTrouble?.(
      event.error === "not-allowed" || event.error === "service-not-allowed"
        ? "Microphone access was refused. Allow it for this app, then try again."
        : event.error === "no-speech"
          ? "Didn't catch that — nothing was said."
          : "Dictation couldn't transcribe that recording.",
    );
  };
  recognition.onend = () => {
    if (state !== "recording") {
      finish();
      return;
    }
    window.clearTimeout(restartTimer);
    restartTimer = window.setTimeout(() => {
      if (state !== "recording") return;
      try {
        recognition.start();
      } catch {
        onTrouble?.("Dictation couldn't stay connected.");
        finish();
      }
    }, RESTART_MS);
  };

  try {
    recognition.start();
  } catch (error) {
    release();
    throw error instanceof Error ? error : new Error(String(error));
  }

  return {
    stop: () =>
      new Promise<string>((resolve) => {
        if (state === "done") {
          resolve(transcript.replace(/\s+/g, " ").trim());
          return;
        }
        settle = resolve;
        if (state === "stopping") return;
        state = "stopping";
        window.clearTimeout(restartTimer);
        try {
          recognition.stop();
          // onend normally lands first and settles this; the timer is the
          // floor for a recogniser that never comes back.
          finaliseTimer = window.setTimeout(finish, FINALISE_MS);
        } catch {
          finish();
        }
      }),
    cancel: () => {
      if (state === "done") return;
      state = "done";
      transcript = "";
      try {
        recognition.abort();
      } catch {
        // Already down; releasing below is all that is left to do.
      }
      release();
      settle?.("");
      settle = null;
    },
  };
}
