import { invoke, isTauri } from "@tauri-apps/api/core";

const PET_WINDOW_COMMANDS = {
  open: "open_pet_window",
  toggle: "toggle_pet_window",
  startDragging: "pet_window_start_dragging",
  setIgnoreCursorEvents: "pet_window_set_ignore_cursor_events",
} as const;

type PetWindowCommand = typeof PET_WINDOW_COMMANDS[keyof typeof PET_WINDOW_COMMANDS];

export class PetWindowBridgeError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "PetWindowBridgeError";
  }
}

export interface PetWindowBridge {
  open(): Promise<void>;
  toggle(): Promise<boolean>;
  startDragging(): Promise<void>;
  setIgnoreCursorEvents(ignore: boolean): Promise<void>;
}

export interface PetWindowBridgeDependencies {
  isDesktop?: () => boolean;
  invoke?: (command: PetWindowCommand, args?: Record<string, unknown>) => Promise<unknown>;
}

function ensureDesktop(isDesktop: () => boolean): void {
  if (!isDesktop()) {
    throw new PetWindowBridgeError("Desktop pet controls require the SynthChat Desktop application.");
  }
}

function asVisible(value: unknown): boolean {
  if (typeof value !== "boolean") {
    throw new PetWindowBridgeError("The desktop pet window returned an invalid visibility state.");
  }
  return value;
}

export function createPetWindowBridge(
  dependencies: PetWindowBridgeDependencies = {},
): PetWindowBridge {
  const desktop = dependencies.isDesktop ?? isTauri;
  const invokeCommand = dependencies.invoke ?? invoke;

  return {
    async open(): Promise<void> {
      ensureDesktop(desktop);
      await invokeCommand(PET_WINDOW_COMMANDS.open);
    },
    async toggle(): Promise<boolean> {
      ensureDesktop(desktop);
      return asVisible(await invokeCommand(PET_WINDOW_COMMANDS.toggle));
    },
    async startDragging(): Promise<void> {
      ensureDesktop(desktop);
      await invokeCommand(PET_WINDOW_COMMANDS.startDragging);
    },
    async setIgnoreCursorEvents(ignore: boolean): Promise<void> {
      ensureDesktop(desktop);
      await invokeCommand(PET_WINDOW_COMMANDS.setIgnoreCursorEvents, { ignore });
    },
  };
}

export const desktopPetWindow = createPetWindowBridge();
