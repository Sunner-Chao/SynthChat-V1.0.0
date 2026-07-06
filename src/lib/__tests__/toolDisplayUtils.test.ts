import { describe, it, expect } from "vitest";
import {
  rawObject,
  rawString,
  rawNumber,
  parseTerminalOutput,
  toolEventPayload,
} from "../toolDisplayUtils";
import type { ToolEvent } from "../types";

describe("rawObject", () => {
  it("returns the object for plain objects", () => {
    const obj = { foo: "bar" };
    expect(rawObject(obj)).toBe(obj);
  });

  it("returns empty object for non-objects", () => {
    expect(rawObject(null)).toEqual({});
    expect(rawObject("string")).toEqual({});
    expect(rawObject([1, 2])).toEqual({});
    expect(rawObject(42)).toEqual({});
    expect(rawObject(undefined)).toEqual({});
  });
});

describe("rawString", () => {
  it("returns trimmed string for non-empty strings", () => {
    expect(rawString("  hello  ")).toBe("hello");
    expect(rawString("world")).toBe("world");
  });

  it("returns empty string for empty or blank input", () => {
    expect(rawString("")).toBe("");
    expect(rawString("   ")).toBe("");
    expect(rawString(null)).toBe("");
    expect(rawString(42)).toBe("");
    expect(rawString(undefined)).toBe("");
  });
});

describe("rawNumber", () => {
  it("returns number for finite number values", () => {
    expect(rawNumber(42)).toBe(42);
    expect(rawNumber(-5.5)).toBe(-5.5);
    expect(rawNumber(0)).toBe(0);
  });

  it("parses integer strings", () => {
    expect(rawNumber("42")).toBe(42);
    expect(rawNumber("-10")).toBe(-10);
  });

  it("returns undefined for non-numeric values", () => {
    expect(rawNumber("not a number")).toBeUndefined();
    expect(rawNumber(null)).toBeUndefined();
    expect(rawNumber(undefined)).toBeUndefined();
    expect(rawNumber(Infinity)).toBeUndefined();
    expect(rawNumber(NaN)).toBeUndefined();
  });
});

describe("parseTerminalOutput", () => {
  it("returns empty object for empty input", () => {
    expect(parseTerminalOutput("")).toEqual({});
    expect(parseTerminalOutput("   ")).toEqual({});
  });

  it("returns empty object for non-structured output", () => {
    expect(parseTerminalOutput("plain output text")).toEqual({});
    // The structured format requires a specific multi-line layout with cwd/exitCode/stdout/stderr.
    // Non-matching input should return empty.
    expect(parseTerminalOutput("some output\nwithout structure")).toEqual({});
  });
});

describe("toolEventPayload", () => {
  it("returns empty object when raw is missing", () => {
    const event = { raw: null } as unknown as ToolEvent;
    expect(toolEventPayload(event)).toEqual({});
  });

  it("returns payload from raw.payload", () => {
    const payload = { result: "success" };
    const event = { raw: { payload } } as unknown as ToolEvent;
    expect(toolEventPayload(event)).toBe(payload);
  });

  it("returns empty object when payload is not an object", () => {
    const event = { raw: { payload: "string" } } as unknown as ToolEvent;
    expect(toolEventPayload(event)).toEqual({});
  });
});
