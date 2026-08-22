// Refs the server speaks — an actor's `identity_ref`, a project's `slug` — are
// stable machine handles, not names. Every surface that renders one wants the
// human name the estate loaded for it, and every surface should fall back the
// same way when the name is not (yet) loaded: show the raw ref, and keep the
// raw ref in a `title` so the machine handle is never lost.
//
// AuditBrowser resolved actors inline first (`actor()?.display_name ?? ref`);
// this is that pattern extracted so Board, Backlog, the work-item detail and
// the audit log all resolve — and fall back — identically.

export interface NamedActor {
  identity_ref: string;
  display_name?: string | null;
}

export interface NamedProject {
  slug: string;
  name?: string | null;
}

/**
 * The display name for an actor `identity_ref`, or the ref itself when the
 * loaded list does not name it (unknown ref, or a blank `display_name`). The
 * raw ref is what a caller should always keep in a `title`.
 */
export function actorName(actors: readonly NamedActor[], ref: string): string {
  if (!ref) return ref;
  const found = actors.find((actor) => actor.identity_ref === ref);
  const name = found?.display_name?.trim();
  return name ? name : ref;
}

/**
 * The display name for a project `slug`, or the slug itself when the loaded
 * list does not name it. As with actors, the raw slug belongs in a `title`.
 */
export function projectName(projects: readonly NamedProject[], slug: string): string {
  if (!slug) return slug;
  const found = projects.find((project) => project.slug === slug);
  const name = found?.name?.trim();
  return name ? name : slug;
}
