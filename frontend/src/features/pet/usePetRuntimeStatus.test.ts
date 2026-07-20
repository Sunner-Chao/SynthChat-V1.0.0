import { describe, expect, it } from "vitest";
import {
  selectPetProfile,
  statusForRun,
} from "./usePetRuntimeStatus";

describe("pet runtime state", () => {
  it("prefers the active Profile over defaults", () => {
    const selected = selectPetProfile([
      { id: "default", displayName: "Default", isDefault: true, isActive: false },
      { id: "work", displayName: "Work", isDefault: false, isActive: true },
    ] as never);

    expect(selected?.id).toBe("work");
  });

  it("maps active Run states without legacy Agent semantics", () => {
    expect(statusForRun({ status: "running" } as never)).toMatchObject({ phase: "thinking" });
    expect(statusForRun({ status: "waitingApproval" } as never)).toMatchObject({ phase: "approval" });
    expect(statusForRun({ status: "waitingClarification" } as never)).toMatchObject({ phase: "clarification" });
    expect(statusForRun(null)).toMatchObject({ phase: "ready" });
  });
});
