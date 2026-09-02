import { describe, expect, it } from "vitest";
import { sanitizeBase } from "../api";

// #521: the API base is prefixed onto every request URL with the bearer token
// attached, and it is read from localStorage. A poisoned value must not be able
// to repoint calls at an attacker origin or a non-http scheme.
describe("sanitizeBase — localStorage-controlled API base", () => {
  it("keeps a plain http(s) origin, dropping any trailing path", () => {
    expect(sanitizeBase("https://box.example")).toBe("https://box.example");
    expect(sanitizeBase("https://box.example/")).toBe("https://box.example");
    expect(sanitizeBase("http://100.64.0.1:8080")).toBe("http://100.64.0.1:8080");
  });

  it("refuses non-http schemes", () => {
    expect(sanitizeBase("javascript:alert(1)")).toBeNull();
    expect(sanitizeBase("data:text/html,evil")).toBeNull();
    expect(sanitizeBase("file:///etc/passwd")).toBeNull();
  });

  it("refuses embedded credentials and a path/query/fragment riding along", () => {
    expect(sanitizeBase("https://user:pass@box.example")).toBeNull();
    expect(sanitizeBase("https://box.example/api/steal")).toBeNull();
    expect(sanitizeBase("https://box.example?x=1")).toBeNull();
    expect(sanitizeBase("https://box.example#frag")).toBeNull();
  });

  it("refuses garbage", () => {
    expect(sanitizeBase("not a url")).toBeNull();
    expect(sanitizeBase("")).toBeNull();
  });
});
