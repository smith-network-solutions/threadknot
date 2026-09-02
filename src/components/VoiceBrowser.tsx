import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { VoiceSummary } from "../lib/protocol";
import { useStore } from "../state/store";

/**
 * Paginated, searchable picker over the ElevenLabs voices the connected
 * account can use. Previews play through the serving machine's speakers via
 * `voice.preview` — the webview's CSP cannot fetch the sample URLs itself,
 * and the speakers that matter are the ones the conversation will use.
 */
export function VoiceBrowser({
  selectedId,
  onSelect,
  onClose,
}: {
  selectedId?: string;
  onSelect: (voice: VoiceSummary) => void;
  onClose: () => void;
}) {
  const { actions } = useStore();
  const [query, setQuery] = useState("");
  const [category, setCategory] = useState("");
  const [voices, setVoices] = useState<VoiceSummary[]>([]);
  const [cursor, setCursor] = useState<string | undefined>();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [previewingId, setPreviewingId] = useState<string | null>(null);
  const generation = useRef(0);

  // Debounced, generation-guarded search: a stale response never lands on a
  // newer query's list.
  useEffect(() => {
    const gen = ++generation.current;
    setLoading(true);
    setError(null);
    const timer = setTimeout(() => {
      actions
        .searchVoices({
          ...(query.trim() ? { search: query.trim() } : {}),
          ...(category ? { category } : {}),
        })
        .then((page) => {
          if (generation.current !== gen) return;
          setVoices(page.voices);
          setCursor(page.nextPageToken);
          setLoading(false);
        })
        .catch((e) => {
          if (generation.current !== gen) return;
          setError(e instanceof Error ? e.message : String(e));
          setLoading(false);
        });
    }, 300);
    return () => clearTimeout(timer);
  }, [actions, query, category]);

  function loadMore() {
    if (!cursor || loading) return;
    const gen = generation.current;
    setLoading(true);
    actions
      .searchVoices({
        ...(query.trim() ? { search: query.trim() } : {}),
        ...(category ? { category } : {}),
        pageToken: cursor,
      })
      .then((page) => {
        if (generation.current !== gen) return;
        setVoices((prev) => [...prev, ...page.voices]);
        setCursor(page.nextPageToken);
        setLoading(false);
      })
      .catch((e) => {
        if (generation.current !== gen) return;
        setError(e instanceof Error ? e.message : String(e));
        setLoading(false);
      });
  }

  function togglePreview(voice: VoiceSummary) {
    if (previewingId === voice.id) {
      setPreviewingId(null);
      void actions.stopVoicePreview();
      return;
    }
    setPreviewingId(voice.id);
    // The free path: the API's own sample file. The server enforces one
    // preview at a time.
    void actions
      .previewVoice(voice.id, voice.previewUrl)
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
  }

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        e.stopPropagation();
        void actions.stopVoicePreview();
        onClose();
      }
    }
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [actions, onClose]);

  const labelChips = (voice: VoiceSummary) =>
    [
      voice.labels.gender,
      voice.labels.age,
      voice.labels.accent,
      voice.labels.descriptive,
      voice.labels.useCase,
    ].filter((l): l is string => !!l);

  return createPortal(
    <div
      className="vb-backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) {
          void actions.stopVoicePreview();
          onClose();
        }
      }}
    >
      <section className="vb-modal" role="dialog" aria-modal="true" aria-label="Choose a voice">
        <header className="vb-head">
          <span className="vb-title">voices</span>
          <input
            type="text"
            className="vb-search"
            placeholder="search voices…"
            value={query}
            autoFocus
            onChange={(e) => setQuery(e.target.value)}
          />
          <select
            className="settings-select vb-category"
            value={category}
            onChange={(e) => setCategory(e.target.value)}
          >
            <option value="">all voices</option>
            <option value="premade">default</option>
            <option value="cloned">personal</option>
            <option value="generated">generated</option>
            <option value="professional">professional</option>
          </select>
          <button type="button" className="vb-close" aria-label="Close" onClick={() => {
            void actions.stopVoicePreview();
            onClose();
          }}>
            ×
          </button>
        </header>

        <div className="vb-list">
          {voices.map((voice) => (
            <div
              key={voice.id}
              className={`vb-card ${voice.id === selectedId ? "selected" : ""}`}
            >
              <div className="vb-card-main">
                <span className="vb-card-name">{voice.name ?? voice.id}</span>
                {voice.category && <span className="vb-card-cat">{voice.category}</span>}
                <span className="vb-card-labels">
                  {labelChips(voice).map((label) => (
                    <span key={label} className="vb-chip">
                      {label}
                    </span>
                  ))}
                </span>
              </div>
              <div className="vb-card-actions">
                <button
                  type="button"
                  className={`settings-toggle ${previewingId === voice.id ? "on" : ""}`}
                  onClick={() => togglePreview(voice)}
                >
                  {previewingId === voice.id ? "stop" : "play"}
                </button>
                <button
                  type="button"
                  className="settings-toggle primary"
                  onClick={() => {
                    void actions.stopVoicePreview();
                    onSelect(voice);
                  }}
                >
                  {voice.id === selectedId ? "selected" : "use voice"}
                </button>
              </div>
            </div>
          ))}
          {!loading && voices.length === 0 && !error && (
            <div className="vb-empty">no voices match</div>
          )}
        </div>

        <footer className="vb-foot">
          {error && <span className="vb-error">{error}</span>}
          {loading && <span className="vb-status">loading…</span>}
          {!loading && cursor && (
            <button type="button" className="settings-toggle" onClick={loadMore}>
              load more
            </button>
          )}
          <span className="vb-count">{voices.length} shown</span>
        </footer>
      </section>
    </div>,
    document.body,
  );
}
