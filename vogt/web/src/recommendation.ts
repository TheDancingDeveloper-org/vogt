// WI-996 E0b: an overseer's pick on a permission dialog, shown beside the
// person's own choice and never pre-selected. Read defensively: it is server
// JSON, so anything malformed shows nothing.
export function recommendationCopy(rec: unknown): string | null {
  if (!rec || typeof rec !== "object") return null;
  const { option, label, by } = rec as Record<string, unknown>;
  if ((typeof option !== "number" && typeof option !== "string") || typeof label !== "string") return null;
  const who = typeof by === "string" && by ? by : "An overseer";
  return `${who} recommends ${option}. ${label}`;
}
