/** Canonical user-facing product identity.
 *
 * Compatibility identifiers intentionally do not live here. Browser-storage
 * keys and ephemeral event names were renamed to the `vogt.*` / `vogt:*` prefix
 * (#271), with a one-shot startup migration in `storageMigration.ts` so no
 * local state is lost. Native package, notification-channel, IndexedDB and
 * engine protocol names still retain their historic `mydevenv2` values so an
 * upgrade does not strand native settings or install a second app.
 */
export const APP_NAME = "Vogt";
export const APP_SHORT_NAME = "Vogt";
export const APP_DESCRIPTION =
  "AI-native product development environment for projects, work, and coding sessions.";

export function productDocumentTitle(surface?: string): string {
  return surface ? `${surface} · ${APP_NAME}` : APP_NAME;
}
