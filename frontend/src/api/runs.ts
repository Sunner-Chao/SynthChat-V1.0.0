import type { components } from "./generated/openapi";
import { isFileMimeType, MAX_FILE_BYTES } from "./fileContract";
import {
  desktopTransport,
  type DesktopRequestOptions,
  type DesktopTransport,
} from "./desktopConnection";
import { parseMessage, parseProblemDetails } from "./sessions";

export type CreateRunInput = components["schemas"]["CreateRun"];
export type Run = components["schemas"]["Run"];
export type RunAccepted = components["schemas"]["RunAccepted"];
export type ActiveRun = components["schemas"]["ActiveRun"];
export type ActiveRunList = components["schemas"]["ActiveRunList"];
export type PendingApprovalAction = components["schemas"]["PendingApprovalAction"];
export type PendingClarificationAction = components["schemas"]["PendingClarificationAction"];
export type ApprovalDecision = components["schemas"]["ApprovalDecision"];
export type ClarificationAnswer = components["schemas"]["ClarificationAnswer"];
export type ActionAccepted = components["schemas"]["ActionAccepted"];
export type RunEventPayload = components["schemas"]["RunEventPayload"];
export type ProblemDetails = components["schemas"]["Problem"];
export type Usage = components["schemas"]["Usage"];
export type FileRef = components["schemas"]["FileRef"];

export const RUN_EVENT_NAMES = [
  "run.queued",
  "run.started",
  "message.started",
  "message.delta",
  "reasoning.delta",
  "tool.started",
  "tool.progress",
  "tool.completed",
  "tool.delivery",
  "tool.failed",
  "approval.required",
  "approval.resolved",
  "clarification.required",
  "clarification.resolved",
  "usage.updated",
  "message.completed",
  "run.completed",
  "run.cancelled",
  "run.failed",
] as const;

export type RunEventName = typeof RUN_EVENT_NAMES[number];
export type RunApiErrorKind = "http" | "invalid_request" | "invalid_response" | "network";

export class RunApiError extends Error {
  readonly kind: RunApiErrorKind;
  readonly status?: number;
  readonly code?: string;
  readonly requestId?: string;
  readonly retryable: boolean;

  constructor(
    kind: RunApiErrorKind,
    message: string,
    options: {
      cause?: unknown;
      status?: number;
      code?: string;
      requestId?: string;
      retryable?: boolean;
    } = {},
  ) {
    super(message, options.cause === undefined ? undefined : { cause: options.cause });
    this.name = "RunApiError";
    this.kind = kind;
    this.status = options.status;
    this.code = options.code;
    this.requestId = options.requestId;
    this.retryable = options.retryable ?? false;
  }
}

export interface RunsApi {
  listActiveRuns(
    profileId: string,
    query?: ActiveRunsQuery,
    options?: DesktopRequestOptions,
  ): Promise<ActiveRunList>;
  createRun(
    sessionId: string,
    input: CreateRunInput,
    idempotencyKey: string,
    options?: DesktopRequestOptions,
  ): Promise<RunAccepted>;
  getRun(runId: string, options?: DesktopRequestOptions): Promise<Run>;
  cancelRun(runId: string, options?: DesktopRequestOptions): Promise<Run>;
  resolveApproval(
    runId: string,
    approvalId: string,
    decision: ApprovalDecision,
    options?: DesktopRequestOptions,
  ): Promise<ActionAccepted>;
  answerClarification(
    runId: string,
    requestId: string,
    answer: ClarificationAnswer,
    options?: DesktopRequestOptions,
  ): Promise<ActionAccepted>;
}

export interface ActiveRunsQuery {
  sessionId?: string;
}

const RUN_STATUSES = new Set<Run["status"]>([
  "queued",
  "running",
  "waitingApproval",
  "waitingClarification",
  "cancelling",
  "completed",
  "cancelled",
  "failed",
]);
const RUN_EVENT_NAME_SET = new Set<string>(RUN_EVENT_NAMES);
const ACTIVE_RUN_STATUSES = new Set<Run["status"]>([
  "queued",
  "running",
  "waitingApproval",
  "waitingClarification",
  "cancelling",
]);
const APPROVAL_CHOICES = new Set(["once", "session", "always", "deny"] as const);
const REASONING_EFFORTS = new Set(["minimal", "low", "medium", "high", "xhigh"] as const);
const IDEMPOTENCY_KEY_PATTERN = /^[\x21-\x7e]{8,128}$/u;
const REVISION_PATTERN = /^[\x21\x23-\x7e]{1,126}$/u;
const PERSONA_ID_PATTERN = /^persona_[0-9a-f]{32}$/u;
const RFC3339_PATTERN = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/u;
const PROFILE_ID_PATTERN = /^(?:default|[a-z0-9_][a-z0-9_-]{0,63})$/u;
const SESSION_ID_PATTERN = /^session_[A-Za-z0-9_-]{1,120}$/u;
const QUEUE_ITEM_ID_PATTERN = /^queue_[0-9a-f]{32}$/u;
const MAX_ACTIVE_RUNS = 16;

function invalidRequest(message: string): never {
  throw new RunApiError("invalid_request", message);
}

function invalidResponse(context: string, cause?: unknown): never {
  throw new RunApiError(
    "invalid_response",
    `${context} does not match the Run API v1 contract.`,
    { cause },
  );
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
    required.some((key) => !Object.prototype.hasOwnProperty.call(record, key))
    || Object.keys(record).some((key) => !allowed.has(key))
  ) {
    invalidResponse(context);
  }
}

function requestRecord(
  value: unknown,
  allowed: readonly string[],
  context: string,
): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return invalidRequest(`${context} must be an object.`);
  }
  const record = value as Record<string, unknown>;
  if (Object.keys(record).some((key) => !allowed.includes(key))) {
    return invalidRequest(`${context} contains unsupported fields.`);
  }
  return record;
}

function stringValue(value: unknown, context: string): string {
  if (typeof value !== "string") return invalidResponse(context);
  return value;
}

function booleanValue(value: unknown, context: string): boolean {
  if (typeof value !== "boolean") return invalidResponse(context);
  return value;
}

function nonEmptyString(value: unknown, context: string): string {
  const result = stringValue(value, context);
  if (result.length === 0) return invalidResponse(context);
  return result;
}

function integerValue(
  value: unknown,
  minimum: number,
  context: string,
  maximum = Number.MAX_SAFE_INTEGER,
): number {
  if (
    !Number.isSafeInteger(value)
    || (value as number) < minimum
    || (value as number) > maximum
  ) {
    return invalidResponse(context);
  }
  return value as number;
}

function finiteNumber(value: unknown, minimum: number, maximum: number, context: string): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value < minimum || value > maximum) {
    return invalidResponse(context);
  }
  return value;
}

function dateTime(value: unknown, context: string): string {
  const result = stringValue(value, context);
  if (!RFC3339_PATTERN.test(result) || Number.isNaN(Date.parse(result))) {
    return invalidResponse(context);
  }
  return result;
}

function nullableString(value: unknown, context: string): string | null {
  return value === null ? null : stringValue(value, context);
}

function parseUsage(value: unknown, context: string): Usage {
  const record = asRecord(value, context);
  exactKeys(record, ["promptTokens", "completionTokens", "totalTokens"], ["cost"], context);
  const result: Usage = {
    promptTokens: integerValue(record.promptTokens, 0, `${context}.promptTokens`),
    completionTokens: integerValue(record.completionTokens, 0, `${context}.completionTokens`),
    totalTokens: integerValue(record.totalTokens, 0, `${context}.totalTokens`),
  };
  if ("cost" in record) {
    result.cost = record.cost === null
      ? null
      : finiteNumber(record.cost, 0, Number.MAX_VALUE, `${context}.cost`);
  }
  return result;
}

function parseFileRef(value: unknown, context: string): FileRef {
  const record = asRecord(value, context);
  exactKeys(record, ["id", "name", "mimeType", "sizeBytes", "createdAt"], [], context);
  if (!isFileMimeType(record.mimeType)) {
    return invalidResponse(`${context}.mimeType`);
  }
  return {
    id: nonEmptyString(record.id, `${context}.id`),
    name: stringValue(record.name, `${context}.name`),
    mimeType: record.mimeType,
    sizeBytes: integerValue(record.sizeBytes, 0, `${context}.sizeBytes`, MAX_FILE_BYTES),
    createdAt: dateTime(record.createdAt, `${context}.createdAt`),
  };
}

export function parseRunProblemDetails(value: unknown): ProblemDetails {
  try {
    return parseProblemDetails(value);
  } catch (cause) {
    return invalidResponse("Run Problem Details", cause);
  }
}

function parseApprovalAction(value: unknown): PendingApprovalAction {
  const record = asRecord(value, "Pending approval action");
  exactKeys(
    record,
    ["kind", "approvalId", "callId", "toolName", "inputSummary", "choices", "expiresAt"],
    [],
    "Pending approval action",
  );
  if (record.kind !== "approval" || !Array.isArray(record.choices) || record.choices.length === 0) {
    return invalidResponse("Pending approval action");
  }
  const choices = record.choices.map((choice) => {
    if (typeof choice !== "string" || !APPROVAL_CHOICES.has(choice as ApprovalDecision["decision"])) {
      return invalidResponse("Pending approval action.choices");
    }
    return choice as ApprovalDecision["decision"];
  });
  if (new Set(choices).size !== choices.length) {
    return invalidResponse("Pending approval action.choices");
  }
  return {
    kind: "approval",
    approvalId: nonEmptyString(record.approvalId, "Pending approval action.approvalId"),
    callId: nonEmptyString(record.callId, "Pending approval action.callId"),
    toolName: nonEmptyString(record.toolName, "Pending approval action.toolName"),
    inputSummary: nullableString(record.inputSummary, "Pending approval action.inputSummary"),
    choices,
    expiresAt: dateTime(record.expiresAt, "Pending approval action.expiresAt"),
  };
}

function parseClarificationAction(value: unknown): PendingClarificationAction {
  const record = asRecord(value, "Pending clarification action");
  exactKeys(record, ["kind", "requestId", "question", "choices"], [], "Pending clarification action");
  if (record.kind !== "clarification" || !Array.isArray(record.choices)) {
    return invalidResponse("Pending clarification action");
  }
  return {
    kind: "clarification",
    requestId: nonEmptyString(record.requestId, "Pending clarification action.requestId"),
    question: stringValue(record.question, "Pending clarification action.question"),
    choices: record.choices.map((choice) => stringValue(choice, "Pending clarification action.choices")),
  };
}

export function parseRun(value: unknown): Run {
  const record = asRecord(value, "Run");
  exactKeys(
    record,
    [
      "id",
      "sessionId",
      "profileId",
      "status",
      "lastSequence",
      "messageId",
      "usage",
      "error",
      "pendingAction",
      "createdAt",
      "updatedAt",
    ],
    [],
    "Run",
  );
  if (typeof record.status !== "string" || !RUN_STATUSES.has(record.status as Run["status"])) {
    return invalidResponse("Run.status");
  }
  const status = record.status as Run["status"];
  let pendingAction: Run["pendingAction"];
  if (status === "waitingApproval") {
    pendingAction = parseApprovalAction(record.pendingAction);
  } else if (status === "waitingClarification") {
    pendingAction = parseClarificationAction(record.pendingAction);
  } else {
    if (record.pendingAction !== null) return invalidResponse("Run.pendingAction");
    pendingAction = null;
  }
  const messageId = record.messageId === null
    ? null
    : nonEmptyString(record.messageId, "Run.messageId");
  const usage = record.usage === null ? null : parseUsage(record.usage, "Run.usage");
  const error = record.error === null ? null : parseRunProblemDetails(record.error);
  return {
    id: nonEmptyString(record.id, "Run.id"),
    sessionId: nonEmptyString(record.sessionId, "Run.sessionId"),
    profileId: nonEmptyString(record.profileId, "Run.profileId"),
    status,
    lastSequence: integerValue(record.lastSequence, 0, "Run.lastSequence"),
    messageId,
    usage,
    error,
    pendingAction,
    createdAt: dateTime(record.createdAt, "Run.createdAt"),
    updatedAt: dateTime(record.updatedAt, "Run.updatedAt"),
  };
}

export function parseRunAccepted(value: unknown): RunAccepted {
  const record = asRecord(value, "Run accepted response");
  exactKeys(
    record,
    ["run", "disposition", "queueItemId", "userMessage", "sessionRevision"],
    [],
    "Run accepted response",
  );
  const run = parseRun(record.run);
  let userMessage: RunAccepted["userMessage"];
  try {
    userMessage = parseMessage(record.userMessage);
  } catch (cause) {
    return invalidResponse("Run accepted response.userMessage", cause);
  }
  if (userMessage.role !== "user" || userMessage.sessionId !== run.sessionId) {
    return invalidResponse("Run accepted response.userMessage");
  }
  if (!REVISION_PATTERN.test(stringValue(record.sessionRevision, "Run accepted response.sessionRevision"))) {
    return invalidResponse("Run accepted response.sessionRevision");
  }
  const queueItemId = record.queueItemId === null
    ? null
    : nonEmptyString(record.queueItemId, "Run accepted response.queueItemId");
  if (record.disposition === "started") {
    if (run.status !== "running" || queueItemId !== null) return invalidResponse("Run accepted response");
  } else if (record.disposition === "queued") {
    if (run.status !== "queued" || queueItemId === null) return invalidResponse("Run accepted response");
  } else if (record.disposition !== "replayed") {
    return invalidResponse("Run accepted response.disposition");
  }
  return {
    run,
    disposition: record.disposition,
    queueItemId,
    userMessage,
    sessionRevision: record.sessionRevision,
  } as RunAccepted;
}

export function parseActiveRunList(value: unknown): ActiveRunList {
  const record = asRecord(value, "Active Run list");
  exactKeys(record, ["items"], [], "Active Run list");
  if (!Array.isArray(record.items) || record.items.length > MAX_ACTIVE_RUNS) {
    return invalidResponse("Active Run list.items");
  }
  const seen = new Set<string>();
  let previous: Run | null = null;
  const items = record.items.map((value, index): ActiveRun => {
    const context = `Active Run list.items[${index}]`;
    const item = asRecord(value, context);
    exactKeys(item, ["run", "queueItemId", "userMessage", "sessionRevision"], [], context);
    const run = parseRun(item.run);
    if (!ACTIVE_RUN_STATUSES.has(run.status) || !seen.add(run.id)) {
      return invalidResponse(`${context}.run`);
    }
    const queueItemId = item.queueItemId === null
      ? null
      : nonEmptyString(item.queueItemId, `${context}.queueItemId`);
    if (
      (run.status === "queued") !== (queueItemId !== null)
      || (queueItemId !== null && !QUEUE_ITEM_ID_PATTERN.test(queueItemId))
    ) {
      return invalidResponse(`${context}.queueItemId`);
    }
    let userMessage: ActiveRun["userMessage"];
    try {
      userMessage = parseMessage(item.userMessage);
    } catch (cause) {
      return invalidResponse(`${context}.userMessage`, cause);
    }
    if (userMessage.role !== "user" || userMessage.sessionId !== run.sessionId) {
      return invalidResponse(`${context}.userMessage`);
    }
    const sessionRevision = stringValue(item.sessionRevision, `${context}.sessionRevision`);
    if (!REVISION_PATTERN.test(sessionRevision)) {
      return invalidResponse(`${context}.sessionRevision`);
    }
    if (previous) {
      const previousTime = Date.parse(previous.createdAt);
      const currentTime = Date.parse(run.createdAt);
      if (previousTime > currentTime || (previousTime === currentTime && previous.id >= run.id)) {
        return invalidResponse("Active Run list ordering");
      }
    }
    previous = run;
    return { run, queueItemId, userMessage, sessionRevision };
  });
  return { items };
}

function parseActionAccepted(value: unknown): ActionAccepted {
  const record = asRecord(value, "Run action response");
  exactKeys(record, ["accepted"], [], "Run action response");
  if (record.accepted !== true) return invalidResponse("Run action response.accepted");
  return { accepted: true };
}

function parseEnvelope(value: unknown): {
  record: Record<string, unknown>;
  envelope: Omit<components["schemas"]["RunEventEnvelope"], "data">;
} {
  const record = asRecord(value, "Run event");
  exactKeys(record, ["schemaVersion", "sequence", "runId", "sessionId", "occurredAt", "data"], [], "Run event");
  if (record.schemaVersion !== 1) return invalidResponse("Run event.schemaVersion");
  return {
    record,
    envelope: {
      schemaVersion: 1,
      sequence: integerValue(record.sequence, 1, "Run event.sequence"),
      runId: nonEmptyString(record.runId, "Run event.runId"),
      sessionId: nonEmptyString(record.sessionId, "Run event.sessionId"),
      occurredAt: dateTime(record.occurredAt, "Run event.occurredAt"),
    },
  };
}

function eventData(value: unknown, required: readonly string[], optional: readonly string[], context: string) {
  const record = asRecord(value, context);
  exactKeys(record, required, optional, context);
  return record;
}

export function isRunEventName(value: string): value is RunEventName {
  return RUN_EVENT_NAME_SET.has(value);
}

export function parseRunEventPayload(event: RunEventName, value: unknown): RunEventPayload {
  const { record, envelope } = parseEnvelope(value);
  const context = `${event} data`;
  let data: unknown;
  switch (event) {
    case "run.queued": {
      const item = eventData(record.data, ["queueItemId"], [], context);
      data = { queueItemId: nonEmptyString(item.queueItemId, `${context}.queueItemId`) };
      break;
    }
    case "run.started": {
      const item = eventData(record.data, ["profileId"], [], context);
      data = { profileId: nonEmptyString(item.profileId, `${context}.profileId`) };
      break;
    }
    case "message.started": {
      const item = eventData(record.data, ["messageId", "role"], [], context);
      if (item.role !== "assistant") return invalidResponse(`${context}.role`);
      data = { messageId: nonEmptyString(item.messageId, `${context}.messageId`), role: "assistant" };
      break;
    }
    case "message.delta":
    case "reasoning.delta": {
      const item = eventData(record.data, ["messageId", "delta"], [], context);
      const delta = nonEmptyString(item.delta, `${context}.delta`);
      data = { messageId: nonEmptyString(item.messageId, `${context}.messageId`), delta };
      break;
    }
    case "tool.started": {
      const item = eventData(record.data, ["callId", "name"], ["inputSummary"], context);
      data = {
        callId: nonEmptyString(item.callId, `${context}.callId`),
        name: nonEmptyString(item.name, `${context}.name`),
        ...("inputSummary" in item
          ? { inputSummary: stringValue(item.inputSummary, `${context}.inputSummary`) }
          : {}),
      };
      break;
    }
    case "tool.progress": {
      const item = eventData(record.data, ["callId"], ["message", "progress"], context);
      data = {
        callId: nonEmptyString(item.callId, `${context}.callId`),
        ...("message" in item ? { message: stringValue(item.message, `${context}.message`) } : {}),
        ...("progress" in item
          ? { progress: finiteNumber(item.progress, 0, 1, `${context}.progress`) }
          : {}),
      };
      break;
    }
    case "tool.completed": {
      const item = eventData(
        record.data,
        ["callId", "artifacts"],
        ["resultSummary", "asyncDeliveryPending"],
        context,
      );
      if (!Array.isArray(item.artifacts)) return invalidResponse(`${context}.artifacts`);
      data = {
        callId: nonEmptyString(item.callId, `${context}.callId`),
        artifacts: item.artifacts.map((artifact, index) => parseFileRef(artifact, `${context}.artifacts[${index}]`)),
        ...("resultSummary" in item
          ? { resultSummary: stringValue(item.resultSummary, `${context}.resultSummary`) }
          : {}),
        ...("asyncDeliveryPending" in item
          ? {
            asyncDeliveryPending: booleanValue(
              item.asyncDeliveryPending,
              `${context}.asyncDeliveryPending`,
            ),
          }
          : {}),
      };
      break;
    }
    case "tool.delivery": {
      const item = eventData(
        record.data,
        ["callId", "processId", "delivery", "status"],
        ["exitCode", "matchedPatternCount"],
        context,
      );
      const delivery = stringValue(item.delivery, `${context}.delivery`);
      const status = stringValue(item.status, `${context}.status`);
      if (
        !/^process_[0-9a-f]{32}$/u.test(nonEmptyString(item.processId, `${context}.processId`))
        || !["completion", "watch"].includes(delivery)
        || !["starting", "running", "exited", "killed", "lost", "failed_start"].includes(status)
      ) {
        return invalidResponse(context);
      }
      const exitCode = "exitCode" in item
        ? item.exitCode === null
          ? null
          : integerValue(item.exitCode, -2_147_483_648, `${context}.exitCode`, 2_147_483_647)
        : undefined;
      const matchedPatternCount = "matchedPatternCount" in item
        ? integerValue(item.matchedPatternCount, 1, `${context}.matchedPatternCount`, 16)
        : undefined;
      if ((delivery === "watch") !== (matchedPatternCount !== undefined)) {
        return invalidResponse(context);
      }
      data = {
        callId: nonEmptyString(item.callId, `${context}.callId`),
        processId: item.processId,
        delivery: delivery as "completion" | "watch",
        status: status as "starting" | "running" | "exited" | "killed" | "lost" | "failed_start",
        ...(exitCode === undefined ? {} : { exitCode }),
        ...(matchedPatternCount === undefined ? {} : { matchedPatternCount }),
      };
      break;
    }
    case "tool.failed": {
      const item = eventData(record.data, ["callId", "error"], [], context);
      data = {
        callId: nonEmptyString(item.callId, `${context}.callId`),
        error: parseRunProblemDetails(item.error),
      };
      break;
    }
    case "approval.required": {
      data = parseApprovalAction({ kind: "approval", ...eventData(
        record.data,
        ["approvalId", "callId", "toolName", "inputSummary", "choices", "expiresAt"],
        [],
        context,
      ) });
      const { kind: _kind, ...approvalData } = data as PendingApprovalAction;
      data = approvalData;
      break;
    }
    case "approval.resolved": {
      const item = eventData(
        record.data,
        ["approvalId", "callId", "decision", "resolvedBy"],
        [],
        context,
      );
      const decision = stringValue(item.decision, `${context}.decision`);
      const resolvedBy = stringValue(item.resolvedBy, `${context}.resolvedBy`);
      if (
        !APPROVAL_CHOICES.has(decision as ApprovalDecision["decision"])
        || !["user", "expiry", "cancellation"].includes(resolvedBy)
        || (resolvedBy !== "user" && decision !== "deny")
      ) {
        return invalidResponse(context);
      }
      data = {
        approvalId: nonEmptyString(item.approvalId, `${context}.approvalId`),
        callId: nonEmptyString(item.callId, `${context}.callId`),
        decision: decision as ApprovalDecision["decision"],
        resolvedBy: resolvedBy as "user" | "expiry" | "cancellation",
      };
      break;
    }
    case "clarification.required": {
      data = parseClarificationAction({ kind: "clarification", ...eventData(
        record.data,
        ["requestId", "question", "choices"],
        [],
        context,
      ) });
      const { kind: _kind, ...clarificationData } = data as PendingClarificationAction;
      data = clarificationData;
      break;
    }
    case "clarification.resolved": {
      const item = eventData(record.data, ["requestId", "resolvedBy"], [], context);
      const resolvedBy = stringValue(item.resolvedBy, `${context}.resolvedBy`);
      if (!["user", "cancellation", "failure"].includes(resolvedBy)) {
        return invalidResponse(context);
      }
      data = {
        requestId: nonEmptyString(item.requestId, `${context}.requestId`),
        resolvedBy: resolvedBy as "user" | "cancellation" | "failure",
      };
      break;
    }
    case "usage.updated":
      data = parseUsage(record.data, context);
      break;
    case "message.completed": {
      const item = eventData(record.data, ["message", "sessionRevision"], [], context);
      let message: components["schemas"]["Message"];
      try {
        message = parseMessage(item.message);
      } catch (cause) {
        return invalidResponse(`${context}.message`, cause);
      }
      const sessionRevision = stringValue(item.sessionRevision, `${context}.sessionRevision`);
      if (
        message.role !== "assistant"
        || message.sessionId !== envelope.sessionId
        || !REVISION_PATTERN.test(sessionRevision)
      ) {
        return invalidResponse(context);
      }
      data = { message, sessionRevision };
      break;
    }
    case "run.completed": {
      const item = eventData(record.data, ["usage", "messageId"], [], context);
      data = {
        usage: parseUsage(item.usage, `${context}.usage`),
        messageId: nonEmptyString(item.messageId, `${context}.messageId`),
      };
      break;
    }
    case "run.cancelled": {
      const item = eventData(record.data, [], ["reason"], context);
      data = "reason" in item ? { reason: stringValue(item.reason, `${context}.reason`) } : {};
      break;
    }
    case "run.failed": {
      const item = eventData(record.data, ["error"], [], context);
      data = { error: parseRunProblemDetails(item.error) };
      break;
    }
  }
  return { ...envelope, data } as unknown as RunEventPayload;
}

async function jsonPayload(response: Response, context: string): Promise<unknown> {
  const contentType = response.headers.get("content-type")?.toLowerCase() ?? "";
  if (!contentType.includes("application/json") && !contentType.includes("application/problem+json")) {
    return invalidResponse(context);
  }
  try {
    return await response.json() as unknown;
  } catch (cause) {
    return invalidResponse(context, cause);
  }
}

export async function throwRunHttpError(response: Response): Promise<never> {
  const problem = parseRunProblemDetails(await jsonPayload(response, "Run error response"));
  if (problem.status !== response.status) return invalidResponse("Run error response.status");
  throw new RunApiError("http", problem.title, {
    status: problem.status,
    code: problem.code,
    requestId: problem.requestId,
    retryable: problem.retryable,
  });
}

async function parsedResponse<T>(
  response: Response,
  expectedStatus: number,
  context: string,
  parser: (value: unknown) => T,
): Promise<T> {
  if (response.status !== expectedStatus) return throwRunHttpError(response);
  return parser(await jsonPayload(response, context));
}

function checkedIdentifier(value: string, context: string): string {
  if (typeof value !== "string" || value.length === 0) {
    return invalidRequest(`${context} is invalid.`);
  }
  return encodeURIComponent(value);
}

function checkedProfileId(value: string): string {
  if (typeof value !== "string" || !PROFILE_ID_PATTERN.test(value)) {
    return invalidRequest("Profile ID is invalid.");
  }
  return value;
}

function checkedActiveRunsQuery(value: ActiveRunsQuery): string | undefined {
  const record = requestRecord(value, ["sessionId"], "Active Run query");
  if (!("sessionId" in record)) return undefined;
  if (typeof record.sessionId !== "string" || !SESSION_ID_PATTERN.test(record.sessionId)) {
    return invalidRequest("Session ID is invalid.");
  }
  return record.sessionId;
}

function checkedIdempotencyKey(value: string): string {
  if (typeof value !== "string" || !IDEMPOTENCY_KEY_PATTERN.test(value)) {
    return invalidRequest("Idempotency-Key must contain 8 to 128 visible ASCII characters.");
  }
  return value;
}

function checkedModelConfig(value: unknown): components["schemas"]["ModelConfig"] {
  const record = requestRecord(value, ["provider", "model", "baseUrl", "reasoningEffort"], "Model override");
  if (
    !("provider" in record)
    || !("model" in record)
    || !("baseUrl" in record)
    || typeof record.provider !== "string"
    || typeof record.model !== "string"
    || (record.baseUrl !== null && typeof record.baseUrl !== "string")
  ) {
    return invalidRequest("Model override fields are invalid.");
  }
  if (typeof record.baseUrl === "string") {
    let url: URL;
    try {
      url = new URL(record.baseUrl);
    } catch {
      return invalidRequest("Model override baseUrl is invalid.");
    }
    if (
      !["http:", "https:"].includes(url.protocol)
      || !url.host
      || url.username
      || url.password
      || url.search
      || url.hash
    ) {
      return invalidRequest("Model override baseUrl is invalid.");
    }
  }
  if (
    "reasoningEffort" in record
    && record.reasoningEffort !== null
    && (typeof record.reasoningEffort !== "string"
      || !REASONING_EFFORTS.has(record.reasoningEffort as NonNullable<CreateRunInput["reasoningEffort"]>))
  ) {
    return invalidRequest("Model override reasoningEffort is invalid.");
  }
  return value as components["schemas"]["ModelConfig"];
}

function checkedCreateRun(input: CreateRunInput): CreateRunInput {
  const record = requestRecord(
    input,
    ["clientRequestId", "message", "personaId", "modelOverride", "reasoningEffort", "workspaceId"],
    "Run input",
  );
  if (!("clientRequestId" in record) || !("message" in record) || typeof record.clientRequestId !== "string") {
    return invalidRequest("Run input fields are invalid.");
  }
  const clientRequestLength = Array.from(record.clientRequestId).length;
  if (clientRequestLength < 1 || clientRequestLength > 128) {
    return invalidRequest("clientRequestId must contain 1 to 128 characters.");
  }
  const message = requestRecord(record.message, ["text", "fileIds"], "Run message");
  if (!("text" in message) || !("fileIds" in message) || typeof message.text !== "string" || !Array.isArray(message.fileIds)) {
    return invalidRequest("Run message fields are invalid.");
  }
  if (Array.from(message.text).length > 1_000_000 || message.fileIds.length > 20) {
    return invalidRequest("Run message exceeds the contract limits.");
  }
  for (const fileId of message.fileIds) checkedIdentifier(fileId as string, "File ID");
  if (
    "personaId" in record
    && record.personaId !== null
    && record.personaId !== undefined
    && (typeof record.personaId !== "string" || !PERSONA_ID_PATTERN.test(record.personaId))
  ) {
    return invalidRequest("Run Persona ID is invalid.");
  }
  if ("modelOverride" in record && record.modelOverride !== null && record.modelOverride !== undefined) {
    checkedModelConfig(record.modelOverride);
  }
  if (
    "reasoningEffort" in record
    && record.reasoningEffort !== null
    && record.reasoningEffort !== undefined
    && (typeof record.reasoningEffort !== "string"
      || !REASONING_EFFORTS.has(record.reasoningEffort as NonNullable<CreateRunInput["reasoningEffort"]>))
  ) {
    return invalidRequest("Run reasoningEffort is invalid.");
  }
  if ("workspaceId" in record && record.workspaceId !== null && record.workspaceId !== undefined) {
    checkedIdentifier(record.workspaceId as string, "Workspace ID");
  }
  return input;
}

function checkedApprovalDecision(value: ApprovalDecision): ApprovalDecision {
  const record = requestRecord(value, ["decision", "reason"], "Approval decision");
  if (
    !("decision" in record)
    || typeof record.decision !== "string"
    || !APPROVAL_CHOICES.has(record.decision as ApprovalDecision["decision"])
    || ("reason" in record && record.reason !== null && typeof record.reason !== "string")
    || (typeof record.reason === "string" && Array.from(record.reason).length > 2_000)
  ) {
    return invalidRequest("Approval decision fields are invalid.");
  }
  return value;
}

function checkedClarificationAnswer(value: ClarificationAnswer): ClarificationAnswer {
  const record = requestRecord(value, ["answer"], "Clarification answer");
  if (
    !("answer" in record)
    || typeof record.answer !== "string"
    || Array.from(record.answer).length > 10_000
  ) {
    return invalidRequest("Clarification answer fields are invalid.");
  }
  return value;
}

class DefaultRunsApi implements RunsApi {
  constructor(private readonly transport: DesktopTransport) {}

  async listActiveRuns(
    profileId: string,
    query: ActiveRunsQuery = {},
    options: DesktopRequestOptions = {},
  ): Promise<ActiveRunList> {
    const params = new URLSearchParams();
    params.set("profileId", checkedProfileId(profileId));
    params.set("state", "active");
    const sessionId = checkedActiveRunsQuery(query);
    if (sessionId !== undefined) params.set("sessionId", sessionId);
    const response = await this.transport.request(
      `/api/v1/runs?${params.toString()}`,
      { method: "GET", headers: { Accept: "application/json" } },
      options,
    );
    const result = await parsedResponse(
      response,
      200,
      "Active Run list response",
      parseActiveRunList,
    );
    if (result.items.some((item) => (
      item.run.profileId !== profileId
      || (sessionId !== undefined && item.run.sessionId !== sessionId)
    ))) {
      return invalidResponse("Active Run list owner binding");
    }
    return result;
  }

  async createRun(
    sessionId: string,
    input: CreateRunInput,
    idempotencyKey: string,
    options: DesktopRequestOptions = {},
  ): Promise<RunAccepted> {
    const response = await this.transport.request(
      `/api/v1/sessions/${checkedIdentifier(sessionId, "Session ID")}/runs`,
      {
        method: "POST",
        headers: {
          Accept: "application/json",
          "Content-Type": "application/json",
          "Idempotency-Key": checkedIdempotencyKey(idempotencyKey),
        },
        body: JSON.stringify(checkedCreateRun(input)),
      },
      options,
    );
    const accepted = await parsedResponse(response, 202, "Run accepted response", parseRunAccepted);
    if (accepted.run.sessionId !== sessionId || accepted.userMessage.sessionId !== sessionId) {
      return invalidResponse("Run accepted response Session binding");
    }
    return accepted;
  }

  async getRun(runId: string, options: DesktopRequestOptions = {}): Promise<Run> {
    const response = await this.transport.request(
      `/api/v1/runs/${checkedIdentifier(runId, "Run ID")}`,
      { method: "GET", headers: { Accept: "application/json" } },
      options,
    );
    const run = await parsedResponse(response, 200, "Run response", parseRun);
    if (run.id !== runId) return invalidResponse("Run response.id");
    return run;
  }

  async cancelRun(runId: string, options: DesktopRequestOptions = {}): Promise<Run> {
    const response = await this.transport.request(
      `/api/v1/runs/${checkedIdentifier(runId, "Run ID")}/cancel`,
      { method: "POST", headers: { Accept: "application/json" } },
      options,
    );
    const run = await parsedResponse(response, 202, "Cancelled Run response", parseRun);
    if (run.id !== runId) return invalidResponse("Cancelled Run response.id");
    return run;
  }

  async resolveApproval(
    runId: string,
    approvalId: string,
    decision: ApprovalDecision,
    options: DesktopRequestOptions = {},
  ): Promise<ActionAccepted> {
    const response = await this.transport.request(
      `/api/v1/runs/${checkedIdentifier(runId, "Run ID")}/approvals/${checkedIdentifier(approvalId, "Approval ID")}`,
      {
        method: "POST",
        headers: { Accept: "application/json", "Content-Type": "application/json" },
        body: JSON.stringify(checkedApprovalDecision(decision)),
      },
      options,
    );
    return parsedResponse(response, 200, "Approval response", parseActionAccepted);
  }

  async answerClarification(
    runId: string,
    requestId: string,
    answer: ClarificationAnswer,
    options: DesktopRequestOptions = {},
  ): Promise<ActionAccepted> {
    const response = await this.transport.request(
      `/api/v1/runs/${checkedIdentifier(runId, "Run ID")}/clarifications/${checkedIdentifier(requestId, "Clarification request ID")}`,
      {
        method: "POST",
        headers: { Accept: "application/json", "Content-Type": "application/json" },
        body: JSON.stringify(checkedClarificationAnswer(answer)),
      },
      options,
    );
    return parsedResponse(response, 200, "Clarification response", parseActionAccepted);
  }
}

export function createRunsApi(transport: DesktopTransport = desktopTransport): RunsApi {
  return new DefaultRunsApi(transport);
}

export const runsApi = createRunsApi();
