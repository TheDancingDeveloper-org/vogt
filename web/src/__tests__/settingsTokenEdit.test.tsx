// Regression: editing the Bearer token field must not re-run the open-time
// initialisation. The effect that seeds the form from storage used to track
// `token()` (read synchronously inside `validateAuth()`), so every keystroke
// reset the field to the stored token and fired another validation — the
// token could never be cleared or replaced from the phone.
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import Settings from "../Settings";
import { setToken } from "../api";

// Every request the modal makes at open resolves 200 with a minimal JSON
// body; only the auth checks are counted, since Settings also refreshes
// operational status, agent CLIs and storage when it opens.
const fetchMock = vi.fn(async () =>
  new Response(JSON.stringify({ version: "0.0.0" }), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  }),
);
const authChecks = () =>
  fetchMock.mock.calls.filter(([url]) => String(url).includes("/api/auth/check")).length;

beforeEach(() => {
  localStorage.clear();
  setToken("stored-token");
  fetchMock.mockClear();
  vi.stubGlobal("fetch", fetchMock);
});
afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
  setToken("");
});

describe("Settings — Bearer token field is editable", () => {
  it("keeps the typed value and does not revalidate per keystroke", async () => {
    render(() => <Settings open={true} onClose={() => {}} />);
    const input = screen.getByLabelText(/Bearer token/) as HTMLInputElement;
    expect(input.value).toBe("stored-token");
    // The open-time validation of the stored token is the only fetch allowed.
    await waitFor(() => expect(authChecks()).toBe(1));

    await fireEvent.input(input, { target: { value: "stored-toke" } });
    await fireEvent.input(input, { target: { value: "" } });
    await fireEvent.input(input, { target: { value: "new-token" } });

    expect(input.value).toBe("new-token");
    expect(screen.getByText("Token changed; validate before saving.")).toBeTruthy();
    // Still exactly the one open-time check: typing never triggered another.
    expect(authChecks()).toBe(1);
  });
});
