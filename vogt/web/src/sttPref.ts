// Device-local speech-to-text preferences and the vocabulary bias a server
// transcriber is given.
//
// On a phone the on-device recognizer is chosen first, and it mishears every
// name it has never been taught — project slugs, session names, "shell" as
// "show". A server transcriber (Whisper) can be handed those names as a bias
// prompt, so this exists for the person whose device recognizer keeps
// mangling the vocabulary: turn it on and captured audio goes to the server
// with the names attached instead.

const PREFER_SERVER_STT_KEY = "vogt.assistant.stt.prefer_server";

/** Whether this device should transcribe via the server ahead of any on-device recognizer. */
export function getPreferServerStt(): boolean {
  try {
    return localStorage.getItem(PREFER_SERVER_STT_KEY) === "1";
  } catch {
    return false;
  }
}

export function setPreferServerStt(on: boolean): void {
  try {
    localStorage.setItem(PREFER_SERVER_STT_KEY, on ? "1" : "0");
  } catch {
    // localStorage unavailable — the preference simply does not persist.
  }
}

/** Words worth teaching the server transcriber, beyond the project names: the
 *  session vocabulary a supervisor request is built from. Kept short — a bias
 *  prompt is a hint, not a document. */
const DOMAIN_TERMS = [
  "shell",
  "session",
  "terminal",
  "Vogt",
  "backlog",
  "work item",
] as const;

/**
 * The vocabulary bias prompt for a server transcription, built from the
 * project slugs this client knows plus a few fixed domain terms. Empty when
 * there are no slugs and nothing to bias toward, so the caller can omit the
 * field entirely. Bounded so a large registry does not send a paragraph.
 */
export function sttVocabularyPrompt(slugs: readonly string[]): string {
  const projects = slugs
    .map((slug) => slug.trim())
    .filter((slug) => slug.length > 0)
    .slice(0, 64);
  const parts: string[] = [];
  if (projects.length > 0) {
    parts.push(`Project names: ${projects.join(", ")}.`);
  }
  parts.push(`Terms: ${DOMAIN_TERMS.join(", ")}.`);
  return parts.join(" ");
}
