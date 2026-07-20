import { describe, expect, it, vi } from "vitest";
import type { DesktopTransport } from "./desktopConnection";
import { RunApiError, type ProblemDetails } from "./runs";
import {
  createRunEventsApi,
  parseRunEventStream,
  type RunStreamEvent,
} from "./sse";

const NOW = "2026-07-16T08:00:00Z";
const encoder = new TextEncoder();

function payload(sequence: number, data: unknown, overrides: Record<string, unknown> = {}) {
  return {
    schemaVersion: 1,
    sequence,
    runId: "run-1",
    sessionId: "session-1",
    occurredAt: NOW,
    data,
    ...overrides,
  };
}

function frame(event: string, sequence: number, data: unknown, lineEnding = "\n"): string {
  return [
    `id: run-1:${sequence}`,
    `event: ${event}`,
    `data: ${JSON.stringify(payload(sequence, data))}`,
    "",
    "",
  ].join(lineEnding);
}

function streamResponse(
  chunks: Uint8Array[],
  status = 200,
  contentType = "text/event-stream; charset=utf-8",
): Response {
  return new Response(new ReadableStream<Uint8Array>({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(chunk);
      controller.close();
    },
  }), { status, headers: { "Content-Type": contentType } });
}

function bytesAtEveryBoundary(value: string): Uint8Array[] {
  return Array.from(encoder.encode(value), (byte) => Uint8Array.of(byte));
}

async function collect(iterable: AsyncIterable<RunStreamEvent>): Promise<RunStreamEvent[]> {
  const events: RunStreamEvent[] = [];
  for await (const event of iterable) events.push(event);
  return events;
}

describe("Run SSE parser", () => {
  it("decodes arbitrary chunks and UTF-8 code points without losing sequence", async () => {
    const body = frame("message.started", 1, {
      messageId: "assistant-1",
      role: "assistant",
    }) + frame("message.delta", 2, {
      messageId: "assistant-1",
      delta: "你好，Hermes",
    });

    await expect(collect(parseRunEventStream(
      streamResponse(bytesAtEveryBoundary(body)),
      "run-1",
      { sessionId: "session-1" },
    ))).resolves.toMatchObject([
      { id: "run-1:1", event: "message.started", payload: { sequence: 1 } },
      { id: "run-1:2", event: "message.delta", payload: { sequence: 2, data: { delta: "你好，Hermes" } } },
    ]);
  });

  it("accepts CRLF, comments, heartbeat-only frames, and multiline data", async () => {
    const jsonLines = JSON.stringify(payload(1, { profileId: "default" }), null, 2)
      .split("\n")
      .map((line) => `data: ${line}`)
      .join("\r\n");
    const body = [
      ": heartbeat\r\n\r\n",
      "id: run-1:1\r\n",
      "event: run.started\r\n",
      `${jsonLines}\r\n\r\n`,
      ": final-heartbeat\r\n\r\n",
    ].join("");

    await expect(collect(parseRunEventStream(
      streamResponse(bytesAtEveryBoundary(body)),
      "run-1",
    ))).resolves.toMatchObject([
      { event: "run.started", payload: { data: { profileId: "default" } } },
    ]);
  });

  it("starts after lastSequence and validates exact event ID/envelope/session binding", async () => {
    const valid = frame("message.delta", 4, { messageId: "m", delta: "next" });
    await expect(collect(parseRunEventStream(
      streamResponse([encoder.encode(valid)]),
      "run-1",
      { lastSequence: 3, sessionId: "session-1" },
    ))).resolves.toHaveLength(1);

    const invalidBodies = [
      frame("message.delta", 2, { messageId: "m", delta: "gap" }),
      frame("message.delta", 1, { messageId: "m", delta: "x" }).replace("id: run-1:1", "id: other:1"),
      frame("message.delta", 1, { messageId: "m", delta: "x" }).replace('"runId":"run-1"', '"runId":"other"'),
      frame("message.delta", 1, { messageId: "m", delta: "x" }).replace('"sessionId":"session-1"', '"sessionId":"other"'),
      frame("message.delta", 1, { messageId: "m", delta: "x" }).replace("id: run-1:1", "id: run-1:01"),
    ];
    for (const body of invalidBodies) {
      await expect(collect(parseRunEventStream(
        streamResponse([encoder.encode(body)]),
        "run-1",
        { sessionId: "session-1" },
      ))).rejects.toMatchObject({ kind: "invalid_response" });
    }
  });

  it.each([
    ["unknown event", "id: run-1:1\nevent: future.event\ndata: {}\n\n", "text/event-stream"],
    ["unknown field", "id: run-1:1\nevent: run.cancelled\nretry: 100\ndata: {}\n\n", "text/event-stream"],
    ["truncated frame", "id: run-1:1\nevent: run.cancelled\ndata: {}\n", "text/event-stream"],
    ["invalid JSON", "id: run-1:1\nevent: run.cancelled\ndata: {bad}\n\n", "text/event-stream"],
    ["wrong content type", frame("run.cancelled", 1, {}), "application/json"],
  ])("rejects %s", async (_label, body, contentType) => {
    await expect(collect(parseRunEventStream(
      streamResponse([encoder.encode(body)], 200, contentType),
      "run-1",
    ))).rejects.toMatchObject({ kind: "invalid_response" });
  });

  it("rejects any sequenced event after a terminal event", async () => {
    const body = frame("run.cancelled", 1, {})
      + frame("message.delta", 2, { messageId: "m", delta: "late" });
    await expect(collect(parseRunEventStream(
      streamResponse([encoder.encode(body)]),
      "run-1",
    ))).rejects.toMatchObject({ kind: "invalid_response" });
  });

  it("accepts one safe async tool delivery after a terminal event", async () => {
    const body = frame("run.cancelled", 1, {})
      + frame("tool.delivery", 2, {
        callId: "call-1",
        processId: "process_0123456789abcdef0123456789abcdef",
        delivery: "completion",
        status: "killed",
      });
    const events = await collect(parseRunEventStream(
      streamResponse([encoder.encode(body)]),
      "run-1",
    ));
    expect(events.map((event) => event.event)).toEqual(["run.cancelled", "tool.delivery"]);
  });

  it("rejects malformed UTF-8", async () => {
    await expect(collect(parseRunEventStream(
      streamResponse([Uint8Array.of(0xff, 0xfe)]),
      "run-1",
    ))).rejects.toMatchObject({ kind: "invalid_response" });
  });

  it("maps body read failures to a retryable network error", async () => {
    const response = new Response(new ReadableStream<Uint8Array>({
      pull(controller) {
        controller.error(new Error("socket closed"));
      },
    }), { headers: { "Content-Type": "text/event-stream" } });

    await expect(collect(parseRunEventStream(response, "run-1"))).rejects.toMatchObject({
      kind: "network",
      retryable: true,
    });
  });

  it("preserves a Problem Details response before reading a stream", async () => {
    const problem: ProblemDetails = {
      type: "urn:synthchat:error:event_history_expired",
      title: "Event history expired",
      status: 409,
      detail: null,
      instance: null,
      code: "event_history_expired",
      requestId: "request-1",
      retryable: false,
    };
    const response = new Response(JSON.stringify(problem), {
      status: 409,
      headers: { "Content-Type": "application/problem+json" },
    });

    await expect(collect(parseRunEventStream(response, "run-1"))).rejects.toMatchObject({
      kind: "http",
      status: 409,
      code: "event_history_expired",
      requestId: "request-1",
    });
  });
});

describe("Run SSE client", () => {
  it("uses fetch transport with Last-Event-ID, Accept, and the caller AbortSignal", async () => {
    const signal = new AbortController().signal;
    const response = streamResponse([
      encoder.encode(frame("run.cancelled", 8, { reason: "user" }).replaceAll("run-1", "run/1")),
    ]);
    const request = vi.fn(async (_path: string, _init?: RequestInit, _options?: unknown) => response);
    const api = createRunEventsApi({ request } as DesktopTransport);

    await expect(collect(api.streamRunEvents("run/1", {
      lastSequence: 7,
      sessionId: "session-1",
      signal,
    }))).resolves.toHaveLength(1);
    const [path, init, options] = request.mock.calls[0]!;
    expect(path).toBe("/api/v1/runs/run%2F1/events");
    expect((init as RequestInit).method).toBe("GET");
    const headers = new Headers((init as RequestInit).headers);
    expect(headers.get("Accept")).toBe("text/event-stream");
    expect(headers.get("Last-Event-ID")).toBe("run/1:7");
    expect(options).toEqual({ signal });
  });

  it("stops before transport when already aborted or lastSequence is invalid", async () => {
    const request = vi.fn();
    const api = createRunEventsApi({ request } as unknown as DesktopTransport);
    const controller = new AbortController();
    controller.abort();

    await expect(collect(api.streamRunEvents("run-1", { signal: controller.signal })))
      .rejects.toMatchObject({ name: "AbortError" });
    await expect(collect(api.streamRunEvents("run-1", { lastSequence: -1 })))
      .rejects.toMatchObject({ kind: "invalid_request" });
    expect(request).not.toHaveBeenCalled();
  });

  it("aborts a pending body read", async () => {
    let streamController: ReadableStreamDefaultController<Uint8Array> | undefined;
    const response = new Response(new ReadableStream<Uint8Array>({
      start(controller) {
        streamController = controller;
      },
      cancel() {
        streamController = undefined;
      },
    }), { headers: { "Content-Type": "text/event-stream" } });
    const request = vi.fn(async () => response);
    const api = createRunEventsApi({ request } as DesktopTransport);
    const controller = new AbortController();
    const pending = collect(api.streamRunEvents("run-1", { signal: controller.signal }));
    await Promise.resolve();
    controller.abort();

    await expect(pending).rejects.toMatchObject({ name: "AbortError" });
  });
});
