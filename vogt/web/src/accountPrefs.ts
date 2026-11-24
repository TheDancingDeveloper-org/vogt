// Per-account settings, kept in the core and cached in this browser.
//
// A setting saved here follows the login, not the device: the phone and the
// desktop restore the same Inbox filter. The core is the source of truth
// (`preference.get` / `preference.set`, audited per actor); localStorage is
// only an offline cache, read first so a reload paints the saved state
// before the network answers, and overwritten by whatever the core says.
//
// Generic on purpose. The Inbox filter is the first key; Board and Backlog
// keep their own localStorage filters today and can move onto this without
// a second implementation — give `accountPreference` a key and a parser.

import { createSignal, type Accessor } from "solid-js";
import { getPreferences, setPreference } from "./vogtApi";

const CACHE_PREFIX = "vogt.pref.v1:";

export type PreferenceSource = "default" | "cache" | "server";

export interface AccountPreference<T> {
  /** The current value: the server's once read, else the cached copy. */
  value: Accessor<T>;
  /** Where `value` came from — "server" once the core has answered. */
  source: Accessor<PreferenceSource>;
  /** Read the core (de-duplicated while one read is in the air). */
  load: () => Promise<void>;
  /** Save `next` for this account. Optimistic: the value changes now, and a
   *  failed write leaves it applied locally and cached for this device. */
  save: (next: T, reason: string) => Promise<void>;
  /** Back to the default, on the server too (`{}` clears a key). */
  clear: (reason: string) => Promise<void>;
  /** Tests only: forget everything, as a fresh page load would. */
  reset: () => void;
}

interface Options<T> {
  empty: T;
  /** Turn a stored object back into a value; never throws. */
  parse: (raw: Record<string, unknown>) => T;
  /** The object stored for a value; `{}` means "the default". */
  serialize: (value: T) => Record<string, unknown>;
}

const registry: AccountPreference<unknown>[] = [];

function readCache(key: string): Record<string, unknown> | null {
  try {
    const raw = localStorage.getItem(CACHE_PREFIX + key);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    return parsed && typeof parsed === "object" && !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

function writeCache(key: string, value: Record<string, unknown>): void {
  try {
    if (Object.keys(value).length === 0) localStorage.removeItem(CACHE_PREFIX + key);
    else localStorage.setItem(CACHE_PREFIX + key, JSON.stringify(value));
  } catch {
    /* the cache is a convenience; the core holds the setting */
  }
}

export function accountPreference<T>(key: string, options: Options<T>): AccountPreference<T> {
  const initial = (): [T, PreferenceSource] => {
    const cached = readCache(key);
    return cached ? [options.parse(cached), "cache"] : [options.empty, "default"];
  };
  const [first, firstSource] = initial();
  const [value, setValue] = createSignal<T>(first);
  const [source, setSource] = createSignal<PreferenceSource>(firstSource);
  let inFlight: Promise<void> | null = null;
  // A save that lands while a read is in the air must not be overwritten by
  // that read's older answer.
  let writes = 0;

  const apply = (raw: Record<string, unknown>) => {
    setValue(() => (Object.keys(raw).length ? options.parse(raw) : options.empty));
    writeCache(key, raw);
  };

  const load = (): Promise<void> => {
    if (inFlight) return inFlight;
    const before = writes;
    inFlight = (async () => {
      try {
        const answer = await getPreferences(key);
        if (writes !== before) return;
        const found = answer.preferences.find((row) => row.key === key);
        apply(found?.value ?? {});
        setSource("server");
      } catch {
        // An older core (404) or no network: the cached copy stands. Not an
        // error the person needs to see — the filter still works locally.
      } finally {
        inFlight = null;
      }
    })();
    return inFlight;
  };

  const save = async (next: T, reason: string): Promise<void> => {
    writes += 1;
    const raw = options.serialize(next);
    apply(raw);
    try {
      const answer = await setPreference(key, raw, reason);
      apply(answer.preference.value);
      setSource("server");
    } catch {
      // Kept locally; the next successful load reconciles with the core.
    }
  };

  const preference: AccountPreference<T> = {
    value,
    source,
    load,
    save,
    clear: (reason) => save(options.empty, reason),
    reset: () => {
      const [again, againSource] = initial();
      setValue(() => again);
      setSource(againSource);
      inFlight = null;
      writes = 0;
    },
  };
  registry.push(preference as AccountPreference<unknown>);
  return preference;
}

/** Tests: reset every account preference to what a fresh page would read. */
export function resetAccountPreferences(): void {
  for (const preference of registry) preference.reset();
}
