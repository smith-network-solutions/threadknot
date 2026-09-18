import { Fragment, type ReactNode } from "react";

export function searchHighlightWords(query: string): string[] {
  return [...new Set((query.match(/[\p{L}\p{N}]+/gu) ?? []).map((word) => word.toLowerCase()))].slice(0, 64);
}

// Mirror the index's conservative, distance-one spelling fallback. Work on
// Unicode characters so an accented letter is not treated as several bytes.
function oneEditApart(left: string, right: string): boolean {
  const a = Array.from(left);
  const b = Array.from(right);
  if (Math.abs(a.length - b.length) > 1) return false;
  let i = 0;
  while (i < Math.min(a.length, b.length) && a[i] === b[i]) i++;
  if (i === Math.min(a.length, b.length)) return true;
  if (a.length < b.length) return a.slice(i).join("") === b.slice(i + 1).join("");
  if (a.length > b.length) return a.slice(i + 1).join("") === b.slice(i).join("");
  return a.slice(i + 1).join("") === b.slice(i + 1).join("") || (
    i + 1 < a.length && a[i] === b[i + 1] && a[i + 1] === b[i] &&
    a.slice(i + 2).join("") === b.slice(i + 2).join("")
  );
}

/** Keep result text as React text nodes, never HTML from a conversation.
 * Mark the actual word, including its completion, without changing its case. */
export function SearchHighlight({ text, words, fuzzy = false, substring = false }: {
  text: string;
  words: readonly string[];
  fuzzy?: boolean;
  /** The instant title fallback also accepts substrings within a word. */
  substring?: boolean;
}) {
  if (!words.length) return <>{text}</>;
  const parts: ReactNode[] = [];
  let end = 0;
  for (const match of text.matchAll(/[\p{L}\p{N}]+/gu)) {
    const token = match[0].toLowerCase();
    if (!words.some((word) => token === word ||
      (substring && token.includes(word)) ||
      (Array.from(word).length >= 2 && token.startsWith(word)) ||
      (fuzzy && Array.from(word).length >= 5 && oneEditApart(word, token)))) continue;
    const start = match.index!;
    parts.push(
      <Fragment key={start}>
        {text.slice(end, start)}
        <mark className="search-result-highlight">{match[0]}</mark>
      </Fragment>,
    );
    end = start + match[0].length;
  }
  parts.push(text.slice(end));
  return <>{parts}</>;
}
