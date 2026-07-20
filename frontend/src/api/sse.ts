import {
  desktopTransport,
  type DesktopRequestOptions,
  type DesktopTransport,
} from "./desktopConnection";
import {
  isRunEventName,
  parseRunEventPayload,
  RunApiError,
  throwRunHttpError,
  type RunEventName,
  type RunEventPayload,
} from "./runs";

export interface RunStreamEvent {
  id: string;
  event: RunEventName;
  payload: RunEventPayload;
}

export interface StreamRunEventsOptions extends DesktopRequestOptions {
  lastSequence?: number;
  sessionId?: string;
}

export interface RunEventsApi {
  streamRunEvents(
    runId: string,
    options?: StreamRunEventsOptions,
  ): AsyncIterable<RunStreamEvent>;
}

interface PendingFrame {
  id: string | null;
  event: string | null;
  data: string[];
  size: number;
  touched: boolean;
}

const MAX_STREAM_BUFFER_CHARS = 16 * 1024 * 1024;

function invalidResponse(context: string, cause?: unknown): never {
  throw new RunApiError(
    "invalid_response",
    `${context} does not match the Run SSE v1 contract.`,
    { cause },
  );
}

function networkError(cause: unknown): RunApiError {
  return new RunApiError("network", "The Run event stream disconnected.", {
    cause,
    retryable: true,
  });
}

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

function checkedIdentifier(value: string, context: string): string {
  if (typeof value !== "string" || value.length === 0) {
    throw new RunApiError("invalid_request", `${context} is invalid.`);
  }
  return value;
}

function checkedLastSequence(value: number | undefined): number {
  if (value === undefined) return 0;
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new RunApiError("invalid_request", "lastSequence must be a non-negative safe integer.");
  }
  return value;
}

function newPendingFrame(): PendingFrame {
  return { id: null, event: null, data: [], size: 0, touched: false };
}

function parseEventId(id: string, runId: string): number {
  const prefix = `${runId}:`;
  if (!id.startsWith(prefix)) return invalidResponse("Run event id binding");
  const encodedSequence = id.slice(prefix.length);
  if (!/^[1-9]\d*$/u.test(encodedSequence)) return invalidResponse("Run event id sequence");
  const sequence = Number(encodedSequence);
  if (!Number.isSafeInteger(sequence) || String(sequence) !== encodedSequence) {
    return invalidResponse("Run event id sequence");
  }
  return sequence;
}

function parseFrame(
  frame: PendingFrame,
  runId: string,
  sessionId: string | undefined,
  expectedSequence: number,
): RunStreamEvent | null {
  if (!frame.touched) return null;
  if (frame.id === null || frame.event === null || frame.data.length === 0) {
    return invalidResponse("Run event frame");
  }
  if (!isRunEventName(frame.event)) return invalidResponse("Run event name");
  const idSequence = parseEventId(frame.id, runId);
  if (idSequence !== expectedSequence) return invalidResponse("Run event sequence continuity");

  let decoded: unknown;
  try {
    decoded = JSON.parse(frame.data.join("\n")) as unknown;
  } catch (cause) {
    return invalidResponse("Run event data", cause);
  }
  const payload = parseRunEventPayload(frame.event, decoded);
  if (
    payload.runId !== runId
    || payload.sequence !== idSequence
    || (sessionId !== undefined && payload.sessionId !== sessionId)
  ) {
    return invalidResponse("Run event envelope binding");
  }
  return { id: frame.id, event: frame.event, payload };
}

function consumeLine(frame: PendingFrame, line: string): void {
  if (line.startsWith(":")) return;
  frame.size += line.length;
  if (frame.size > MAX_STREAM_BUFFER_CHARS) invalidResponse("Run event frame size");
  const separator = line.indexOf(":");
  const field = separator === -1 ? line : line.slice(0, separator);
  let value = separator === -1 ? "" : line.slice(separator + 1);
  if (value.startsWith(" ")) value = value.slice(1);
  switch (field) {
    case "id":
      if (frame.id !== null || value.includes("\0") || value.length === 0) {
        invalidResponse("Run event id field");
      }
      frame.id = value;
      frame.touched = true;
      break;
    case "event":
      if (frame.event !== null || value.length === 0) invalidResponse("Run event name field");
      frame.event = value;
      frame.touched = true;
      break;
    case "data":
      frame.data.push(value);
      frame.touched = true;
      break;
    case "":
      if (line.length !== 0) invalidResponse("Run event field");
      break;
    default:
      invalidResponse("Run event field");
  }
}

function nextLine(buffer: string, final: boolean): { line: string; rest: string } | null {
  const lf = buffer.indexOf("\n");
  const cr = buffer.indexOf("\r");
  let index = -1;
  if (lf !== -1 && cr !== -1) index = Math.min(lf, cr);
  else index = Math.max(lf, cr);
  if (index === -1) {
    if (!final || buffer.length === 0) return null;
    return { line: buffer, rest: "" };
  }
  if (buffer[index] === "\r" && index + 1 === buffer.length && !final) return null;
  const terminatorLength = buffer[index] === "\r" && buffer[index + 1] === "\n" ? 2 : 1;
  return {
    line: buffer.slice(0, index),
    rest: buffer.slice(index + terminatorLength),
  };
}

export async function* parseRunEventStream(
  response: Response,
  runId: string,
  options: StreamRunEventsOptions = {},
): AsyncGenerator<RunStreamEvent, void, void> {
  const checkedRunId = checkedIdentifier(runId, "Run ID");
  const expectedSessionId = options.sessionId === undefined
    ? undefined
    : checkedIdentifier(options.sessionId, "Session ID");
  let expectedSequence = checkedLastSequence(options.lastSequence) + 1;
  if (response.status !== 200) {
    await throwRunHttpError(response);
    return;
  }
  const contentType = response.headers.get("content-type")?.toLowerCase() ?? "";
  if (contentType.split(";", 1)[0]?.trim() !== "text/event-stream") {
    return invalidResponse("Run event Content-Type");
  }
  if (!response.body) return invalidResponse("Run event response body");

  const reader = response.body.getReader();
  const decoder = new TextDecoder("utf-8", { fatal: true });
  const abortError = new DOMException("The request was aborted.", "AbortError");
  let abortListener: (() => void) | undefined;
  const abortPromise = new Promise<never>((_resolve, reject) => {
    if (!options.signal) return;
    abortListener = () => reject(abortError);
    options.signal.addEventListener("abort", abortListener, { once: true });
  });
  let buffer = "";
  let frame = newPendingFrame();
  let completed = false;
  let terminalSeen = false;
  try {
    while (true) {
      if (options.signal?.aborted) throw new DOMException("The request was aborted.", "AbortError");
      let chunk: ReadableStreamReadResult<Uint8Array>;
      try {
        chunk = await Promise.race([reader.read(), abortPromise]);
      } catch (cause) {
        if (options.signal?.aborted) throw abortError;
        if (isAbortError(cause)) throw cause;
        throw networkError(cause);
      }
      if (chunk.done) {
        try {
          buffer += decoder.decode();
        } catch (cause) {
          return invalidResponse("Run event UTF-8", cause);
        }
        completed = true;
      } else {
        try {
          buffer += decoder.decode(chunk.value, { stream: true });
        } catch (cause) {
          return invalidResponse("Run event UTF-8", cause);
        }
      }
      if (buffer.length > MAX_STREAM_BUFFER_CHARS) return invalidResponse("Run event buffer size");

      while (true) {
        const parsed = nextLine(buffer, completed);
        if (!parsed) break;
        buffer = parsed.rest;
        if (parsed.line.length === 0) {
          const event = parseFrame(frame, checkedRunId, expectedSessionId, expectedSequence);
          frame = newPendingFrame();
          if (event) {
            if (terminalSeen && event.event !== "tool.delivery") {
              return invalidResponse("Run event after terminal event");
            }
            terminalSeen = event.event === "run.completed"
              || event.event === "run.cancelled"
              || event.event === "run.failed";
            expectedSequence += 1;
            yield event;
          }
        } else {
          consumeLine(frame, parsed.line);
        }
      }

      if (completed) {
        if (buffer.length > 0 || frame.touched) return invalidResponse("Run event truncated frame");
        return;
      }
    }
  } finally {
    if (abortListener) options.signal?.removeEventListener("abort", abortListener);
    if (!completed) {
      try {
        await reader.cancel();
      } catch {
        // The caller is already leaving the stream.
      }
    }
    reader.releaseLock();
  }
}

class DefaultRunEventsApi implements RunEventsApi {
  constructor(private readonly transport: DesktopTransport) {}

  async *streamRunEvents(
    runId: string,
    options: StreamRunEventsOptions = {},
  ): AsyncGenerator<RunStreamEvent, void, void> {
    const checkedRunId = checkedIdentifier(runId, "Run ID");
    const lastSequence = checkedLastSequence(options.lastSequence);
    if (options.signal?.aborted) throw new DOMException("The request was aborted.", "AbortError");
    const headers = new Headers({ Accept: "text/event-stream" });
    if (lastSequence > 0) headers.set("Last-Event-ID", `${checkedRunId}:${lastSequence}`);
    const response = await this.transport.request(
      `/api/v1/runs/${encodeURIComponent(checkedRunId)}/events`,
      { method: "GET", headers },
      { signal: options.signal },
    );
    yield* parseRunEventStream(response, checkedRunId, options);
  }
}

export function createRunEventsApi(transport: DesktopTransport = desktopTransport): RunEventsApi {
  return new DefaultRunEventsApi(transport);
}

export const runEventsApi = createRunEventsApi();
