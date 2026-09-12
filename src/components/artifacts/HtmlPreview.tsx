import { useEffect, useRef, useState } from "react";

type Props = { url: string; title: string; className?: string };
export function HtmlPreview(props: Props) {
  return <HtmlPreviewDocument key={props.url} {...props} />;
}

/** The frame has its own HTTP policy and opaque origin. Fetch in the parent
 * so neither file credentials nor the app's CSP enter the rendered document. */
function HtmlPreviewDocument({ url, title, className }: Props) {
  const frame = useRef<HTMLIFrameElement>(null);
  const [html, setHtml] = useState<string | null>(null);
  const [ready, setReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const source = new URL("/html-preview", new URL(url, window.location.href));
  useEffect(() => {
    const controller = new AbortController();
    fetch(url, { signal: controller.signal })
      .then(response => {
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        return response.text();
      })
      .then(setHtml)
      .catch(error => { if (!controller.signal.aborted) setError(String(error.message ?? error)); });
    return () => controller.abort();
  }, [url]);
  useEffect(() => {
    if (ready && html !== null) frame.current?.contentWindow?.postMessage({ type: "threadknot.html-preview", html }, "*");
  }, [ready, html]);
  if (error) return <div className="artifact-preview-status is-error">HTML preview unavailable: {error}</div>;
  return <iframe ref={frame} className={className} src={source.toString()} onLoad={() => setReady(true)}
    sandbox="allow-scripts" referrerPolicy="no-referrer" title={title} />;
}
