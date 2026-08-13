import { memo, useState, type ReactNode } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeHighlight from "rehype-highlight";
import "highlight.js/styles/github-dark.css";
import { copyText } from "../lib/format";
import { CheckIcon, CopyIcon } from "./icons";

/** Extract "language-xxx" from a className list, if present. */
function langOf(className: unknown): string | null {
  if (typeof className !== "string") return null;
  const m = className.match(/language-([\w+#-]+)/);
  return m ? m[1] : null;
}

/** Recursively collect text from React children (for the copy-raw-code button). */
function textOf(node: ReactNode): string {
  if (node == null || node === false) return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textOf).join("");
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const props = (node as any)?.props;
  if (props && "children" in props) return textOf(props.children);
  return "";
}

const FILE_EXTENSION = /(?:^|\/)[\w.-]+\.(?:tsx?|jsx?|css|scss|less|json|md|rs|py|go|java|kt|swift|html?|vue|svelte|sql|ya?ml|toml|sh|bash)\b/i;

function filePathFromLink(href: string | undefined, label: string): string | null {
  const labelMatch = label.trim().match(FILE_EXTENSION);
  let candidate = "";
  let localLink = !href;

  if (href) {
    try {
      const url = new URL(href, window.location.href);
      const localHost =
        url.hostname === "localhost" ||
        url.hostname === "127.0.0.1" ||
        url.hostname === "[::1]";
      if (url.protocol === "file:" || localHost || !url.hostname) {
        localLink = true;
        candidate = decodeURIComponent(url.pathname).replace(/^\/+/, "");
      }
    } catch {
      localLink = true;
      candidate = href;
    }
  }

  const pathMatch = candidate.match(FILE_EXTENSION);
  if (pathMatch) {
    const start = pathMatch.index ?? 0;
    return candidate.slice(start);
  }
  return localLink ? labelMatch?.[0] ?? null : null;
}

function CodeBlock({ children, lang }: { children: ReactNode; lang: string }) {
  const [done, setDone] = useState(false);
  const raw = textOf(children).replace(/\n$/, "");
  return (
    <div className="code-block">
      <div className="code-bar">
        <span className="code-lang">{lang}</span>
        <button
          type="button"
          className="code-copy"
          onClick={async () => {
            if (await copyText(raw)) {
              setDone(true);
              setTimeout(() => setDone(false), 1400);
            }
          }}
        >
          {done ? <CheckIcon size={12} /> : <CopyIcon size={12} />}
          <span>{done ? "Copied" : "Copy"}</span>
        </button>
      </div>
      <pre className="code-pre">{children}</pre>
    </div>
  );
}

function markdownComponents(onOpenFile?: (path: string) => void): Components {
  return {
  // Block code: react-markdown passes the <code> as the child of <pre>.
  pre({ children }) {
    // Pull language off the inner <code> element.
    let lang = "text";
    if (children && typeof children === "object" && "props" in children) {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      lang = langOf((children as any).props?.className) ?? "text";
    }
    return <CodeBlock lang={lang}>{children}</CodeBlock>;
  },
  code({ className, children, ...rest }) {
    // Inline code only — block code is rendered inside <pre> above, which keeps
    // the <code> intact; hljs classes still apply for highlighting there.
    const isBlock = typeof className === "string" && className.includes("language-");
    if (isBlock) {
      return (
        <code className={className} {...rest}>
          {children}
        </code>
      );
    }
    return <code className="inline-code">{children}</code>;
  },
  a({ children, href }) {
    const label = textOf(children);
    const filePath = onOpenFile ? filePathFromLink(href, label) : null;
    if (filePath) {
      return (
        <button
          type="button"
          className="md-file-link"
          title={`Open ${filePath} in Files`}
          onClick={() => {
            onOpenFile?.(filePath);
          }}
        >
          {children}
        </button>
      );
    }
    return (
      <a href={href} target="_blank" rel="noopener noreferrer">
        {children}
      </a>
    );
  },
  table({ children }) {
    return (
      <div className="md-table-wrap">
        <table>{children}</table>
      </div>
    );
  },
  };
}

export const Markdown = memo(function Markdown({
  text,
  streaming = false,
  onOpenFile,
}: {
  text: string;
  /** Live output is intentionally plain text; parse/highlight once at turn end. */
  streaming?: boolean;
  onOpenFile?: (path: string) => void;
}) {
  if (streaming) {
    return <div className="md md-streaming">{text}</div>;
  }
  return (
    <div className="md">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[[rehypeHighlight, { detect: true, ignoreMissing: true }]]}
        components={markdownComponents(onOpenFile)}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
});
