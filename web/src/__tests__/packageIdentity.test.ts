import { describe, it, expect } from "vitest";
import pkg from "../../package.json";

// The npm package name is the last web surface still carrying the historic
// MyDevEnv2 identity (#271, web half). It is `private: true` and referenced
// nowhere else in the tree, so the rename is safe and this test pins it so a
// future edit cannot quietly reintroduce the legacy brand.
describe("web package identity (#271)", () => {
  it("uses the Vogt name", () => {
    expect(pkg.name).toBe("vogt-web");
  });

  it("carries no legacy MyDevEnv2 brand in its name", () => {
    expect(pkg.name).not.toMatch(/mydevenv2/i);
  });
});
