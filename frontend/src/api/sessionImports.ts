import type { components } from "./generated/openapi";
import {
  desktopTransport,
  type DesktopRequestOptions,
  type DesktopTransport,
} from "./desktopConnection";

export type HermesImportPreview = components["schemas"]["HermesImportPreview"];
export type HermesImportRequest = components["schemas"]["HermesImportRequest"];
export type HermesImportResult = components["schemas"]["HermesImportResult"];
export type HermesImportWarning = components["schemas"]["HermesImportWarning"];
export type HermesImportConflict = components["schemas"]["HermesImportConflict"];

export type SessionImportApiErrorKind = "http" | "invalid_request" | "invalid_response";

export class SessionImportApiError extends Error {
  readonly kind: SessionImportApiErrorKind;
  readonly status?: number;
  readonly code?: string;
  readonly requestId?: string;
  readonly retryable: boolean;
  readonly conflicts: HermesImportConflict[];
  readonly conflictCount: number;
  readonly conflictsDropped: number;

  constructor(
    kind: SessionImportApiErrorKind,
    message: string,
    options: {
      status?: number;
      code?: string;
      requestId?: string;
      retryable?: boolean;
      conflicts?: HermesImportConflict[];
      conflictCount?: number;
      conflictsDropped?: number;
    } = {},
  ) {
    super(message);
    this.name = "SessionImportApiError";
    this.kind = kind;
    this.status = options.status;
    this.code = options.code;
    this.requestId = options.requestId;
    this.retryable = options.retryable ?? false;
    this.conflicts = options.conflicts ?? [];
    this.conflictCount = options.conflictCount ?? 0;
    this.conflictsDropped = options.conflictsDropped ?? 0;
  }
}

export interface SessionImportsApi {
  previewHermesV21Import(
    profileId: string,
    options?: DesktopRequestOptions,
  ): Promise<HermesImportPreview>;
  importHermesV21(
    profileId: string,
    input: HermesImportRequest,
    idempotencyKey: string,
    options?: DesktopRequestOptions,
  ): Promise<HermesImportResult>;
}

const PROFILE_ID_PATTERN = /^(?:default|[a-z0-9_][a-z0-9_-]{0,63})$/u;
const FINGERPRINT_PATTERN = /^[0-9a-f]{64}$/u;
const IDEMPOTENCY_KEY_PATTERN = /^[\x21-\x7e]{8,128}$/u;
const ADAPTER_ID = "hermes-agent-state-v21";
const IMPORT_DISPOSITIONS = new Set(["imported", "unchanged", "replayed"]);
const CONFLICT_CODES = new Set([
  "sourceRemoved",
  "sourceChanged",
  "sourceExtended",
  "targetDeleted",
  "targetModified",
]);

function invalidResponse(context: string): never {
  throw new SessionImportApiError(
    "invalid_response",
    `${context} did not match the API v1 contract.`,
  );
}

function invalidRequest(message: string): never {
  throw new SessionImportApiError("invalid_request", message);
}

function asRecord(value: unknown, context: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return invalidResponse(context);
  }
  return value as Record<string, unknown>;
}

function exactKeys(
  record: Record<string, unknown>,
  required: readonly string[],
  optional: readonly string[],
  context: string,
): void {
  const allowed = new Set([...required, ...optional]);
  if (
    required.some((key) => !(key in record))
    || Object.keys(record).some((key) => !allowed.has(key))
  ) {
    invalidResponse(context);
  }
}

function stringValue(value: unknown, context: string): string {
  if (typeof value !== "string") return invalidResponse(context);
  return value;
}

function nonEmptyString(value: unknown, context: string): string {
  const result = stringValue(value, context);
  if (result.length === 0) return invalidResponse(context);
  return result;
}

function booleanValue(value: unknown, context: string): boolean {
  if (typeof value !== "boolean") return invalidResponse(context);
  return value;
}

function integerValue(value: unknown, minimum: number, context: string): number {
  if (!Number.isSafeInteger(value) || (value as number) < minimum) {
    return invalidResponse(context);
  }
  return value as number;
}

function nullableInteger(value: unknown, context: string): number | null {
  return value === null ? null : integerValue(value, 0, context);
}

function nullableString(value: unknown, context: string): string | null {
  return value === null ? null : stringValue(value, context);
}

function parseWarning(value: unknown): HermesImportWarning {
  const record = asRecord(value, "Hermes import warning");
  exactKeys(record, ["code", "count"], [], "Hermes import warning");
  return {
    code: nonEmptyString(record.code, "Hermes import warning.code"),
    count: integerValue(record.count, 1, "Hermes import warning.count"),
  };
}

function parseWarnings(value: unknown): HermesImportWarning[] {
  if (!Array.isArray(value) || value.length > 256) {
    return invalidResponse("Hermes import warnings");
  }
  const warnings = value.map(parseWarning);
  if (new Set(warnings.map((warning) => warning.code)).size !== warnings.length) {
    return invalidResponse("Hermes import warnings");
  }
  return warnings;
}

export function parseHermesImportPreview(value: unknown): HermesImportPreview {
  const record = asRecord(value, "Hermes import preview");
  const countKeys = [
    "schemaVersion",
    "snapshotFingerprint",
    "sessionCount",
    "messageCount",
    "modelUsageRowCount",
    "attachmentCount",
    "rewoundMessageCount",
  ] as const;
  exactKeys(
    record,
    ["state", "adapterId", "referenceCommit", ...countKeys, "warnings", "warningsDropped"],
    [],
    "Hermes import preview",
  );
  const state = stringValue(record.state, "Hermes import preview.state");
  if (state !== "absent" && state !== "ready") invalidResponse("Hermes import preview.state");
  if (record.adapterId !== ADAPTER_ID) invalidResponse("Hermes import preview.adapterId");
  const referenceCommit = nonEmptyString(record.referenceCommit, "Hermes import preview.referenceCommit");
  const snapshotFingerprint = nullableString(
    record.snapshotFingerprint,
    "Hermes import preview.snapshotFingerprint",
  );
  const preview: HermesImportPreview = {
    state,
    adapterId: ADAPTER_ID,
    referenceCommit,
    schemaVersion: nullableInteger(record.schemaVersion, "Hermes import preview.schemaVersion"),
    snapshotFingerprint,
    sessionCount: nullableInteger(record.sessionCount, "Hermes import preview.sessionCount"),
    messageCount: nullableInteger(record.messageCount, "Hermes import preview.messageCount"),
    modelUsageRowCount: nullableInteger(
      record.modelUsageRowCount,
      "Hermes import preview.modelUsageRowCount",
    ),
    attachmentCount: nullableInteger(record.attachmentCount, "Hermes import preview.attachmentCount"),
    rewoundMessageCount: nullableInteger(
      record.rewoundMessageCount,
      "Hermes import preview.rewoundMessageCount",
    ),
    warnings: parseWarnings(record.warnings),
    warningsDropped: integerValue(record.warningsDropped, 0, "Hermes import preview.warningsDropped"),
  };
  if (state === "absent") {
    if (countKeys.some((key) => record[key] !== null) || preview.warnings.length > 0) {
      return invalidResponse("Absent Hermes import preview");
    }
  } else if (
    preview.schemaVersion === null
    || preview.snapshotFingerprint === null
    || !FINGERPRINT_PATTERN.test(preview.snapshotFingerprint)
    || countKeys.slice(2).some((key) => record[key] === null)
  ) {
    return invalidResponse("Ready Hermes import preview");
  }
  return preview;
}

export function parseHermesImportResult(value: unknown): HermesImportResult {
  const record = asRecord(value, "Hermes import result");
  exactKeys(
    record,
    [
      "importId",
      "profileId",
      "disposition",
      "adapterId",
      "referenceCommit",
      "sourceSchemaVersion",
      "snapshotFingerprint",
      "importedSessionCount",
      "importedMessageCount",
      "importedModelUsageRowCount",
      "omittedAttachmentCount",
      "warnings",
      "warningsDropped",
    ],
    [],
    "Hermes import result",
  );
  const disposition = stringValue(record.disposition, "Hermes import result.disposition");
  const fingerprint = stringValue(record.snapshotFingerprint, "Hermes import result.snapshotFingerprint");
  if (
    !IMPORT_DISPOSITIONS.has(disposition)
    || record.adapterId !== ADAPTER_ID
    || record.sourceSchemaVersion !== 21
    || !FINGERPRINT_PATTERN.test(fingerprint)
  ) {
    return invalidResponse("Hermes import result");
  }
  return {
    importId: nonEmptyString(record.importId, "Hermes import result.importId"),
    profileId: nonEmptyString(record.profileId, "Hermes import result.profileId"),
    disposition: disposition as HermesImportResult["disposition"],
    adapterId: ADAPTER_ID,
    referenceCommit: nonEmptyString(record.referenceCommit, "Hermes import result.referenceCommit"),
    sourceSchemaVersion: 21,
    snapshotFingerprint: fingerprint,
    importedSessionCount: integerValue(record.importedSessionCount, 0, "Hermes import result.importedSessionCount"),
    importedMessageCount: integerValue(record.importedMessageCount, 0, "Hermes import result.importedMessageCount"),
    importedModelUsageRowCount: integerValue(
      record.importedModelUsageRowCount,
      0,
      "Hermes import result.importedModelUsageRowCount",
    ),
    omittedAttachmentCount: integerValue(
      record.omittedAttachmentCount,
      0,
      "Hermes import result.omittedAttachmentCount",
    ),
    warnings: parseWarnings(record.warnings),
    warningsDropped: integerValue(record.warningsDropped, 0, "Hermes import result.warningsDropped"),
  };
}

function parseConflict(value: unknown): HermesImportConflict {
  const record = asRecord(value, "Hermes import conflict");
  exactKeys(record, ["code", "sourceKeyDigest", "targetSessionId"], [], "Hermes import conflict");
  const code = stringValue(record.code, "Hermes import conflict.code");
  const digest = stringValue(record.sourceKeyDigest, "Hermes import conflict.sourceKeyDigest");
  if (!CONFLICT_CODES.has(code) || !FINGERPRINT_PATTERN.test(digest)) {
    return invalidResponse("Hermes import conflict");
  }
  return {
    code: code as HermesImportConflict["code"],
    sourceKeyDigest: digest,
    targetSessionId: nullableString(record.targetSessionId, "Hermes import conflict.targetSessionId"),
  };
}

async function jsonPayload(response: Response, context: string): Promise<unknown> {
  const contentType = response.headers.get("content-type")?.toLowerCase() ?? "";
  if (!contentType.includes("application/json") && !contentType.includes("application/problem+json")) {
    return invalidResponse(context);
  }
  try {
    return await response.json() as unknown;
  } catch {
    return invalidResponse(context);
  }
}

async function throwHttpError(response: Response): Promise<never> {
  const record = asRecord(await jsonPayload(response, "Hermes import error"), "Hermes import error");
  const core = ["type", "title", "status", "code", "requestId", "retryable"];
  const isConflict = record.code === "hermes_import_conflict";
  exactKeys(
    record,
    isConflict
      ? [...core, "detail", "instance", "conflictCount", "conflicts", "conflictsDropped"]
      : core,
    isConflict ? [] : ["detail", "instance"],
    "Hermes import error",
  );
  const status = integerValue(record.status, 400, "Hermes import error.status");
  if (status !== response.status || status > 599) invalidResponse("Hermes import error.status");
  let conflicts: HermesImportConflict[] = [];
  let conflictCount = 0;
  let conflictsDropped = 0;
  if (isConflict) {
    if (!Array.isArray(record.conflicts) || record.conflicts.length > 64) {
      return invalidResponse("Hermes import conflicts");
    }
    conflicts = record.conflicts.map(parseConflict);
    conflictCount = integerValue(record.conflictCount, 1, "Hermes import error.conflictCount");
    conflictsDropped = integerValue(record.conflictsDropped, 0, "Hermes import error.conflictsDropped");
    if (conflictCount !== conflicts.length + conflictsDropped) {
      return invalidResponse("Hermes import error conflict count");
    }
  }
  throw new SessionImportApiError("http", stringValue(record.title, "Hermes import error.title"), {
    status,
    code: stringValue(record.code, "Hermes import error.code"),
    requestId: stringValue(record.requestId, "Hermes import error.requestId"),
    retryable: booleanValue(record.retryable, "Hermes import error.retryable"),
    conflicts,
    conflictCount,
    conflictsDropped,
  });
}

function checkedProfileId(value: string): string {
  if (typeof value !== "string" || !PROFILE_ID_PATTERN.test(value)) {
    return invalidRequest("Profile ID is invalid.");
  }
  return encodeURIComponent(value);
}

function checkedIdempotencyKey(value: string): string {
  if (!IDEMPOTENCY_KEY_PATTERN.test(value)) {
    return invalidRequest("Idempotency-Key must contain 8 to 128 visible ASCII characters.");
  }
  return value;
}

function checkedImportRequest(input: HermesImportRequest): HermesImportRequest {
  if (input === null || typeof input !== "object" || Array.isArray(input)) {
    return invalidRequest("Hermes import request is invalid.");
  }
  const record = input as Record<string, unknown>;
  const keys = Object.keys(record);
  if (
    keys.length !== 2
    || !keys.includes("expectedSnapshotFingerprint")
    || !keys.includes("allowAttachmentOmission")
    || typeof input.expectedSnapshotFingerprint !== "string"
    || !FINGERPRINT_PATTERN.test(input.expectedSnapshotFingerprint)
    || typeof input.allowAttachmentOmission !== "boolean"
  ) {
    return invalidRequest("Hermes import request fields are invalid.");
  }
  return input;
}

class DefaultSessionImportsApi implements SessionImportsApi {
  constructor(private readonly transport: DesktopTransport) {}

  async previewHermesV21Import(
    profileId: string,
    options: DesktopRequestOptions = {},
  ): Promise<HermesImportPreview> {
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfileId(profileId)}/session-imports/hermes-v21`,
      { method: "GET", headers: { Accept: "application/json" } },
      options,
    );
    if (response.status !== 200) return throwHttpError(response);
    return parseHermesImportPreview(await jsonPayload(response, "Hermes import preview"));
  }

  async importHermesV21(
    profileId: string,
    input: HermesImportRequest,
    idempotencyKey: string,
    options: DesktopRequestOptions = {},
  ): Promise<HermesImportResult> {
    const checked = checkedImportRequest(input);
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfileId(profileId)}/session-imports/hermes-v21`,
      {
        method: "POST",
        headers: {
          Accept: "application/json",
          "Content-Type": "application/json",
          "Idempotency-Key": checkedIdempotencyKey(idempotencyKey),
        },
        body: JSON.stringify(checked),
      },
      options,
    );
    if (response.status !== 200) return throwHttpError(response);
    const result = parseHermesImportResult(await jsonPayload(response, "Hermes import result"));
    if (result.profileId !== profileId) return invalidResponse("Hermes import result.profileId");
    return result;
  }
}

export function createSessionImportsApi(
  transport: DesktopTransport = desktopTransport,
): SessionImportsApi {
  return new DefaultSessionImportsApi(transport);
}

export const sessionImportsApi = createSessionImportsApi();
