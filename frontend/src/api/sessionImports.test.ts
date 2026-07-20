import { describe, expect, it, vi } from "vitest";
import type { DesktopTransport } from "./desktopConnection";
import {
  createSessionImportsApi,
  parseHermesImportPreview,
  parseHermesImportResult,
  SessionImportApiError,
  type HermesImportPreview,
  type HermesImportResult,
} from "./sessionImports";

const FINGERPRINT = "a".repeat(64);
const REFERENCE_COMMIT = "3f2a389c7e1f1729cad91ae63c26fb08c7753c74";

const READY_PREVIEW: HermesImportPreview = {
  state: "ready",
  adapterId: "hermes-agent-state-v21",
  referenceCommit: REFERENCE_COMMIT,
  schemaVersion: 21,
  snapshotFingerprint: FINGERPRINT,
  sessionCount: 2,
  messageCount: 12,
  modelUsageRowCount: 3,
  attachmentCount: 1,
  rewoundMessageCount: 4,
  warnings: [{ code: "active_null_treated_as_active", count: 2 }],
  warningsDropped: 0,
};

const ABSENT_PREVIEW: HermesImportPreview = {
  state: "absent",
  adapterId: "hermes-agent-state-v21",
  referenceCommit: REFERENCE_COMMIT,
  schemaVersion: null,
  snapshotFingerprint: null,
  sessionCount: null,
  messageCount: null,
  modelUsageRowCount: null,
  attachmentCount: null,
  rewoundMessageCount: null,
  warnings: [],
  warningsDropped: 0,
};

const IMPORT_RESULT: HermesImportResult = {
  importId: "import_hv21_1",
  profileId: "work",
  disposition: "imported",
  adapterId: "hermes-agent-state-v21",
  referenceCommit: REFERENCE_COMMIT,
  sourceSchemaVersion: 21,
  snapshotFingerprint: FINGERPRINT,
  importedSessionCount: 2,
  importedMessageCount: 12,
  importedModelUsageRowCount: 3,
  omittedAttachmentCount: 1,
  warnings: [{ code: "attachment_omitted", count: 1 }],
  warningsDropped: 0,
};

function jsonResponse(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: {
      "Content-Type": status >= 400
        ? "application/problem+json"
        : "application/json; charset=utf-8",
    },
  });
}

function expectInvalid(parser: (value: unknown) => unknown, value: unknown): void {
  expect(() => parser(value)).toThrowError(
    expect.objectContaining<Partial<SessionImportApiError>>({ kind: "invalid_response" }),
  );
}

describe("Hermes session import runtime contract", () => {
  it("parses ready, absent, imported, unchanged, and replayed responses", () => {
    expect(parseHermesImportPreview(READY_PREVIEW)).toEqual(READY_PREVIEW);
    expect(parseHermesImportPreview(ABSENT_PREVIEW)).toEqual(ABSENT_PREVIEW);
    expect(parseHermesImportResult(IMPORT_RESULT)).toEqual(IMPORT_RESULT);
    expect(parseHermesImportResult({
      ...IMPORT_RESULT,
      disposition: "unchanged",
      importedSessionCount: 0,
      importedMessageCount: 0,
      importedModelUsageRowCount: 0,
    })).toMatchObject({ disposition: "unchanged", importedSessionCount: 0 });
    expect(parseHermesImportResult({ ...IMPORT_RESULT, disposition: "replayed" }))
      .toMatchObject({ disposition: "replayed" });
  });

  it.each([
    ["unknown preview field", { ...READY_PREVIEW, sourcePath: "C:/secret/state.db" }],
    ["wrong adapter", { ...READY_PREVIEW, adapterId: "future" }],
    ["bad fingerprint", { ...READY_PREVIEW, snapshotFingerprint: "bad" }],
    ["ready missing count", { ...READY_PREVIEW, messageCount: null }],
    ["absent leaking count", { ...ABSENT_PREVIEW, sessionCount: 1 }],
    ["duplicate warning", {
      ...READY_PREVIEW,
      warnings: [READY_PREVIEW.warnings[0], READY_PREVIEW.warnings[0]],
    }],
    ["too many warnings", {
      ...READY_PREVIEW,
      warnings: Array.from({ length: 257 }, (_, index) => ({ code: `w${index}`, count: 1 })),
    }],
  ])("rejects %s", (_label, value) => {
    expectInvalid(parseHermesImportPreview, value);
  });

  it.each([
    ["unknown result field", { ...IMPORT_RESULT, sourcePath: "C:/secret/state.db" }],
    ["wrong schema", { ...IMPORT_RESULT, sourceSchemaVersion: 22 }],
    ["bad disposition", { ...IMPORT_RESULT, disposition: "partial" }],
    ["negative count", { ...IMPORT_RESULT, importedMessageCount: -1 }],
  ])("rejects %s", (_label, value) => {
    expectInvalid(parseHermesImportResult, value);
  });
});

describe("Hermes session import client", () => {
  it("binds preview to the encoded Profile path and forwards AbortSignal", async () => {
    const signal = new AbortController().signal;
    const request = vi.fn(async () => jsonResponse(READY_PREVIEW));
    const api = createSessionImportsApi({ request } as DesktopTransport);

    await expect(api.previewHermesV21Import("work", { signal })).resolves.toEqual(READY_PREVIEW);
    expect(request).toHaveBeenCalledWith(
      "/api/v1/profiles/work/session-imports/hermes-v21",
      { method: "GET", headers: { Accept: "application/json" } },
      { signal },
    );
  });

  it("sends the expected fingerprint, omission policy, and stable idempotency key", async () => {
    const request = vi.fn(async (_path: string, _init: RequestInit) => jsonResponse(IMPORT_RESULT));
    const api = createSessionImportsApi({ request } as DesktopTransport);
    const input = {
      expectedSnapshotFingerprint: FINGERPRINT,
      allowAttachmentOmission: true,
    };

    await expect(api.importHermesV21("work", input, "import-attempt-0001"))
      .resolves.toEqual(IMPORT_RESULT);
    const [, init] = request.mock.calls[0]!;
    expect(init).toMatchObject({ method: "POST", body: JSON.stringify(input) });
    expect(new Headers(init.headers).get("Idempotency-Key")).toBe("import-attempt-0001");
    expect(new Headers(init.headers).get("Content-Type")).toBe("application/json");
  });

  it("rejects invalid local input before making a request", async () => {
    const request = vi.fn();
    const api = createSessionImportsApi({ request } as DesktopTransport);

    await expect(api.previewHermesV21Import("../escape")).rejects.toMatchObject({
      kind: "invalid_request",
    });
    await expect(api.importHermesV21("work", {
      expectedSnapshotFingerprint: "short",
      allowAttachmentOmission: false,
    }, "import-attempt-0001")).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(api.importHermesV21("work", {
      expectedSnapshotFingerprint: FINGERPRINT,
      allowAttachmentOmission: false,
    }, "short")).rejects.toMatchObject({ kind: "invalid_request" });
    expect(request).not.toHaveBeenCalled();
  });

  it("parses bounded atomic conflict details", async () => {
    const conflict = {
      type: "urn:synthchat:error:hermes_import_conflict",
      title: "Hermes import conflict",
      status: 409,
      detail: "No data was imported.",
      instance: "/api/v1/profiles/work/session-imports/hermes-v21",
      code: "hermes_import_conflict",
      requestId: "request-1",
      retryable: false,
      conflictCount: 2,
      conflicts: [{
        code: "targetModified",
        sourceKeyDigest: "b".repeat(64),
        targetSessionId: "session_hv21_1",
      }],
      conflictsDropped: 1,
    };
    const api = createSessionImportsApi({
      request: vi.fn(async () => jsonResponse(conflict, 409)),
    } as DesktopTransport);

    await expect(api.importHermesV21("work", {
      expectedSnapshotFingerprint: FINGERPRINT,
      allowAttachmentOmission: false,
    }, "import-attempt-0001")).rejects.toMatchObject({
      kind: "http",
      status: 409,
      code: "hermes_import_conflict",
      conflictCount: 2,
      conflictsDropped: 1,
      conflicts: [expect.objectContaining({ code: "targetModified" })],
    });
  });

  it("rejects malformed errors, response Profile mismatches, and non-JSON bodies", async () => {
    const malformedConflict = {
      type: "urn:synthchat:error:hermes_import_conflict",
      title: "Conflict",
      status: 409,
      detail: "Atomic rollback.",
      instance: "/import",
      code: "hermes_import_conflict",
      requestId: "request-1",
      retryable: false,
      conflictCount: 2,
      conflicts: [],
      conflictsDropped: 0,
    };
    const malformed = createSessionImportsApi({
      request: vi.fn(async () => jsonResponse(malformedConflict, 409)),
    } as DesktopTransport);
    await expect(malformed.previewHermesV21Import("work")).rejects.toMatchObject({
      kind: "invalid_response",
    });

    const mismatched = createSessionImportsApi({
      request: vi.fn(async () => jsonResponse({ ...IMPORT_RESULT, profileId: "default" })),
    } as DesktopTransport);
    await expect(mismatched.importHermesV21("work", {
      expectedSnapshotFingerprint: FINGERPRINT,
      allowAttachmentOmission: true,
    }, "import-attempt-0001")).rejects.toMatchObject({ kind: "invalid_response" });

    const nonJson = createSessionImportsApi({
      request: vi.fn(async () => new Response("no", {
        status: 503,
        headers: { "Content-Type": "text/plain" },
      })),
    } as DesktopTransport);
    await expect(nonJson.previewHermesV21Import("work")).rejects.toMatchObject({
      kind: "invalid_response",
    });
  });
});
