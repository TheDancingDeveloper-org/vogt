// Keyed rail rows (WI-2328).
//
// The rail and the Sessions page list render `flattenRail` entries. Rendered
// with `<For>` over the entry objects, a row was replaced whenever its entry
// was rebuilt (a section move, a tally or a lift changing), and every activity
// event remounted rows. These lists key rows by `entry.key` instead, and a row
// reads its fields through memos that fire only when that field's value
// changes, so a rebuilt entry with the same values touches nothing and a
// section move is a DOM move, not a rebuild.
import { createMemo, type Accessor } from "solid-js";
import type { RailEntry, RailRowEntry } from "./sessionRail";

export interface RailRowFields {
  session: Accessor<RailRowEntry["session"]>;
  section: Accessor<RailRowEntry["section"]>;
  depth: Accessor<RailRowEntry["depth"]>;
  lifted: Accessor<RailRowEntry["lifted"]>;
  expanded: Accessor<RailRowEntry["expanded"]>;
  tally: Accessor<RailRowEntry["tally"]>;
}

/** The entries by key, and the key list, for a keyed `<For>`. The key list
 *  only changes identity when the order or membership does. */
export function createRailKeys(entries: Accessor<readonly RailEntry[]>): {
  byKey: Accessor<Map<string, RailEntry>>;
  keys: Accessor<string[]>;
} {
  const byKey = createMemo(() => new Map(entries().map((entry) => [entry.key, entry])));
  const keys = createMemo(
    () => entries().map((entry) => entry.key),
    [],
    { equals: (a, b) => a.length === b.length && a.every((key, i) => key === b[i]) },
  );
  return { byKey, keys };
}

/** One row's fields, each changing only when its own value does. Call inside
 *  the `<For>` child so the memos are owned by (and disposed with) the row. */
export function createRailRowFields(
  byKey: Accessor<Map<string, RailEntry>>,
  key: string,
): RailRowFields {
  let last: RailRowEntry | undefined;
  // A key leaving the list keeps its last entry until the row is disposed.
  const entry = createMemo(() => {
    const found = byKey().get(key);
    if (found && found.kind === "row") last = found;
    return last as RailRowEntry;
  });
  const field = <T,>(read: (row: RailRowEntry) => T, equals: (a: T, b: T) => boolean = Object.is) =>
    createMemo(() => read(entry()), undefined, { equals: equals as (a: T | undefined, b: T) => boolean }) as Accessor<T>;
  return {
    session: field((row) => row.session),
    section: field((row) => row.section),
    depth: field((row) => row.depth),
    lifted: field((row) => row.lifted),
    expanded: field((row) => row.expanded),
    tally: field((row) => row.tally, (a, b) => JSON.stringify(a) === JSON.stringify(b)),
  };
}
