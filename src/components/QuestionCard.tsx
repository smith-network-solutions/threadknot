import { useEffect, useMemo, useState } from "react";
import type { Question } from "../lib/protocol";
import type { FeedItem } from "../state/feed";
import { useStore } from "../state/store";
import { CheckIcon, ChevronIcon } from "./icons";

type QItem = Extract<FeedItem, { type: "question" }>;

const OTHER = "__other__";

/** Is this question fully answered given current selections + custom text? */
function isAnswered(q: Question, sel: string[], custom: string): boolean {
  if (q.allowOther && custom.trim().length > 0) return true;
  return sel.filter((s) => s !== OTHER).length > 0;
}

/** Merge option selections + custom text into the wire answer array for a question. */
function toAnswer(sel: string[], custom: string): string[] {
  const out = sel.filter((s) => s !== OTHER);
  if (custom.trim().length > 0) out.push(custom.trim());
  return out;
}

export function QuestionCard({ item }: { item: QItem }) {
  const { actions } = useStore();
  const multiStep = item.questions.length > 1;
  const [step, setStep] = useState(0);

  // Per-question local draft: chosen option labels and custom "Other" text.
  const [sel, setSel] = useState<Record<string, string[]>>({});
  const [custom, setCustom] = useState<Record<string, string>>({});

  const active = item.questions[Math.min(step, item.questions.length - 1)];
  const disabled = item.answered || item.pending;

  const answeredSummary = useMemo(() => {
    const parts: string[] = [];
    for (const q of item.questions) {
      const a = item.answers[q.id];
      if (a && a.length) parts.push(...a);
    }
    return parts;
  }, [item.answers, item.questions]);

  const allAnswered = item.questions.every((q) =>
    isAnswered(q, sel[q.id] ?? [], custom[q.id] ?? ""),
  );

  function chooseOption(q: Question, label: string) {
    if (disabled) return;
    setSel((prev) => {
      const cur = prev[q.id] ?? [];
      let nextSel: string[];
      if (q.multiSelect) {
        nextSel = cur.includes(label) ? cur.filter((l) => l !== label) : [...cur, label];
      } else {
        nextSel = [label];
        // Single-select option supersedes any free-text.
        setCustom((c) => ({ ...c, [q.id]: "" }));
      }
      const next = { ...prev, [q.id]: nextSel };
      // Auto-advance single-select in a stepper.
      if (!q.multiSelect && multiStep && step < item.questions.length - 1) {
        window.setTimeout(() => setStep((s) => Math.min(s + 1, item.questions.length - 1)), 180);
      }
      return next;
    });
  }

  function setCustomText(q: Question, value: string) {
    if (disabled) return;
    setCustom((c) => ({ ...c, [q.id]: value }));
    // Free text supersedes a single-select option choice.
    if (!q.multiSelect && value.trim().length > 0) {
      setSel((prev) => ({ ...prev, [q.id]: [OTHER] }));
    }
  }

  // Number-key shortcuts 1-9 for the active question (when not typing).
  useEffect(() => {
    if (disabled || !active) return;
    function onKey(e: KeyboardEvent) {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const t = e.target;
      if (t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement) return;
      const d = Number.parseInt(e.key, 10);
      if (Number.isNaN(d) || d < 1 || d > 9) return;
      const opt = active.options[d - 1];
      if (!opt) return;
      e.preventDefault();
      chooseOption(active, opt.label);
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active, disabled, step]);

  function submit() {
    if (disabled || !allAnswered) return;
    const answers: Record<string, string[]> = {};
    for (const q of item.questions) {
      answers[q.id] = toAnswer(sel[q.id] ?? [], custom[q.id] ?? "");
    }
    actions.setQuestionAnswers(item.requestId, answers);
    void actions.respondQuestion(item.requestId, answers);
  }

  if (item.answered) {
    return (
      <div className="question-resolved">
        <CheckIcon size={12} />
        <span>
          Answered
          {answeredSummary.length > 0 ? `: ${answeredSummary.join(", ")}` : ""}
        </span>
      </div>
    );
  }

  if (!active) return null;

  return (
    <div className="question-card">
      <div className="question-head">
        <span className="question-eyebrow">{active.header || "Question"}</span>
        {multiStep && (
          <span className="question-step">
            {step + 1} of {item.questions.length}
          </span>
        )}
      </div>
      <p className="question-prompt">{active.question}</p>
      {active.multiSelect && <p className="question-hint">Select one or more.</p>}

      <div className="question-options">
        {active.options.map((opt, i) => {
          const chosen = (sel[active.id] ?? []).includes(opt.label);
          return (
            <button
              key={`${active.id}:${opt.label}`}
              type="button"
              className={`question-option${chosen ? " on" : ""}`}
              disabled={disabled}
              onClick={() => chooseOption(active, opt.label)}
            >
              <span className={`opt-marker${active.multiSelect ? " box" : ""}`}>
                {chosen && <CheckIcon size={11} />}
              </span>
              <span className="opt-body">
                <span className="opt-label">{opt.label}</span>
                {opt.description && opt.description !== opt.label && (
                  <span className="opt-desc">{opt.description}</span>
                )}
              </span>
              {i < 9 && <kbd className="opt-key">{i + 1}</kbd>}
            </button>
          );
        })}
      </div>

      {active.allowOther && (
        <input
          className="question-other"
          type={active.isSecret ? "password" : "text"}
          placeholder="Other…"
          value={custom[active.id] ?? ""}
          disabled={disabled}
          onChange={(e) => setCustomText(active, e.target.value)}
        />
      )}

      <div className="question-actions">
        {multiStep && (
          <div className="question-nav">
            <button
              className="btn tone-deny"
              disabled={step === 0}
              onClick={() => setStep((s) => Math.max(0, s - 1))}
            >
              <ChevronIcon size={13} className="flip" /> Back
            </button>
            <button
              className="btn tone-deny"
              disabled={step >= item.questions.length - 1}
              onClick={() => setStep((s) => Math.min(item.questions.length - 1, s + 1))}
            >
              Next <ChevronIcon size={13} />
            </button>
          </div>
        )}
        <button
          className="btn tone-allow question-submit"
          disabled={disabled || !allAnswered}
          onClick={submit}
        >
          {item.pending ? "Sending…" : "Submit answers"}
        </button>
      </div>
    </div>
  );
}
