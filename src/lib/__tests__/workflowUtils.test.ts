import { describe, it, expect } from "vitest";
import { workflowToolOriginSummaryText } from "../workflowUtils";

describe("workflowToolOriginSummaryText", () => {
  it("returns empty string for empty array", () => {
    expect(workflowToolOriginSummaryText([])).toBe("");
  });

  it("translates known origin identifiers", () => {
    expect(workflowToolOriginSummaryText(["provider_native"])).toBe("provider native");
    expect(workflowToolOriginSummaryText(["planner_json"])).toBe("planner JSON");
    expect(workflowToolOriginSummaryText(["hermes_markup"])).toBe("Hermes markup");
  });

  it("converts underscores to spaces for unknown origins", () => {
    expect(workflowToolOriginSummaryText(["custom_origin"])).toBe("custom origin");
  });

  it("joins multiple origins with comma and space", () => {
    const result = workflowToolOriginSummaryText(["provider_native", "planner_json"]);
    expect(result).toBe("provider native, planner JSON");
  });
});
