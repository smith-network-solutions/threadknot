import type { AttachmentMeta } from "./protocol";

/** A message selected as the context for the next human turn. Replies are
 * serialized into the outgoing text so the relationship survives replay and
 * is understandable to every agent/device on the thread. */
export interface ReplyTarget {
  id: string;
  kind: "user" | "assistant";
  author: string;
  text: string;
  attachments?: AttachmentMeta[];
  timestamp?: string;
}

export interface ParsedReply {
  author: string;
  quote: string;
  body: string;
}

const MAX_QUOTED_CHARS = 1800;

export function replyPreview(text: string, max = 240): string {
  const compact = text.replace(/\s+/g, " ").trim();
  return compact.length > max ? `${compact.slice(0, max - 1).trimEnd()}…` : compact;
}

/** Keep a large answer from turning a short follow-up into another huge turn. */
export function quotedReplyText(text: string): string {
  const normalized = text.trim();
  if (normalized.length <= MAX_QUOTED_CHARS) return normalized;
  return `${normalized.slice(0, MAX_QUOTED_CHARS).trimEnd()}…`;
}

export function formatReply(target: ReplyTarget, body: string): string {
  const source = target.text.trim() || (target.attachments?.length ? "[image attachment]" : "");
  const quoted = quotedReplyText(source)
    .split("\n")
    .map((line) => `> ${line}`)
    .join("\n");
  const context = `[Replying to ${target.author}]\n${quoted}`;
  return body.trim() ? `${context}\n\n${body.trim()}` : context;
}

/** Pull the transport-only reply envelope back out before rendering a message.
 * Older messages and ordinary text simply return null. */
export function parseReply(text: string): ParsedReply | null {
  const match = text.match(/^\[Replying to ([^\]\n]+)\]\n([\s\S]*)$/);
  if (!match) return null;

  const rest = match[2];
  const separator = rest.indexOf("\n\n");
  const quoteBlock = separator >= 0 ? rest.slice(0, separator) : rest;
  if (!quoteBlock || quoteBlock.split("\n").some((line) => !line.startsWith(">"))) {
    return null;
  }

  return {
    author: match[1].trim(),
    quote: quoteBlock
      .split("\n")
      .map((line) => line.replace(/^> ?/, ""))
      .join("\n")
      .trim(),
    body: separator >= 0 ? rest.slice(separator + 2).trim() : "",
  };
}
