export class FrontendRuntimeConfigError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "FrontendRuntimeConfigError";
  }
}

type RuntimeConfigGlobal = typeof globalThis & {
  __SYNTHCHAT_RUNTIME_CONFIG__?: unknown;
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

export function runtimeConfigSection(name: string): Record<string, unknown> | undefined {
  const root = (globalThis as RuntimeConfigGlobal).__SYNTHCHAT_RUNTIME_CONFIG__;
  if (root === undefined) return undefined;
  if (!isRecord(root)) {
    throw new FrontendRuntimeConfigError(
      "globalThis.__SYNTHCHAT_RUNTIME_CONFIG__ must be an object.",
    );
  }

  const section = root[name];
  if (section === undefined) return undefined;
  if (!isRecord(section)) {
    throw new FrontendRuntimeConfigError(
      `Runtime configuration section ${name} must be an object.`,
    );
  }
  return section;
}

interface IntegerSettingOptions {
  buildValue: unknown;
  defaultValue: number;
  maximum: number;
  minimum: number;
  name: string;
  runtimeValue: unknown;
}

export function readIntegerSetting({
  buildValue,
  defaultValue,
  maximum,
  minimum,
  name,
  runtimeValue,
}: IntegerSettingOptions): number {
  let value: number;
  if (runtimeValue !== undefined) {
    if (typeof runtimeValue !== "number") {
      throw new FrontendRuntimeConfigError(`${name} runtime value must be a number.`);
    }
    value = runtimeValue;
  } else if (buildValue !== undefined && buildValue !== "") {
    if (typeof buildValue !== "string" || !/^(?:0|[1-9][0-9]*)$/u.test(buildValue.trim())) {
      throw new FrontendRuntimeConfigError(`${name} VITE value must be a base-10 integer.`);
    }
    value = Number(buildValue.trim());
  } else {
    value = defaultValue;
  }

  if (!Number.isSafeInteger(value) || value < minimum || value > maximum) {
    throw new FrontendRuntimeConfigError(
      `${name} must be an integer between ${minimum} and ${maximum}.`,
    );
  }
  return value;
}

interface StringSettingOptions {
  buildValue: unknown;
  defaultValue: string;
  name: string;
  runtimeValue: unknown;
}

export function readStringSetting({
  buildValue,
  defaultValue,
  name,
  runtimeValue,
}: StringSettingOptions): string {
  if (runtimeValue !== undefined) {
    if (typeof runtimeValue !== "string" || !runtimeValue.trim()) {
      throw new FrontendRuntimeConfigError(`${name} runtime value must be a non-empty string.`);
    }
    return runtimeValue.trim();
  }
  if (buildValue !== undefined && buildValue !== "") {
    if (typeof buildValue !== "string" || !buildValue.trim()) {
      throw new FrontendRuntimeConfigError(`${name} VITE value must be a non-empty string.`);
    }
    return buildValue.trim();
  }
  return defaultValue;
}
