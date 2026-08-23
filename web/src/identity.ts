/** Canonical user-facing product identity.
 *
 * Compatibility identifiers intentionally do not live here. Browser-storage
 * keys were renamed to `vogt.*` under #271 with a one-shot migration (see
 * `storageMigration.ts`), but the native package, notification-channel and
 * engine protocol names retain their historic `mydevenv2` values so an upgrade
 * does not install a second app or drop native state.
 */
export const APP_NAME = "Vogt";
export const APP_SHORT_NAME = "Vogt";
export const APP_DESCRIPTION =
  "AI-native product development environment for projects, work, and coding sessions.";

export function productDocumentTitle(surface?: string): string {
  return surface ? `${surface} · ${APP_NAME}` : APP_NAME;
}
