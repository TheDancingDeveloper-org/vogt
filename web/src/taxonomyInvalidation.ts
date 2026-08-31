/** Write-side seam for the shared taxonomy cache (#417). */

export type TaxonomyKind =
  | "projects"
  | "actors"
  | "workflows"
  | "labels"
  | "initiatives";

let invalidator: ((kind?: TaxonomyKind) => void) | null = null;

export function registerTaxonomyInvalidator(
  callback: (kind?: TaxonomyKind) => void,
): void {
  invalidator = callback;
}

export function invalidateTaxonomy(kind?: TaxonomyKind): void {
  invalidator?.(kind);
}
