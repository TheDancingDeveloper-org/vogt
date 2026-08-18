/** Canonical user-facing product identity.
 *
 * Compatibility identifiers intentionally do not live here. Browser storage,
 * native package, notification-channel and engine protocol names retain their
 * historic `mydevenv2` values so an upgrade does not discard local state or
 * install a second app.
 */
export const APP_NAME = "Vogt";
export const APP_SHORT_NAME = "Vogt";
export const APP_DESCRIPTION =
  "AI-native product development environment for projects, work, and coding sessions.";

export function productDocumentTitle(surface?: string): string {
  return surface ? `${surface} · ${APP_NAME}` : APP_NAME;
}
