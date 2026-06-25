// Custom session templates stored in localStorage, merged with server defaults.
import type { SessionTemplate } from "./api";

const CUSTOM_TEMPLATES_KEY = "mydevenv2.customTemplates.v1";

export function getCustomTemplates(): SessionTemplate[] {
  try {
    const raw = localStorage.getItem(CUSTOM_TEMPLATES_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (t): t is SessionTemplate =>
        t && typeof t.name === "string" && typeof t.description === "string",
    );
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
  templates.push(template);
  saveCustomTemplates(templates);
}

export function updateCustomTemplate(index: number, template: SessionTemplate) {
  const templates = getCustomTemplates();
  if (index >= 0 && index < templates.length) {
    templates[index] = template;
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
  return [...serverTemplates, ...getCustomTemplates()];
}
