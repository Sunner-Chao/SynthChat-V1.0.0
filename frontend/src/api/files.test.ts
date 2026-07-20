// @vitest-environment jsdom

import { describe, expect, it, vi } from "vitest";
import type { DesktopTransport } from "./desktopConnection";
import { FileApiError, createFilesApi, parseFileRef } from "./files";

const FILE_REF = {
  id: "file_0123456789abcdef0123456789abcdef",
  name: "skill.zip",
  mimeType: "application/zip",
  sizeBytes: 42,
  createdAt: "2026-07-17T08:00:00Z",
};

function jsonResponse(
  body: unknown,
  status = 201,
  headers: Record<string, string> = {},
): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "Content-Type": status >= 400 ? "application/problem+json" : "application/json",
      ...headers,
    },
  });
}

describe("Files API", () => {
  it("strictly parses FileRef", () => {
    expect(parseFileRef(FILE_REF)).toEqual(FILE_REF);
    for (const invalid of [
      { ...FILE_REF, sizeBytes: -1 },
      { ...FILE_REF, sizeBytes: 1.5 },
      { ...FILE_REF, sizeBytes: 8 * 1024 * 1024 + 1 },
      { ...FILE_REF, mimeType: "application/x-untrusted" },
      { ...FILE_REF, createdAt: "yesterday" },
      { ...FILE_REF, extra: true },
      { ...FILE_REF, id: "" },
    ]) {
      expect(() => parseFileRef(invalid)).toThrow(FileApiError);
    }
  });

  it("uploads one multipart file without overriding its boundary", async () => {
    const request = vi.fn(async (
      _path: string,
      _init?: RequestInit,
      _options?: { signal?: AbortSignal },
    ) => jsonResponse(FILE_REF));
    const transport: DesktopTransport = { request };
    const file = new File(["skill archive"], "skill.zip", { type: "application/zip" });
    const controller = new AbortController();

    await expect(createFilesApi(transport).uploadFile(
      file,
      "upload-skill-0001",
      { signal: controller.signal },
    )).resolves.toEqual(FILE_REF);

    expect(request).toHaveBeenCalledTimes(1);
    const [path, init, options] = request.mock.calls[0]!;
    expect(path).toBe("/api/v1/files");
    expect(init?.method).toBe("POST");
    const headers = new Headers(init?.headers);
    expect(headers.get("Accept")).toBe("application/json");
    expect(headers.get("Idempotency-Key")).toBe("upload-skill-0001");
    expect(headers.has("Content-Type")).toBe(false);
    expect(init?.body).toBeInstanceOf(FormData);
    expect((init?.body as FormData).get("file")).toBeInstanceOf(File);
    expect(options).toEqual({ signal: controller.signal });
  });

  it("strictly parses Problems and preserves public metadata", async () => {
    const response = jsonResponse({
      type: "about:blank",
      title: "File rejected",
      status: 413,
      detail: "private diagnostic",
      code: "payload_too_large",
      requestId: "req-file",
      retryable: false,
    }, 413);
    const api = createFilesApi({ request: vi.fn(async () => response) });
    await expect(api.uploadFile(
      new File(["x"], "skill.zip"),
      "upload-skill-0001",
    )).rejects.toMatchObject({
      kind: "http",
      status: 413,
      code: "payload_too_large",
      requestId: "req-file",
    });

    const malformed = createFilesApi({
      request: vi.fn(async () => jsonResponse({
        type: "about:blank",
        title: "File rejected",
        status: 400,
        code: "bad_request",
        requestId: "req-file",
        retryable: false,
        unexpected: true,
      }, 400)),
    });
    await expect(malformed.uploadFile(
      new File(["x"], "skill.zip"),
      "upload-skill-0002",
    )).rejects.toMatchObject({ kind: "invalid_response" });
  });

  it("rejects invalid requests and forwards aborts", async () => {
    const controller = new AbortController();
    const request = vi.fn(async (
      _path: string,
      _init?: RequestInit,
      options?: { signal?: AbortSignal },
    ) => {
      expect(options?.signal).toBe(controller.signal);
      controller.abort();
      throw new DOMException("aborted", "AbortError");
    });
    const api = createFilesApi({ request });
    await expect(api.uploadFile(
      new File(["x"], "skill.zip"),
      "upload-skill-0001",
      { signal: controller.signal },
    )).rejects.toMatchObject({ name: "AbortError" });

    await expect(api.uploadFile(
      new File(["x"], "skill.zip"),
      "short",
    )).rejects.toMatchObject({ kind: "invalid_request" });
  });

  it("deletes only an opaque file ID and accepts only 204", async () => {
    const request = vi.fn(async () => new Response(null, { status: 204 }));
    const controller = new AbortController();
    const api = createFilesApi({ request });

    await expect(api.deleteFile(FILE_REF.id, { signal: controller.signal })).resolves.toBeUndefined();
    expect(request).toHaveBeenCalledWith(
      `/api/v1/files/${FILE_REF.id}`,
      { method: "DELETE", headers: { Accept: "application/json" } },
      { signal: controller.signal },
    );

    await expect(api.deleteFile("file_not-opaque")).rejects.toMatchObject({
      kind: "invalid_request",
    });
    expect(request).toHaveBeenCalledTimes(1);

    const unexpectedSuccess = createFilesApi({
      request: vi.fn(async () => jsonResponse(FILE_REF, 200)),
    });
    await expect(unexpectedSuccess.deleteFile(FILE_REF.id)).rejects.toMatchObject({
      kind: "invalid_response",
    });
  });

  it("strictly parses delete Problems and forwards aborts", async () => {
    const problem = jsonResponse({
      type: "about:blank",
      title: "File delete failed",
      status: 503,
      code: "file_store_unavailable",
      requestId: "req-file-delete",
      retryable: true,
    }, 503);
    const api = createFilesApi({ request: vi.fn(async () => problem) });
    await expect(api.deleteFile(FILE_REF.id)).rejects.toMatchObject({
      kind: "http",
      status: 503,
      code: "file_store_unavailable",
      requestId: "req-file-delete",
      retryable: true,
    });

    const controller = new AbortController();
    const aborting = createFilesApi({
      request: vi.fn(async (_path, _init, options) => {
        expect(options?.signal).toBe(controller.signal);
        controller.abort();
        throw new DOMException("aborted", "AbortError");
      }),
    });
    await expect(aborting.deleteFile(FILE_REF.id, { signal: controller.signal }))
      .rejects.toMatchObject({ name: "AbortError" });
  });
});
