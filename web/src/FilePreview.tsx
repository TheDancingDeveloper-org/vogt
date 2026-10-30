import { Component, Show, createEffect, createSignal, onCleanup } from "solid-js";
import { api } from "./api";
import { previewKind } from "./filePreview";

interface Props {
  path: string;
  /** Leave the preview for the editor. */
  onEdit?: (path: string) => void;
  onError?: (message: string) => void;
}

/**
 * One file, shown rather than edited.
 *
 * `.html` renders inside an iframe whose `sandbox` attribute grants nothing:
 * no scripts, no same-origin, no forms, no popups, no top navigation. The
 * document arrives as a blob URL, so it also has no origin of its own and
 * cannot reach the app even if a later change loosened the sandbox. "Source"
 * is the same bytes as text. An image renders from the read payload; any
 * other file is its text.
 */
const FilePreview: Component<Props> = (props) => {
  const kind = () => previewKind(props.path);
  const [mode, setMode] = createSignal<"rendered" | "source">("rendered");
  const [source, setSource] = createSignal<string | null>(null);
  const [mediaUrl, setMediaUrl] = createSignal<string | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);

  let blobUrl: string | null = null;
  const dropBlob = () => {
    if (blobUrl) URL.revokeObjectURL(blobUrl);
    blobUrl = null;
  };
  onCleanup(dropBlob);

  createEffect(() => {
    const path = props.path;
    const renderHtml = kind() === "html" && mode() === "rendered";
    const renderImage = kind() === "image";
    setLoading(true);
    setError(null);
    setSource(null);
    dropBlob();
    setMediaUrl(null);
    let cancelled = false;
    onCleanup(() => { cancelled = true; });
    void api.readFile(path).then((file) => {
      if (cancelled) return;
      const text = file.content;
      if (renderHtml && text !== null) {
        dropBlob();
        blobUrl = URL.createObjectURL(new Blob([text], { type: "text/html" }));
        setMediaUrl(blobUrl);
      } else if (renderImage && file.is_binary && file.content_base64) {
        setMediaUrl(`data:application/octet-stream;base64,${file.content_base64}`);
      } else if (text !== null) {
        setSource(text);
      } else {
        setError("This file has no text to show.");
      }
      setLoading(false);
    }).catch((e: unknown) => {
      if (cancelled) return;
      const message = e instanceof Error ? e.message : String(e);
      setError(message);
      setLoading(false);
      props.onError?.(message);
    });
  });

  return (
    <section class="file-preview" aria-label={`Preview of ${props.path}`}>
      <header class="file-preview-bar">
        <span class="file-preview-path" title={props.path}>{props.path}</span>
        <Show when={kind() === "html"}>
          <div class="file-preview-toggle" role="group" aria-label="HTML view">
            <button
              type="button"
              aria-pressed={mode() === "rendered"}
              onClick={() => setMode("rendered")}
            >Rendered</button>
            <button
              type="button"
              aria-pressed={mode() === "source"}
              onClick={() => setMode("source")}
            >Source</button>
          </div>
        </Show>
        <Show when={props.onEdit}>
          <button type="button" onClick={() => props.onEdit?.(props.path)}>Edit</button>
        </Show>
      </header>
      <div class="file-preview-body">
        <Show when={error()}>
          {(message) => <p class="file-preview-error" role="alert">{message()}</p>}
        </Show>
        <Show when={kind() === "html" && mode() === "rendered" && mediaUrl()}>
          <iframe
            class="file-preview-frame"
            title={`Rendered ${props.path}`}
            sandbox=""
            referrerpolicy="no-referrer"
            src={mediaUrl() ?? ""}
          />
        </Show>
        <Show when={kind() === "image" && mediaUrl() && !error()}>
          <img class="file-preview-image" alt={props.path} src={mediaUrl() ?? ""} />
        </Show>
        <Show when={source() !== null}>
          <pre class="file-preview-source">{source()}</pre>
        </Show>
        <Show when={loading() && source() === null && !mediaUrl() && !error()}>
          <p class="file-preview-status">Loading…</p>
        </Show>
      </div>
    </section>
  );
};

export default FilePreview;
