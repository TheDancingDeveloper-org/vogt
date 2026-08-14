export const MAX_TERMINAL_INPUT_BYTES = 64 * 1024;

const encoder = new TextEncoder();

export function terminalInputBytes(data: string | ArrayBuffer): number {
  return typeof data === "string"
    ? encoder.encode(data).byteLength
    : data.byteLength;
}

export function terminalInputTooLarge(data: string | ArrayBuffer): boolean {
  return terminalInputBytes(data) > MAX_TERMINAL_INPUT_BYTES;
}

export function formatTerminalInputLimit(): string {
  return `${MAX_TERMINAL_INPUT_BYTES / 1024} KiB`;
}
