// Custom session templates stored in localStorage, merged with server defaults.
import { api, type SessionTemplate } from "./api";

const CUSTOM_TEMPLATES_KEY = "mydevenv2.customTemplates.v1";

export interface TemplateContext {
  cwd: string | null;
  repoRoot: string | null;
  repoName: string | null;
}

function normalizeTemplate(template: Partial<SessionTemplate>): SessionTemplate {
  return {
    name: template.name?.trim() || "Unnamed preset",
    description: template.description?.trim() || "",
    command: Array.isArray(template.command) ? template.command : null,
    cwd: template.cwd?.trim() || null,
    env: Array.isArray(template.env) ? template.env : [],
    default_name: template.default_name?.trim() || null,
    match_repo_names: Array.isArray(template.match_repo_names)
      ? template.match_repo_names.map((value) => value.trim()).filter(Boolean)
      : [],
    match_path_prefixes: Array.isArray(template.match_path_prefixes)
      ? template.match_path_prefixes.map((value) => value.trim()).filter(Boolean)
      : [],
    tags: Array.isArray(template.tags)
      ? template.tags.map((value) => value.trim()).filter(Boolean)
      : [],
  };
}

function slugify(value: string): string {
  return value
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 32) || "session";
}

function normalizePath(value: string | null | undefined): string | null {
  const trimmed = value?.trim().replace(/^\/+|\/+$/g, "");
  return trimmed ? trimmed.toLowerCase() : null;
}

export function getCustomTemplates(): SessionTemplate[] {
  try {
    const raw = localStorage.getItem(CUSTOM_TEMPLATES_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (t): t is SessionTemplate =>
        t && typeof t.name === "string" && typeof t.description === "string",
    ).map(normalizeTemplate);
  } catch {
    return [];
  }
}

export function saveCustomTemplates(templates: SessionTemplate[]) {
  try {
    localStorage.setItem(CUSTOM_TEMPLATES_KEY, JSON.stringify(templates));
  } catch {
    /* quota / private mode — non-fatal */
  }
}

export function addCustomTemplate(template: SessionTemplate) {
  const templates = getCustomTemplates();
  templates.push(normalizeTemplate(template));
  saveCustomTemplates(templates);
}

export function updateCustomTemplate(index: number, template: SessionTemplate) {
  const templates = getCustomTemplates();
  if (index >= 0 && index < templates.length) {
    templates[index] = normalizeTemplate(template);
    saveCustomTemplates(templates);
  }
}

export function deleteCustomTemplate(index: number) {
  const templates = getCustomTemplates();
  if (index >= 0 && index < templates.length) {
    templates.splice(index, 1);
    saveCustomTemplates(templates);
  }
}

/** Merge server-provided templates with user custom templates. */
export function mergeTemplates(
  serverTemplates: SessionTemplate[],
): SessionTemplate[] {
  return [...serverTemplates.map(normalizeTemplate), ...getCustomTemplates()];
}

export async function resolveTemplateContext(
  cwd?: string,
): Promise<TemplateContext> {
  const trimmed = cwd?.trim() || null;
  try {
    const status = await api.gitStatus(trimmed ?? "");
    if (status.is_repo === false || !status.repo) {
      return { cwd: trimmed, repoRoot: null, repoName: null };
    }
    const repoRoot = status.repo;
    return {
      cwd: trimmed ?? repoRoot,
      repoRoot,
      repoName: repoRoot.split("/").pop() || repoRoot,
    };
  } catch {
    return { cwd: trimmed, repoRoot: null, repoName: null };
  }
}

export function templateMatchesContext(
  template: SessionTemplate,
  context: TemplateContext | null,
): boolean {
  if (!context) {
    return (
      (template.match_repo_names?.length ?? 0) === 0 &&
      (template.match_path_prefixes?.length ?? 0) === 0
    );
  }
  const repoNames = template.match_repo_names ?? [];
  const pathPrefixes = template.match_path_prefixes ?? [];
  const repoName = context.repoName?.trim().toLowerCase() || null;
  const cwd = normalizePath(context.cwd);
  const repoRoot = normalizePath(context.repoRoot);

  const repoMatches =
    repoNames.length === 0 ||
    (repoName ? repoNames.some((value) => value.trim().toLowerCase() === repoName) : false);
  const pathMatches =
    pathPrefixes.length === 0 ||
    pathPrefixes.some((prefixRaw) => {
      const prefix = normalizePath(prefixRaw);
      if (!prefix) return false;
      return [cwd, repoRoot].some(
        (value) => value === prefix || Boolean(value?.startsWith(`${prefix}/`)),
      );
    });
  return repoMatches && pathMatches;
}

function templateMatchScore(
  template: SessionTemplate,
  context: TemplateContext | null,
): number {
  if (!context) return 0;
  let score = 0;
  if (templateMatchesContext(template, context)) score += 100;
  score += (template.match_repo_names?.length ?? 0) * 10;
  score += (template.match_path_prefixes?.length ?? 0) * 5;
  if (context.repoName && (template.default_name ?? "").includes("{repo_name}")) score += 2;
  if (context.repoRoot && (template.cwd ?? "").includes("{repo}")) score += 2;
  return score;
}

export function sortTemplatesForContext(
  templates: SessionTemplate[],
  context: TemplateContext | null,
): SessionTemplate[] {
  return [...templates].sort((a, b) => {
    const score = templateMatchScore(b, context) - templateMatchScore(a, context);
    if (score !== 0) return score;
    return a.name.localeCompare(b.name);
  });
}

export function fillTemplateString(
  value: string,
  template: SessionTemplate,
  context: TemplateContext | null,
  sessionName?: string,
): string {
  const replacements: Record<string, string> = {
    "{preset}": template.name,
    "{session}": sessionName ?? "",
    "{cwd}": context?.cwd ?? "",
    "{repo}": context?.repoRoot ?? "",
    "{repo_name}": context?.repoName ?? "",
    "{timestamp}": String(Date.now() % 1000),
  };
  let out = value;
  for (const [token, replacement] of Object.entries(replacements)) {
    out = out.split(token).join(replacement);
  }
  return out.trim();
}

export function buildDefaultSessionName(
  template: SessionTemplate,
  context: TemplateContext | null,
): string {
  const pattern =
    template.default_name?.trim() ||
    (template.name === "Shell"
      ? "shell-{timestamp}"
      : `${slugify(template.name)}-{timestamp}`);
  const rendered = fillTemplateString(pattern, template, context)
    .replace(/--+/g, "-")
    .replace(/^-+|-+$/g, "");
  return rendered || `session-${Date.now() % 1000}`;
}

export function resolveTemplateLaunch(
  template: SessionTemplate,
  context: TemplateContext | null,
  sessionName: string,
): {
  command: string[] | undefined;
  cwd: string | undefined;
  env: [string, string][];
} {
  const command = template.command?.map((arg) =>
    fillTemplateString(arg, template, context, sessionName),
  );
  let cwd = (
    template.cwd
      ? fillTemplateString(template.cwd, template, context, sessionName)
      : context?.cwd || ""
  ).trim();
  if (template.cwd?.includes("{repo}") && !context?.repoRoot) {
    cwd = context?.cwd?.trim() || "";
  }
  const env = template.env.map(([key, value]) => [
    key,
    fillTemplateString(value, template, context, sessionName),
  ]) as [string, string][];
  return {
    command: command && command.length > 0 ? command : undefined,
    cwd: cwd || undefined,
    env,
  };
}
