import { describe, it, expect } from "vitest";
import {
  formatDurationMs,
  runStateLabel,
  runPhaseLabel,
  isTerminalRunState,
  compactRunText,
  queueStatusLabel,
  shortRuntimeId,
  runtimePayloadRecord,
  agentLabel,
} from "../agentRunUtils";
import type { AgentDefinition } from "../types";

describe("formatDurationMs", () => {
  it("formats milliseconds under 1s", () => {
    expect(formatDurationMs(0)).toBe("0ms");
    expect(formatDurationMs(500)).toBe("500ms");
    expect(formatDurationMs(999)).toBe("999ms");
  });

  it("formats seconds under 1 minute", () => {
    expect(formatDurationMs(1000)).toBe("1.0s");
    expect(formatDurationMs(5500)).toBe("5.5s");
    expect(formatDurationMs(59999)).toBe("60.0s");
  });

  it("formats minutes and seconds", () => {
    expect(formatDurationMs(60_000)).toBe("1m 00s");
    expect(formatDurationMs(90_000)).toBe("1m 30s");
    expect(formatDurationMs(125_000)).toBe("2m 05s");
  });

  it("clamps negative values to 0", () => {
    expect(formatDurationMs(-100)).toBe("0ms");
  });
});

describe("runStateLabel", () => {
  it("returns Chinese label for known states", () => {
    expect(runStateLabel("pending")).toBe("排队中");
    expect(runStateLabel("running")).toBe("正在思考");
    expect(runStateLabel("completed")).toBe("已完成");
    expect(runStateLabel("failed")).toBe("失败");
    expect(runStateLabel("aborted")).toBe("已停止");
  });

  it("returns raw state for unknown values", () => {
    expect(runStateLabel("unknown_state")).toBe("unknown_state");
  });
});

describe("runPhaseLabel", () => {
  it("returns Chinese label for known phases", () => {
    expect(runPhaseLabel("planner_started")).toBe("开始规划");
    expect(runPhaseLabel("tool_started")).toBe("执行中...");
    expect(runPhaseLabel("llm_retry")).toBe("模型请求重试");
  });

  it("returns raw phase for unknown values", () => {
    expect(runPhaseLabel("custom_phase")).toBe("custom_phase");
  });
});

describe("isTerminalRunState", () => {
  it("returns true for terminal states", () => {
    expect(isTerminalRunState("completed")).toBe(true);
    expect(isTerminalRunState("failed")).toBe(true);
    expect(isTerminalRunState("aborted")).toBe(true);
  });

  it("returns false for non-terminal states", () => {
    expect(isTerminalRunState("running")).toBe(false);
    expect(isTerminalRunState("pending")).toBe(false);
    expect(isTerminalRunState(null)).toBe(false);
    expect(isTerminalRunState(undefined)).toBe(false);
  });
});

describe("compactRunText", () => {
  it("returns empty string for empty input", () => {
    expect(compactRunText("")).toBe("");
    expect(compactRunText(null)).toBe("");
    expect(compactRunText(undefined)).toBe("");
    expect(compactRunText("   ")).toBe("");
  });

  it("returns text unchanged when under limit", () => {
    const short = "Hello world";
    expect(compactRunText(short)).toBe(short);
  });

  it("truncates text at custom limit with ellipsis", () => {
    const long = "A".repeat(200);
    const result = compactRunText(long, 50);
    expect(result.length).toBe(53);
    expect(result.endsWith("...")).toBe(true);
  });
});

describe("queueStatusLabel", () => {
  it("returns Chinese label for known statuses", () => {
    expect(queueStatusLabel("pending")).toBe("排队中");
    expect(queueStatusLabel("running")).toBe("执行中");
    expect(queueStatusLabel("completed")).toBe("已完成");
    expect(queueStatusLabel("canceled")).toBe("已取消");
  });

  it("returns raw status for unknown values", () => {
    expect(queueStatusLabel("unknown")).toBe("unknown");
  });
});

describe("shortRuntimeId", () => {
  it("returns empty string for empty input", () => {
    expect(shortRuntimeId("")).toBe("");
    expect(shortRuntimeId(null)).toBe("");
    expect(shortRuntimeId(undefined)).toBe("");
  });

  it("returns short IDs unchanged", () => {
    expect(shortRuntimeId("abc-12345")).toBe("abc-12345");
  });

  it("shortens long UUIDs", () => {
    const longId = "run-abc12345-def67890-xyz99999";
    const result = shortRuntimeId(longId);
    expect(result.length).toBeLessThan(longId.length);
    expect(result).toContain("-");
  });
});

describe("runtimePayloadRecord", () => {
  it("returns null for non-object values", () => {
    expect(runtimePayloadRecord(null)).toBeNull();
    expect(runtimePayloadRecord("string")).toBeNull();
    expect(runtimePayloadRecord([1, 2])).toBeNull();
    expect(runtimePayloadRecord(42)).toBeNull();
  });

  it("returns the object for valid records", () => {
    const obj = { foo: "bar", count: 1 };
    expect(runtimePayloadRecord(obj)).toBe(obj);
  });
});

describe("agentLabel", () => {
  it("returns default label for null/undefined agent", () => {
    expect(agentLabel(null)).toBe("Default Agent");
    expect(agentLabel(undefined)).toBe("Default Agent");
  });

  it("returns agent name when available", () => {
    const agent = { id: "agent-1", name: "My Agent" } as AgentDefinition;
    expect(agentLabel(agent)).toBe("My Agent");
  });

  it("falls back to id when name is empty", () => {
    const agent = { id: "agent-1", name: "" } as AgentDefinition;
    expect(agentLabel(agent)).toBe("agent-1");
  });
});
