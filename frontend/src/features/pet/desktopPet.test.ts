import { describe, expect, it, vi } from "vitest";
import {
  PetWindowBridgeError,
  createPetWindowBridge,
} from "./desktopPet";

describe("PetWindowBridge", () => {
  it("only invokes the reviewed window commands", async () => {
    const invoke = vi.fn(async (command: string) => command === "toggle_pet_window" ? true : undefined);
    const bridge = createPetWindowBridge({
      isDesktop: () => true,
      invoke,
    });

    await bridge.open();
    await expect(bridge.toggle()).resolves.toBe(true);
    await bridge.startDragging();
    await bridge.setIgnoreCursorEvents(false);

    expect(invoke.mock.calls).toEqual([
      ["open_pet_window"],
      ["toggle_pet_window"],
      ["pet_window_start_dragging"],
      ["pet_window_set_ignore_cursor_events", { ignore: false }],
    ]);
  });

  it("does not expose pet commands outside Desktop", async () => {
    const invoke = vi.fn();
    const bridge = createPetWindowBridge({ isDesktop: () => false, invoke });

    await expect(bridge.open()).rejects.toBeInstanceOf(PetWindowBridgeError);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("rejects malformed toggle responses", async () => {
    const bridge = createPetWindowBridge({
      isDesktop: () => true,
      invoke: async () => "visible",
    });

    await expect(bridge.toggle()).rejects.toBeInstanceOf(PetWindowBridgeError);
  });
});
