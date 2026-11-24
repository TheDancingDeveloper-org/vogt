import { BROWSER_STORAGE_KEYS, getStoragePrefs } from "./storagePrefs";

export interface AuthProfile {
  id: string;
  name: string;
  token: string;
  base: string;
  updated_at: string;
}

const STORAGE_KEY = BROWSER_STORAGE_KEYS.authProfiles;

function normalizeProfile(profile: AuthProfile): AuthProfile {
  return {
    id: profile.id,
    name: profile.name.trim(),
    token: profile.token.trim(),
    base: profile.base.trim().replace(/\/+$/, ""),
    updated_at: profile.updated_at,
  };
}

function nextId(): string {
  try {
    return crypto.randomUUID();
  } catch {
    return `auth-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
  }
}

export function listAuthProfiles(): AuthProfile[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    const next = parsed
      .filter(
        (entry): entry is AuthProfile =>
          !!entry &&
          typeof entry.id === "string" &&
          typeof entry.name === "string" &&
          typeof entry.token === "string" &&
          typeof entry.base === "string" &&
          typeof entry.updated_at === "string",
      )
      .map(normalizeProfile)
      .filter((entry) => entry.name && entry.token)
      .sort((a, b) => b.updated_at.localeCompare(a.updated_at));
    const limit = getStoragePrefs().maxAuthProfiles;
    const trimmed = limit <= 0 ? [] : next.slice(0, limit);
    if (JSON.stringify(trimmed) !== JSON.stringify(parsed)) {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(trimmed));
    }
    return trimmed;
  } catch {
    return [];
  }
}

function writeAuthProfiles(profiles: AuthProfile[]) {
  const limit = getStoragePrefs().maxAuthProfiles;
  const next = limit <= 0 ? [] : profiles.slice(0, limit);
  localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
}

export function saveAuthProfile(profile: {
  id?: string;
  name: string;
  token: string;
  base: string;
}): AuthProfile {
  const next = normalizeProfile({
    id: profile.id ?? nextId(),
    name: profile.name,
    token: profile.token,
    base: profile.base,
    updated_at: new Date().toISOString(),
  });

  const profiles = listAuthProfiles().filter((entry) => entry.id !== next.id);
  profiles.unshift(next);
  writeAuthProfiles(profiles);
  return next;
}

export function deleteAuthProfile(id: string) {
  const profiles = listAuthProfiles().filter((entry) => entry.id !== id);
  writeAuthProfiles(profiles);
}

export function clearAuthProfiles() {
  writeAuthProfiles([]);
}

export function trimAuthProfiles() {
  void listAuthProfiles();
}
