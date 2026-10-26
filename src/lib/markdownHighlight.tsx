import type { ReactNode } from "react";

/* Live Markdown styling for the composer.
 *
 * The output is painted *behind* a transparent textarea, so it has one hard
 * rule: every character of the draft appears exactly once, in order, and
 * nothing is added or removed. Syntax markers stay visible (dimmed) rather than
 * being hidden, and no style may change a glyph's advance width — the caret and
 * the selection belong to the textarea, and the moment the two layers disagree
 * about where a letter sits, typing visibly lands somewhere else. That is why
 * there is no font-weight, font-size, italic face or monospace swap here; see
 * the `.md-hl-*` rules in styles.css for what is used instead. */

type Rule = {
  re: RegExp;
  render: (m: RegExpExecArray, key: string) => ReactNode;
};

const mark = (text: string, key: string) => (
  <span key={key} className="md-hl-mark">
    {text}
  </span>
);

/** `open` + body + `close`, with the markers dimmed and the body styled. */
function wrapped(className: string, open: string, close = open) {
  return (m: RegExpExecArray, key: string) => (
    <span key={key} className={className}>
      {mark(open, `${key}o`)}
      {inline(m[1], `${key}b`)}
      {mark(close, `${key}c`)}
    </span>
  );
}

// Order matters: code first (nothing inside it is Markdown), then the longer
// markers before the shorter ones that share a character.
const INLINE: Rule[] = [
  {
    re: /`([^`\n]+)`/y,
    render: (m, key) => (
      <span key={key} className="md-hl-code">
        {mark("`", `${key}o`)}
        {m[1]}
        {mark("`", `${key}c`)}
      </span>
    ),
  },
  { re: /\*\*(?=\S)([^\n]*?\S)\*\*/y, render: wrapped("md-hl-strong", "**") },
  { re: /__(?=\S)([^\n]*?\S)__/y, render: wrapped("md-hl-strong", "__") },
  { re: /~~(?=\S)([^\n]*?\S)~~/y, render: wrapped("md-hl-strike", "~~") },
  { re: /\*(?=[^\s*])([^\n*]*?[^\s*])\*/y, render: wrapped("md-hl-em", "*") },
  { re: /(?<![\w])_(?=[^\s_])([^\n_]*?[^\s_])_(?![\w])/y, render: wrapped("md-hl-em", "_") },
  {
    re: /\[([^\]\n]+)\]\(([^)\s]+)\)/y,
    render: (m, key) => (
      <span key={key} className="md-hl-link">
        {mark("[", `${key}a`)}
        {inline(m[1], `${key}t`)}
        {mark(`](${m[2]})`, `${key}u`)}
      </span>
    ),
  },
  {
    re: /https?:\/\/[^\s<>()]+[^\s<>().,;:!?'"]/y,
    render: (m, key) => (
      <span key={key} className="md-hl-url">
        {m[0]}
      </span>
    ),
  },
];

/** Inline spans within one line. Plain runs are emitted as bare strings. */
function inline(text: string, keyBase: string): ReactNode[] {
  const out: ReactNode[] = [];
  let plain = "";
  let i = 0;
  while (i < text.length) {
    let matched = false;
    for (const rule of INLINE) {
      rule.re.lastIndex = i;
      const m = rule.re.exec(text);
      if (!m) continue;
      if (plain) {
        out.push(plain);
        plain = "";
      }
      out.push(rule.render(m, `${keyBase}-${i}`));
      i += m[0].length;
      matched = true;
      break;
    }
    if (!matched) {
      // A backslash escape keeps its next character literal.
      if (text[i] === "\\" && i + 1 < text.length) {
        plain += text.slice(i, i + 2);
        i += 2;
      } else {
        plain += text[i];
        i += 1;
      }
    }
  }
  if (plain) out.push(plain);
  return out;
}

const FENCE = /^(\s{0,3})(`{3,}|~{3,})(.*)$/;
const HEADING = /^(\s{0,3}#{1,6}\s+)(.*)$/;
const QUOTE = /^(\s{0,3}(?:>\s?)+)(.*)$/;
const LIST = /^(\s*(?:[-*+]|\d{1,9}[.)])\s+)(\[[ xX]\]\s+)?(.*)$/;
const RULE = /^\s{0,3}([-*_])(?:\s*\1){2,}\s*$/;

/** The draft as styled React nodes, character-for-character identical. */
export function highlightMarkdown(text: string): ReactNode[] {
  const lines = text.split("\n");
  const out: ReactNode[] = [];
  let fence: string | null = null;

  lines.forEach((line, n) => {
    const key = `l${n}`;
    if (n > 0) out.push("\n");

    const fenceMatch = line.match(FENCE);
    if (fence !== null) {
      const closes =
        fenceMatch !== null &&
        fenceMatch[2][0] === fence[0] &&
        fenceMatch[2].length >= fence.length &&
        fenceMatch[3].trim() === "";
      out.push(
        <span key={key} className={closes ? "md-hl-mark" : "md-hl-codeblock"}>
          {line}
        </span>,
      );
      if (closes) fence = null;
      return;
    }
    if (fenceMatch) {
      fence = fenceMatch[2];
      out.push(
        <span key={key} className="md-hl-mark">
          {line}
        </span>,
      );
      return;
    }

    if (RULE.test(line)) {
      out.push(
        <span key={key} className="md-hl-mark">
          {line}
        </span>,
      );
      return;
    }

    const heading = line.match(HEADING);
    if (heading) {
      out.push(
        <span key={key} className="md-hl-heading">
          {mark(heading[1], `${key}m`)}
          {inline(heading[2], key)}
        </span>,
      );
      return;
    }

    const quote = line.match(QUOTE);
    if (quote) {
      out.push(
        <span key={key} className="md-hl-quote">
          {mark(quote[1], `${key}m`)}
          {inline(quote[2], key)}
        </span>,
      );
      return;
    }

    const list = line.match(LIST);
    if (list) {
      out.push(
        <span key={key}>
          <span className="md-hl-bullet">{list[1]}</span>
          {list[2] && <span className="md-hl-bullet">{list[2]}</span>}
          {inline(list[3], key)}
        </span>,
      );
      return;
    }

    out.push(<span key={key}>{inline(line, key)}</span>);
  });

  return out;
}
