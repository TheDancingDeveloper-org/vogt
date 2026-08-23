// The first-run wizard (#292), increment 2: the identity step.
//
// The fake sits at `fetch`, under `installApi.ts`, so these tests exercise
// the real paths (`/api/install/status`, `/api/install/bootstrap`), the real
// request bodies, and the real handling of the core's refusal shapes —
// rather than asserting the mock agreed with itself.

import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import SetupWizard from "../SetupWizard";
import { bootstrapInstall, fetchInstallStatus } from "../installApi";

type FetchArgs = [input: RequestInfo | URL, init?: RequestInit];
let fetchMock: ReturnType<typeof vi.fn<(...args: FetchArgs) => Promise<Response>>>;

const jsonResponse = (status: number, body: unknown): Response =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });

const bootstrapResult = {
  actor: {
    id: "act-1",
    identity_ref: "human:ada-lovelace",
    display_name: "Ada Lovelace",
    kind: "human",
  },
  token: { id: "tok-1", name: "first-run browser token", scopes: ["admin"] },
  secret: "vogt_first-run-secret",
  warning: "This is the only time the secret is shown.",
};

beforeEach(() => {
  localStorage.clear();
  fetchMock = vi.fn<(...args: FetchArgs) => Promise<Response>>();
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

describe("installApi", () => {
  it("reads install mode from the install status route", async () => {
    fetchMock.mockResolvedValue(jsonResponse(200, { install_mode: true }));
    await expect(fetchInstallStatus()).resolves.toEqual({ install_mode: true });
    expect(String(fetchMock.mock.calls[0]![0])).toBe("/api/install/status");
  });

  it("treats an unreachable or older deployment as no install mode", async () => {
    fetchMock.mockRejectedValue(new TypeError("Failed to fetch"));
    await expect(fetchInstallStatus()).resolves.toBeNull();

    fetchMock.mockResolvedValue(jsonResponse(404, { error: "not found" }));
    await expect(fetchInstallStatus()).resolves.toBeNull();
  });

  it("posts the display name and surfaces the core's refusal message", async () => {
    fetchMock.mockResolvedValue(
      jsonResponse(409, {
        error: { code: "install_closed", message: "install mode is closed" },
      }),
    );
    await expect(bootstrapInstall("Ada")).rejects.toThrow(
      "install mode is closed",
    );
    const [url, init] = fetchMock.mock.calls[0]!;
    expect(String(url)).toBe("/api/install/bootstrap");
    expect(init?.method).toBe("POST");
    expect(JSON.parse(String(init?.body))).toEqual({ display_name: "Ada" });
  });
});

describe("#292 — the identity step", () => {
  it("claims the instance and shows the secret exactly once, with equivalents", async () => {
    fetchMock.mockResolvedValue(jsonResponse(200, bootstrapResult));
    render(() => (
      <SetupWizard onSignIn={() => {}} onAuthenticated={async () => {}} />
    ));

    // The wizard names the whole journey, with identity as the active step.
    expect(screen.getByRole("list", { name: "Setup steps" })).toHaveTextContent(
      "Identity",
    );

    await fireEvent.input(screen.getByPlaceholderText("Ada Lovelace"), {
      target: { value: "Ada Lovelace" },
    });
    await fireEvent.click(
      screen.getByRole("button", { name: "Claim instance & mint my token" }),
    );

    await waitFor(() =>
      expect(screen.getByTestId("setup-secret")).toHaveTextContent(
        "vogt_first-run-secret",
      ),
    );
    expect(screen.getByText("human:ada-lovelace")).toBeInTheDocument();
    // The CLI and MCP equivalents are one disclosure away, not hidden.
    expect(
      screen.getByText("Use it from a terminal or an agent"),
    ).toBeInTheDocument();
  });

  it("refuses an empty name without calling the server", async () => {
    render(() => (
      <SetupWizard onSignIn={() => {}} onAuthenticated={async () => {}} />
    ));
    await fireEvent.click(
      screen.getByRole("button", { name: "Claim instance & mint my token" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Your name is required",
    );
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("shows the core's refusal when the door has already closed", async () => {
    fetchMock.mockResolvedValue(
      jsonResponse(409, {
        error: { code: "install_closed", message: "install mode is closed" },
      }),
    );
    render(() => (
      <SetupWizard onSignIn={() => {}} onAuthenticated={async () => {}} />
    ));
    await fireEvent.input(screen.getByPlaceholderText("Ada Lovelace"), {
      target: { value: "Eve" },
    });
    await fireEvent.click(
      screen.getByRole("button", { name: "Claim instance & mint my token" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "install mode is closed",
    );
  });

  it("hands the minted secret to the ordinary sign-in path on continue", async () => {
    fetchMock.mockResolvedValue(jsonResponse(200, bootstrapResult));
    const onAuthenticated = vi.fn(async () => {});
    render(() => (
      <SetupWizard onSignIn={() => {}} onAuthenticated={onAuthenticated} />
    ));
    await fireEvent.input(screen.getByPlaceholderText("Ada Lovelace"), {
      target: { value: "Ada Lovelace" },
    });
    await fireEvent.click(
      screen.getByRole("button", { name: "Claim instance & mint my token" }),
    );
    await fireEvent.click(
      await screen.findByRole("button", { name: "Continue to Vogt" }),
    );
    await waitFor(() =>
      expect(onAuthenticated).toHaveBeenCalledWith("vogt_first-run-secret", ""),
    );
  });

  it("explains a front door that refuses the core token, and offers sign-in", async () => {
    fetchMock.mockResolvedValue(jsonResponse(200, bootstrapResult));
    const onSignIn = vi.fn();
    render(() => (
      <SetupWizard
        onSignIn={onSignIn}
        onAuthenticated={async () => {
          throw new Error("401");
        }}
      />
    ));
    await fireEvent.input(screen.getByPlaceholderText("Ada Lovelace"), {
      target: { value: "Ada Lovelace" },
    });
    await fireEvent.click(
      screen.getByRole("button", { name: "Claim instance & mint my token" }),
    );
    await fireEvent.click(
      await screen.findByRole("button", { name: "Continue to Vogt" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "token namespace",
    );
    // The secret stays visible: the reader has guidance and their credential.
    expect(screen.getByTestId("setup-secret")).toHaveTextContent(
      "vogt_first-run-secret",
    );
    await fireEvent.click(screen.getByRole("button", { name: "Go to sign-in" }));
    expect(onSignIn).toHaveBeenCalled();
  });

  it("offers the ordinary gate to a reader who already holds a token", async () => {
    const onSignIn = vi.fn();
    render(() => (
      <SetupWizard onSignIn={onSignIn} onAuthenticated={async () => {}} />
    ));
    await fireEvent.click(
      screen.getByRole("button", { name: "Sign in instead" }),
    );
    expect(onSignIn).toHaveBeenCalled();
  });
});
