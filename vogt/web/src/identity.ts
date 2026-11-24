/** Canonical user-facing product identity. */
export const APP_NAME = "Vogt";
export const APP_SHORT_NAME = "Vogt";
export const APP_DESCRIPTION =
  "AI-native product development environment for projects, work, and coding sessions.";

export function productDocumentTitle(surface?: string): string {
  return surface ? `${surface} · ${APP_NAME}` : APP_NAME;
}
