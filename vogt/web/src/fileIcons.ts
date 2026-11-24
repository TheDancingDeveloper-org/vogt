/** Stable, text-only markers for the narrow file rail. */
export function getFileIcon(path: string): string {
  const ext = path.split(".").pop()?.toLowerCase() || "";
  const basename = path.split("/").pop()?.toLowerCase() || "";

  const specialFiles: Record<string, string> = {
    "package.json": "PKG",
    "package-lock.json": "LOCK",
    "cargo.toml": "PKG",
    "cargo.lock": "LOCK",
    dockerfile: "CTR",
    "docker-compose.yml": "CTR",
    "readme.md": "DOC",
    license: "DOC",
    makefile: "MAKE",
    ".gitignore": "GIT",
    ".env": "ENV",
  };
  if (specialFiles[basename]) return specialFiles[basename];

  const iconMap: Record<string, string> = {
    js: "JS", jsx: "JSX", ts: "TS", tsx: "TSX", rs: "RS", py: "PY",
    java: "JAVA", go: "GO", rb: "RB", php: "PHP", c: "C", cpp: "C++",
    h: "H", cs: "C#", swift: "SW", kt: "KT", html: "HTML", css: "CSS",
    scss: "SCSS", sass: "SASS", less: "LESS", vue: "VUE", svelte: "SV",
    json: "JSON", yaml: "YAML", yml: "YAML", toml: "TOML", xml: "XML",
    csv: "CSV", md: "MD", txt: "TXT", pdf: "PDF", doc: "DOC", docx: "DOC",
    png: "IMG", jpg: "IMG", jpeg: "IMG", gif: "IMG", svg: "SVG", ico: "ICO",
    zip: "ZIP", tar: "TAR", gz: "GZ", rar: "RAR", sh: "SH", bash: "SH",
    zsh: "SH", fish: "SH", sql: "SQL", db: "DB", sqlite: "DB", log: "LOG",
  };
  return iconMap[ext] || "FILE";
}

export function getFolderIcon(isOpen: boolean): string {
  return isOpen ? "DIR−" : "DIR";
}

export function getGitStatusIcon(status?: "modified" | "untracked" | "deleted"): string | null {
  const statusMap = { modified: "●", untracked: "?", deleted: "✕" };
  return status ? statusMap[status] : null;
}
