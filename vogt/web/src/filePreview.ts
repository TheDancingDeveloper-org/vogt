/** Which files the browser previews instead of handing to the editor. */

export type PreviewKind = "html" | "image" | "text";

const IMAGE_EXTENSIONS = new Set([
  "png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "ico", "avif",
]);

export function extensionOf(path: string): string {
  const name = path.split("/").pop() ?? path;
  const dot = name.lastIndexOf(".");
  return dot <= 0 ? "" : name.slice(dot + 1).toLowerCase();
}

/** `.html` and `.htm` render in the sandboxed preview; images render from the
 *  read payload; everything else is source. */
export function previewKind(path: string): PreviewKind {
  const ext = extensionOf(path);
  if (ext === "html" || ext === "htm") return "html";
  if (IMAGE_EXTENSIONS.has(ext)) return "image";
  return "text";
}
