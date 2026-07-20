import { describe, expect, it } from "vitest";
import { isPetWindowRoute } from "./route";

describe("isPetWindowRoute", () => {
  it("only selects the isolated pet route", () => {
    expect(isPetWindowRoute("?window=pet")).toBe(true);
    expect(isPetWindowRoute("?window=main")).toBe(false);
    expect(isPetWindowRoute("?pet=true")).toBe(false);
  });
});
