import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  useSyncExternalStore,
  type CSSProperties,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import type { Thread, ThreadPreview } from "../lib/protocol";
import { HERMES_HOME_PROJECT_ID, isQuickHomeProjectId } from "../lib/protocol";
import { timeAgo } from "../lib/format";
import { machineLook } from "./MachineAvatar";
import { useStore } from "../state/store";
import { AgentMark } from "./icons";
import { hermesPresence } from "./HermesPresence";

const SHOW_DELAY_MS = 350;
const CARD_WIDTH = 280;
const VIEW_MARGIN = 8;
const ANCHOR_GAP = 12;

export interface HoverCardHandle {
  hoverProps: {
    onPointerEnter: (e: React.PointerEvent<HTMLElement>) => void;
    onPointerLeave: () => void;
  };
  portal: ReactNode;
}

/**
 * Shared floating info-card behind the sidebar's thread + workspace hovers.
 * Mirrors AvatarHoverPreview's discipline (intent delay, fixed portal on
 * document.body, viewport clamp, hide on scroll / leave / unmount) but renders
 * a rectangular panel whose body is supplied LAZILY via `render`: the body only
 * mounts once the card is shown, so a thread card fires its preview fetch on
 * first hover rather than on every row render. `disabled` both blocks new shows
 * and retracts an open card, which is how an opening kebab menu, an inline
 * rename, or a drag suppresses it.
 */
export function useHoverCard({
  render,
  disabled = false,
  width = CARD_WIDTH,
}: {
  render: () => ReactNode;
  disabled?: boolean;
  width?: number;
}): HoverCardHandle {
  const timer = useRef<number | null>(null);
  const anchorRef = useRef<HTMLElement | null>(null);
  const cardRef = useRef<HTMLDivElement | null>(null);
  // Anchor rect captured at show time: `left` is derived from it synchronously,
  // `top` after we can measure the card (its height is content-dependent).
  const [anchor, setAnchor] = useState<DOMRect | null>(null);
  const [top, setTop] = useState(0);

  const hide = useCallback(() => {
    if (timer.current !== null) {
      clearTimeout(timer.current);
      timer.current = null;
    }
    setAnchor(null);
  }, []);

  // Delay before showing so a cursor merely passing over rows never flickers a
  // card into existence. Pointer events (not mouse events) let us skip the
  // timer on touch: tapping a thread in the mobile overlay synthesizes a mouse
  // pointer and then closes the sidebar WITHOUT a matching leave, which would
  // otherwise strand a card over the conversation. Only a real mouse on a
  // hover-capable medium ever arms the timer.
  const onPointerEnter = useCallback(
    (e: React.PointerEvent<HTMLElement>) => {
      if (disabled) return;
      if (e.pointerType !== "mouse") return;
      if (
        typeof window.matchMedia === "function" &&
        !window.matchMedia("(hover: hover)").matches
      )
        return;
      anchorRef.current = e.currentTarget;
      if (timer.current !== null) clearTimeout(timer.current);
      timer.current = window.setTimeout(() => {
        timer.current = null;
        const el = anchorRef.current;
        // Between arming and firing the row may have unmounted or its sidebar
        // may have closed out from under it (mobile overlay): only show for an
        // element still connected and actually laid out with a non-zero rect.
        if (!el || !el.isConnected) return;
        const rect = el.getBoundingClientRect();
        if (rect.width <= 0 || rect.height <= 0) return;
        setAnchor(rect);
      }, SHOW_DELAY_MS);
    },
    [disabled],
  );

  // A newly-true `disabled` (menu opened / rename started) cancels a pending
  // show and retracts a visible one.
  useEffect(() => {
    if (disabled) hide();
  }, [disabled, hide]);

  // Clamp vertically once the card is in the DOM, and again whenever its height
  // changes (the summary grows from a one-line shimmer to the fetched text).
  useLayoutEffect(() => {
    if (!anchor) return;
    const clamp = () => {
      const h = cardRef.current?.offsetHeight ?? 0;
      setTop(
        Math.max(
          VIEW_MARGIN,
          Math.min(anchor.top, window.innerHeight - h - VIEW_MARGIN),
        ),
      );
    };
    clamp();
    const card = cardRef.current;
    if (!card || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(clamp);
    ro.observe(card);
    return () => ro.disconnect();
  }, [anchor]);

  // Any scroll while open means the anchor slid out from under the card: hide.
  useEffect(() => {
    if (!anchor) return;
    const onScroll = () => hide();
    window.addEventListener("scroll", onScroll, true);
    return () => window.removeEventListener("scroll", onScroll, true);
  }, [anchor, hide]);

  // Never leave a timer running past unmount (the portal unmounts with the row
  // this hook lives inside).
  useEffect(
    () => () => {
      if (timer.current !== null) clearTimeout(timer.current);
    },
    [],
  );

  let portal: ReactNode = null;
  if (anchor && !disabled) {
    // Prefer sitting to the right of the sidebar edge so the card floats over
    // the work area; flip left when it would overflow, then clamp fully in.
    let left = anchor.right + ANCHOR_GAP;
    if (left + width + VIEW_MARGIN > window.innerWidth) {
      left = anchor.left - ANCHOR_GAP - width;
    }
    left = Math.max(VIEW_MARGIN, Math.min(left, window.innerWidth - width - VIEW_MARGIN));
    const style: CSSProperties = { top, left, width };
    portal = createPortal(
      <div ref={cardRef} className="hover-card" style={style} role="tooltip">
        {render()}
      </div>,
      document.body,
    );
  }

  return { hoverProps: { onPointerEnter, onPointerLeave: hide }, portal };
}

// ---- thread preview cache + fetch ----------------------------------------

/** Max distinct thread revisions kept resolved at once. Evicted oldest-first
 *  (Map preserves insertion order) so a long session can't leak unboundedly. */
const PREVIEW_CACHE_CAP = 100;

/** In-flight fetches keyed `${threadId}:${updatedAt}`, stored the instant a
 *  fetch starts so concurrent hovers of the same row share ONE request. The
 *  promise is retained on success regardless of which component started it, so
 *  re-hovering never refires the request (which rescans the whole JSONL log,
 *  worse over peer sockets). On failure the entry is deleted so a later hover
 *  retries a peer that has since come back online. */
const previewPromises = new Map<string, Promise<ThreadPreview>>();

/** Resolved values only, so `peekThreadPreview` never has to await. Kept in
 *  lockstep with `previewPromises` (a superset by key: every value key has a
 *  live promise; deletes clear both). */
const previewValues = new Map<string, ThreadPreview>();

// Reactive reads: rows subscribe so a hover-driven insert re-renders their
// "N turns" hint. Module-level cache stays module-level; only reads are made
// reactive via a bumped version counter.
const previewListeners = new Set<() => void>();
let previewVersion = 0;

function notifyPreviewChange(): void {
  previewVersion += 1;
  for (const l of previewListeners) l();
}

function subscribePreview(listener: () => void): () => void {
  previewListeners.add(listener);
  return () => {
    previewListeners.delete(listener);
  };
}

function previewKey(thread: Thread): string {
  return `${thread.id}:${thread.updatedAt}`;
}

function deletePreviewKey(key: string): void {
  previewPromises.delete(key);
  previewValues.delete(key);
}

/** Drop any cached revisions of this thread other than `keep` so an edited
 *  thread (fresh updatedAt) doesn't leave its prior entry behind forever. */
function evictOtherRevisions(threadId: string, keep: string): void {
  const prefix = `${threadId}:`;
  for (const k of previewPromises.keys()) {
    if (k !== keep && k.startsWith(prefix)) deletePreviewKey(k);
  }
}

/** Fetch a thread's preview once and share the promise. Resolved values are
 *  retained past the caller's lifetime; the alive-guarded setState belongs to
 *  the caller, not this cache. */
function loadThreadPreview(
  thread: Thread,
  fetcher: (threadId: string) => Promise<ThreadPreview>,
): Promise<ThreadPreview> {
  const key = previewKey(thread);
  const existing = previewPromises.get(key);
  if (existing) return existing;
  evictOtherRevisions(thread.id, key);
  const promise = fetcher(thread.id)
    .then((preview) => {
      previewValues.set(key, preview);
      notifyPreviewChange();
      return preview;
    })
    .catch((err) => {
      deletePreviewKey(key);
      throw err;
    });
  previewPromises.set(key, promise);
  // Cap after insert; delete oldest insertion(s) if over.
  while (previewPromises.size > PREVIEW_CACHE_CAP) {
    const oldest = previewPromises.keys().next().value as string | undefined;
    if (oldest === undefined || oldest === key) break;
    deletePreviewKey(oldest);
  }
  return promise;
}

/** Read a resolved preview without triggering a fetch. Used by the card-view
 *  meta row for its "N turns" hint (only shown when a hover already populated
 *  it). Never returns an unresolved promise. */
export function peekThreadPreview(thread: Thread): ThreadPreview | undefined {
  return previewValues.get(previewKey(thread));
}

/** Reactive form of `peekThreadPreview`: re-renders the caller when the cache
 *  gains this thread's value, so a hover fetch surfaces "N turns" immediately. */
export function usePeekThreadPreview(thread: Thread): ThreadPreview | undefined {
  useSyncExternalStore(subscribePreview, () => previewVersion);
  return peekThreadPreview(thread);
}

/** The thread card's body. Rendered only inside a visible hover card, so its
 *  effect (the lazy preview fetch) runs on first show. Lives within the store
 *  provider even though portaled, since React context flows through portals. */
export function ThreadHoverCardBody({ thread }: { thread: Thread }) {
  const { state, actions } = useStore();
  const key = previewKey(thread);
  const cached = peekThreadPreview(thread);
  const [preview, setPreview] = useState<ThreadPreview | null>(cached ?? null);
  const [phase, setPhase] = useState<"loading" | "ready" | "error">(
    cached ? "ready" : "loading",
  );

  // A resolved cache hit skips the network; otherwise the shared loader dedupes
  // concurrent hovers and retains the value past this card's lifetime, so
  // re-hovering never refires the request. Only the setState is life-guarded;
  // a failure never throws - it flips to the "unavailable" note below and the
  // loader drops its entry so a later hover retries.
  useEffect(() => {
    if (cached) return;
    let alive = true;
    setPhase("loading");
    loadThreadPreview(thread, actions.threadPreview)
      .then((p) => {
        if (!alive) return;
        setPreview(p);
        setPhase("ready");
      })
      .catch(() => {
        if (alive) setPhase("error");
      });
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  const localId = state.hello?.machineId;
  const isRemote = !!thread.machineId && !!localId && thread.machineId !== localId;
  const peer = isRemote
    ? state.peers.find((p) => p.machineId === thread.machineId)
    : undefined;
  const online = !isRemote || !!peer?.online;
  const machineLabel = isRemote ? machineLook(state, thread.machineId).name : "this machine";

  // Hermes chats wear their gateway photo + name; folder chats use their
  // workspace name (falling back to the project name).
  const hermesRec =
    thread.agent === "hermes"
      ? state.hermesAgents.find((a) => a.id === thread.settings.model)
      : undefined;
  const hermesAvatar = hermesRec?.avatar ?? hermesRec?.image;
  // Live gateway presence for the hermes-thread presence line below.
  const hermesStatus = hermesRec ? state.hermesStatuses[thread.settings.model] : undefined;
  const workspaceName =
    thread.projectId === HERMES_HOME_PROJECT_ID
      ? (hermesRec?.name ?? "Hermes agent")
      : isQuickHomeProjectId(thread.projectId)
        ? "Quick chats"
      : (state.workspaces.find((w) =>
          w.members.some((m) => m.projectId === thread.projectId),
        )?.name ??
        state.projects.find((p) => p.id === thread.projectId)?.name ??
        "workspace");

  // Prefer the human model name from the agent registry; fall back to the id.
  const agentInfo = state.hello?.agents.find((a) => a.id === thread.agent);
  const modelName =
    agentInfo?.models.find((m) => m.id === thread.settings.model)?.name ??
    thread.settings.model;
  const modelLine = [modelName, thread.settings.effort].filter(Boolean).join(" · ");

  const empty =
    phase === "ready" && preview
      ? preview.turnCount === 0 && !preview.summary
      : false;

  return (
    <>
      <div className="hover-card-head">
        {hermesAvatar ? (
          <span className="hover-card-avatar">
            <img src={hermesAvatar} alt="" />
          </span>
        ) : (
          <AgentMark agent={thread.agent} size={14} className="hover-card-mark" />
        )}
        <span className="hover-card-title">{thread.title || "Untitled thread"}</span>
        <span className={`status-dot st-${thread.status}`} />
      </div>

      <div className="hover-card-meta">
        <span className="hover-card-meta-line">{workspaceName}</span>
        <span className="hover-card-meta-line">
          <span className={`peer-dot${online ? " online" : ""}`} />
          {machineLabel}
          {online ? "" : " (offline)"}
        </span>
        {thread.agent === "hermes" &&
          hermesRec &&
          (() => {
            const p = hermesPresence(hermesStatus);
            return (
              <span className="hover-card-meta-line">
                <span className={`hermes-presence-inline ${p.kind}`} />
                {hermesRec.name} · {p.label}
              </span>
            );
          })()}
        {modelLine && <span className="hover-card-meta-line">{modelLine}</span>}
        <span className="hover-card-meta-line">
          created {timeAgo(thread.createdAt)} · updated {timeAgo(thread.updatedAt)}
        </span>
      </div>

      <div className="hover-card-summary">
        {phase === "loading" && <span className="hover-card-shimmer" />}
        {phase === "error" && (
          <span className="hover-card-dim">summary unavailable (machine offline?)</span>
        )}
        {phase === "ready" && empty && (
          <span className="hover-card-dim">no messages yet</span>
        )}
        {phase === "ready" && !empty && preview && (
          <>
            {preview.lastUser && (
              <div className="hover-card-you">you: {preview.lastUser}</div>
            )}
            {preview.summary && <div className="hover-card-quote">{preview.summary}</div>}
          </>
        )}
      </div>

      {phase === "ready" && preview && preview.turnCount > 0 && (
        <div className="hover-card-foot">
          {preview.turnCount} {preview.turnCount === 1 ? "turn" : "turns"}
        </div>
      )}
    </>
  );
}

/** One workspace root line for the workspace hover card. */
export interface WorkspaceHoverMember {
  machineLabel: string;
  path: string;
  online: boolean;
}

/** The workspace-header card's body: name + one line per root (machine, folder,
 *  online dot), then a thread-count / last-activity footer. Reuses the same
 *  floating-card shell as the thread card. */
export function WorkspaceHoverCardBody({
  name,
  members,
  threadCount,
  lastActivity,
}: {
  name: string;
  members: WorkspaceHoverMember[];
  threadCount: number;
  lastActivity?: string;
}) {
  return (
    <>
      <div className="hover-card-head">
        <span className="hover-card-title hover-card-title-lg">{name}</span>
      </div>
      <div className="hover-card-roots">
        {members.map((m) => (
          <div key={`${m.machineLabel}:${m.path}`} className="hover-card-root">
            <span className={`peer-dot${m.online ? " online" : ""}`} />
            <span className="hover-card-root-machine">{m.machineLabel}</span>
            <span className="hover-card-root-path">{m.path || "no path"}</span>
          </div>
        ))}
      </div>
      <div className="hover-card-foot">
        {threadCount} {threadCount === 1 ? "thread" : "threads"}
        {lastActivity ? ` · last active ${timeAgo(lastActivity)}` : ""}
      </div>
    </>
  );
}
