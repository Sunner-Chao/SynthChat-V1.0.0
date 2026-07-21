import {
  desktopTransport,
  type DesktopRequestOptions,
  type DesktopTransport,
} from "./desktopConnection";

export interface PersonaInput {
  name: string;
  avatar?: string | null;
  systemPrompt?: string;
  characterPrompt?: string;
  outputExamples?: string;
  systemInstructions?: string;
  provider?: string;
  model?: string;
  temperature?: number;
  maxTokens?: number;
  toolsEnabled?: boolean;
  memoryEnabled?: boolean;
  proactiveEnabled?: boolean;
  legacyAgentId?: string | null;
}

export interface Persona {
  id: string;
  name: string;
  avatar: string | null;
  systemPrompt: string;
  characterPrompt: string;
  outputExamples: string;
  systemInstructions: string;
  provider: string;
  model: string;
  temperature: number;
  maxTokens: number;
  toolsEnabled: boolean;
  memoryEnabled: boolean;
  proactiveEnabled: boolean;
  legacyAgentId: string | null;
  createdAt: string;
  updatedAt: string;
  revision: number;
}

export interface WorldbookSectionInput {
  key: string;
  content: string;
  enabled?: boolean;
}

export interface WorldbookSection {
  id: string;
  key: string;
  content: string;
  enabled: boolean;
}

export interface WorldbookInput {
  name: string;
  description?: string;
  boundPersonaIds?: string[];
  sections?: WorldbookSectionInput[];
}

export interface Worldbook {
  id: string;
  name: string;
  description: string;
  boundPersonaIds: string[];
  sections: WorldbookSection[];
  createdAt: string;
  updatedAt: string;
  revision: number;
}

export interface MomentComment {
  id: string;
  authorId: string;
  text: string;
  replyTo: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface MomentInput {
  authorId?: string;
  body: string;
  coverFileId?: string | null;
}

export interface Moment {
  id: string;
  authorId: string;
  body: string;
  coverFileId: string | null;
  likedBy: string[];
  comments: MomentComment[];
  createdAt: string;
  updatedAt: string;
  revision: number;
}

export interface MomentCommentInput {
  authorId?: string;
  text: string;
  replyTo?: string | null;
}

export interface MomentLikeInput {
  actorId?: string;
  liked: boolean;
}

export interface VersionedProduct<T> {
  value: T;
  etag: string;
}

export type ProductCatalogApiErrorKind = "http" | "invalid_request" | "invalid_response";

export class ProductCatalogApiError extends Error {
  readonly kind: ProductCatalogApiErrorKind;
  readonly status?: number;
  readonly code?: string;
  readonly requestId?: string;
  readonly retryable: boolean;

  constructor(
    kind: ProductCatalogApiErrorKind,
    message: string,
    options: {
      status?: number;
      code?: string;
      requestId?: string;
      retryable?: boolean;
    } = {},
  ) {
    super(message);
    this.name = "ProductCatalogApiError";
    this.kind = kind;
    this.status = options.status;
    this.code = options.code;
    this.requestId = options.requestId;
    this.retryable = options.retryable ?? false;
  }
}

export interface ProductCatalogApi {
  listPersonas(profileId: string, query?: string, options?: DesktopRequestOptions): Promise<Persona[]>;
  createPersona(profileId: string, input: PersonaInput, options?: DesktopRequestOptions): Promise<VersionedProduct<Persona>>;
  getPersona(profileId: string, personaId: string, options?: DesktopRequestOptions): Promise<VersionedProduct<Persona>>;
  updatePersona(profileId: string, personaId: string, input: PersonaInput, etag: string, options?: DesktopRequestOptions): Promise<VersionedProduct<Persona>>;
  deletePersona(profileId: string, personaId: string, etag: string, options?: DesktopRequestOptions): Promise<void>;
  listWorldbooks(profileId: string, query?: string, options?: DesktopRequestOptions): Promise<Worldbook[]>;
  createWorldbook(profileId: string, input: WorldbookInput, options?: DesktopRequestOptions): Promise<VersionedProduct<Worldbook>>;
  getWorldbook(profileId: string, worldbookId: string, options?: DesktopRequestOptions): Promise<VersionedProduct<Worldbook>>;
  updateWorldbook(profileId: string, worldbookId: string, input: WorldbookInput, etag: string, options?: DesktopRequestOptions): Promise<VersionedProduct<Worldbook>>;
  deleteWorldbook(profileId: string, worldbookId: string, etag: string, options?: DesktopRequestOptions): Promise<void>;
  listMoments(profileId: string, options?: DesktopRequestOptions): Promise<Moment[]>;
  createMoment(profileId: string, input: MomentInput, options?: DesktopRequestOptions): Promise<VersionedProduct<Moment>>;
  getMoment(profileId: string, momentId: string, options?: DesktopRequestOptions): Promise<VersionedProduct<Moment>>;
  updateMoment(profileId: string, momentId: string, input: MomentInput, etag: string, options?: DesktopRequestOptions): Promise<VersionedProduct<Moment>>;
  deleteMoment(profileId: string, momentId: string, etag: string, options?: DesktopRequestOptions): Promise<void>;
  addMomentComment(profileId: string, momentId: string, input: MomentCommentInput, etag: string, options?: DesktopRequestOptions): Promise<VersionedProduct<Moment>>;
  deleteMomentComment(profileId: string, momentId: string, commentId: string, etag: string, options?: DesktopRequestOptions): Promise<VersionedProduct<Moment>>;
  setMomentLike(profileId: string, momentId: string, input: MomentLikeInput, etag: string, options?: DesktopRequestOptions): Promise<VersionedProduct<Moment>>;
}

interface ProblemDetails {
  type: string;
  title: string;
  status: number;
  detail: string;
  instance: string;
  code: string;
  requestId: string;
  retryable: boolean;
}

type ProductKind = "persona" | "worldbook" | "moment";

const PROFILE_ID_PATTERN = /^(?:default|[a-z0-9_][a-z0-9_-]{0,63})$/u;
const ITEM_ID_PATTERN = /^[A-Za-z0-9_.-]{1,256}$/u;
const RFC3339_PATTERN = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/u;
const MAX_NAME_CHARS = 120;
const MAX_PROMPT_CHARS = 64_000;
const MAX_BODY_CHARS = 16_000;
const MAX_DESCRIPTION_CHARS = 8_000;
const MAX_SECTIONS = 200;
const MAX_COMMENTS = 1_000;
const MAX_BINDINGS = 200;

function invalidResponse(context: string): never {
  throw new ProductCatalogApiError(
    "invalid_response",
    `${context} did not match the API v1 contract.`,
  );
}

function invalidRequest(message: string): never {
  throw new ProductCatalogApiError("invalid_request", message);
}

function asRecord(value: unknown, context: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return invalidResponse(context);
  }
  return value as Record<string, unknown>;
}

function asInputRecord(value: unknown, context: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return invalidRequest(`${context} is invalid.`);
  }
  return value as Record<string, unknown>;
}

function exactKeys(
  record: Record<string, unknown>,
  required: readonly string[],
  optional: readonly string[],
  context: string,
  request = false,
): void {
  const allowed = new Set([...required, ...optional]);
  if (
    required.some((key) => !(key in record))
    || Object.keys(record).some((key) => !allowed.has(key))
  ) {
    if (request) invalidRequest(`${context} is invalid.`);
    invalidResponse(context);
  }
}

function stringValue(value: unknown, context: string): string {
  if (typeof value !== "string") return invalidResponse(context);
  return value;
}

function booleanValue(value: unknown, context: string): boolean {
  if (typeof value !== "boolean") return invalidResponse(context);
  return value;
}

function positiveRevision(value: unknown, context: string): number {
  if (!Number.isSafeInteger(value) || (value as number) < 1) return invalidResponse(context);
  return value as number;
}

function dateTime(value: unknown, context: string): string {
  const result = stringValue(value, context);
  if (!RFC3339_PATTERN.test(result) || Number.isNaN(Date.parse(result))) {
    return invalidResponse(context);
  }
  return result;
}

function boundedResponseText(
  value: unknown,
  max: number,
  context: string,
  nonEmpty = false,
): string {
  const result = stringValue(value, context);
  if (
    result.includes("\0")
    || Array.from(result).length > max
    || (nonEmpty && !result.trim())
  ) {
    return invalidResponse(context);
  }
  return result;
}

function responseId(value: unknown, context: string): string {
  const result = stringValue(value, context);
  if (!ITEM_ID_PATTERN.test(result)) return invalidResponse(context);
  return result;
}

function optionalBoundedResponseText(value: unknown, max: number, context: string): string | null {
  if (value === null) return null;
  return boundedResponseText(value, max, context);
}

function unique(values: string[], context: string): string[] {
  if (new Set(values).size !== values.length) return invalidResponse(context);
  return values;
}

export function parsePersona(value: unknown): Persona {
  const record = asRecord(value, "Persona");
  exactKeys(record, [
    "id", "name", "avatar", "systemPrompt", "characterPrompt", "outputExamples",
    "systemInstructions", "provider", "model", "temperature", "maxTokens",
    "toolsEnabled", "memoryEnabled", "proactiveEnabled", "legacyAgentId",
    "createdAt", "updatedAt", "revision",
  ], [], "Persona");
  if (
    typeof record.temperature !== "number"
    || !Number.isFinite(record.temperature)
    || record.temperature < 0
    || record.temperature > 2
    || !Number.isSafeInteger(record.maxTokens)
    || (record.maxTokens as number) < 1
    || (record.maxTokens as number) > 1_000_000
  ) {
    return invalidResponse("Persona generation settings");
  }
  return {
    id: responseId(record.id, "Persona.id"),
    name: boundedResponseText(record.name, MAX_NAME_CHARS, "Persona.name", true),
    avatar: optionalBoundedResponseText(record.avatar, 4_096, "Persona.avatar"),
    systemPrompt: boundedResponseText(record.systemPrompt, MAX_PROMPT_CHARS, "Persona.systemPrompt"),
    characterPrompt: boundedResponseText(record.characterPrompt, MAX_PROMPT_CHARS, "Persona.characterPrompt"),
    outputExamples: boundedResponseText(record.outputExamples, MAX_PROMPT_CHARS, "Persona.outputExamples"),
    systemInstructions: boundedResponseText(record.systemInstructions, MAX_PROMPT_CHARS, "Persona.systemInstructions"),
    provider: boundedResponseText(record.provider, 256, "Persona.provider"),
    model: boundedResponseText(record.model, 256, "Persona.model"),
    temperature: record.temperature,
    maxTokens: record.maxTokens as number,
    toolsEnabled: booleanValue(record.toolsEnabled, "Persona.toolsEnabled"),
    memoryEnabled: booleanValue(record.memoryEnabled, "Persona.memoryEnabled"),
    proactiveEnabled: booleanValue(record.proactiveEnabled, "Persona.proactiveEnabled"),
    legacyAgentId: optionalBoundedResponseText(record.legacyAgentId, 256, "Persona.legacyAgentId"),
    createdAt: dateTime(record.createdAt, "Persona.createdAt"),
    updatedAt: dateTime(record.updatedAt, "Persona.updatedAt"),
    revision: positiveRevision(record.revision, "Persona.revision"),
  };
}

function parseWorldbookSection(value: unknown): WorldbookSection {
  const record = asRecord(value, "Worldbook section");
  exactKeys(record, ["id", "key", "content", "enabled"], [], "Worldbook section");
  return {
    id: responseId(record.id, "Worldbook section.id"),
    key: boundedResponseText(record.key, 300, "Worldbook section.key", true),
    content: boundedResponseText(record.content, MAX_PROMPT_CHARS, "Worldbook section.content", true),
    enabled: booleanValue(record.enabled, "Worldbook section.enabled"),
  };
}

export function parseWorldbook(value: unknown): Worldbook {
  const record = asRecord(value, "Worldbook");
  exactKeys(record, [
    "id", "name", "description", "boundPersonaIds", "sections", "createdAt", "updatedAt", "revision",
  ], [], "Worldbook");
  if (!Array.isArray(record.boundPersonaIds) || !Array.isArray(record.sections)) {
    return invalidResponse("Worldbook collections");
  }
  if (record.boundPersonaIds.length > MAX_BINDINGS || record.sections.length > MAX_SECTIONS) {
    return invalidResponse("Worldbook collection limits");
  }
  const boundPersonaIds = record.boundPersonaIds.map((id) => responseId(id, "Worldbook.boundPersonaIds"));
  const sections = record.sections.map(parseWorldbookSection);
  unique(sections.map((section) => section.id), "Worldbook section IDs");
  return {
    id: responseId(record.id, "Worldbook.id"),
    name: boundedResponseText(record.name, MAX_NAME_CHARS, "Worldbook.name", true),
    description: boundedResponseText(record.description, MAX_DESCRIPTION_CHARS, "Worldbook.description"),
    boundPersonaIds,
    sections,
    createdAt: dateTime(record.createdAt, "Worldbook.createdAt"),
    updatedAt: dateTime(record.updatedAt, "Worldbook.updatedAt"),
    revision: positiveRevision(record.revision, "Worldbook.revision"),
  };
}

function parseMomentComment(value: unknown): MomentComment {
  const record = asRecord(value, "Moment comment");
  exactKeys(record, ["id", "authorId", "text", "replyTo", "createdAt", "updatedAt"], [], "Moment comment");
  return {
    id: responseId(record.id, "Moment comment.id"),
    authorId: boundedResponseText(record.authorId, MAX_NAME_CHARS, "Moment comment.authorId", true),
    text: boundedResponseText(record.text, MAX_BODY_CHARS, "Moment comment.text", true),
    replyTo: record.replyTo === null ? null : responseId(record.replyTo, "Moment comment.replyTo"),
    createdAt: dateTime(record.createdAt, "Moment comment.createdAt"),
    updatedAt: dateTime(record.updatedAt, "Moment comment.updatedAt"),
  };
}

export function parseMoment(value: unknown): Moment {
  const record = asRecord(value, "Moment");
  exactKeys(record, [
    "id", "authorId", "body", "coverFileId", "likedBy", "comments", "createdAt", "updatedAt", "revision",
  ], [], "Moment");
  if (!Array.isArray(record.likedBy) || !Array.isArray(record.comments)) {
    return invalidResponse("Moment collections");
  }
  if (record.comments.length > MAX_COMMENTS) return invalidResponse("Moment comments");
  const likedBy = unique(record.likedBy.map((actor) => (
    boundedResponseText(actor, MAX_NAME_CHARS, "Moment.likedBy", true)
  )), "Moment.likedBy");
  const comments = record.comments.map(parseMomentComment);
  const commentIds = unique(comments.map((comment) => comment.id), "Moment comment IDs");
  if (comments.some((comment) => comment.replyTo !== null && !commentIds.includes(comment.replyTo))) {
    return invalidResponse("Moment comment reply target");
  }
  return {
    id: responseId(record.id, "Moment.id"),
    authorId: boundedResponseText(record.authorId, MAX_NAME_CHARS, "Moment.authorId", true),
    body: boundedResponseText(record.body, MAX_BODY_CHARS, "Moment.body", true),
    coverFileId: record.coverFileId === null ? null : responseId(record.coverFileId, "Moment.coverFileId"),
    likedBy,
    comments,
    createdAt: dateTime(record.createdAt, "Moment.createdAt"),
    updatedAt: dateTime(record.updatedAt, "Moment.updatedAt"),
    revision: positiveRevision(record.revision, "Moment.revision"),
  };
}

function parseProblemDetails(value: unknown): ProblemDetails {
  const record = asRecord(value, "Problem details");
  exactKeys(record, [
    "type", "title", "status", "detail", "instance", "code", "requestId", "retryable",
  ], [], "Problem details");
  if (!Number.isInteger(record.status) || (record.status as number) < 400 || (record.status as number) > 599) {
    return invalidResponse("Problem details.status");
  }
  return {
    type: stringValue(record.type, "Problem details.type"),
    title: stringValue(record.title, "Problem details.title"),
    status: record.status as number,
    detail: stringValue(record.detail, "Problem details.detail"),
    instance: stringValue(record.instance, "Problem details.instance"),
    code: stringValue(record.code, "Problem details.code"),
    requestId: stringValue(record.requestId, "Problem details.requestId"),
    retryable: booleanValue(record.retryable, "Problem details.retryable"),
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

function productEtag(kind: ProductKind, revision: number): string {
  return `"product-${kind}-${revision}"`;
}

function checkedProductEtag(value: string, kind: ProductKind): string {
  const match = new RegExp(`^"product-${kind}-([1-9]\\d*)"$`, "u").exec(value);
  if (!match || !Number.isSafeInteger(Number(match[1]))) {
    return invalidRequest(`A strong ${kind} ETag is required.`);
  }
  return value;
}

function requiredMutationEtag(response: Response, kind: ProductKind, revision: number): string {
  const expected = productEtag(kind, revision);
  if (response.headers.get("etag") !== expected) return invalidResponse(`${kind} response ETag`);
  return expected;
}

function checkedProfileId(profileId: string): string {
  if (typeof profileId !== "string" || !PROFILE_ID_PATTERN.test(profileId)) {
    return invalidRequest("Profile ID is invalid.");
  }
  return encodeURIComponent(profileId);
}

function checkedItemId(value: string, context: string): string {
  if (typeof value !== "string" || !ITEM_ID_PATTERN.test(value)) {
    return invalidRequest(`${context} is invalid.`);
  }
  return encodeURIComponent(value);
}

function checkedQuery(value: string | undefined): string {
  if (value === undefined) return "";
  if (
    typeof value !== "string"
    || Array.from(value).length > 200
    || /[\u0000-\u001f\u007f]/u.test(value)
  ) {
    return invalidRequest("Product catalog query is invalid.");
  }
  return `?${new URLSearchParams({ q: value })}`;
}

function requestText(value: unknown, max: number, context: string, nonEmpty = false): string {
  if (
    typeof value !== "string"
    || value.includes("\0")
    || Array.from(value).length > max
    || (nonEmpty && !value.trim())
  ) {
    return invalidRequest(`${context} is invalid.`);
  }
  return value;
}

function requestNullableText(value: unknown, max: number, context: string): string | null {
  if (value === null) return null;
  return requestText(value, max, context);
}

function requestId(value: unknown, context: string): string {
  if (typeof value !== "string" || !ITEM_ID_PATTERN.test(value)) {
    return invalidRequest(`${context} is invalid.`);
  }
  return value;
}

function checkedPersonaInput(input: PersonaInput): PersonaInput {
  const record = asInputRecord(input, "Persona input");
  const optional = [
    "avatar", "systemPrompt", "characterPrompt", "outputExamples", "systemInstructions",
    "provider", "model", "temperature", "maxTokens", "toolsEnabled", "memoryEnabled",
    "proactiveEnabled", "legacyAgentId",
  ] as const;
  exactKeys(record, ["name"], optional, "Persona input", true);
  const result: PersonaInput = {
    name: requestText(record.name, MAX_NAME_CHARS, "Persona name", true),
  };
  if ("avatar" in record) result.avatar = requestNullableText(record.avatar, 4_096, "Persona avatar");
  for (const [key, max] of [
    ["systemPrompt", MAX_PROMPT_CHARS], ["characterPrompt", MAX_PROMPT_CHARS],
    ["outputExamples", MAX_PROMPT_CHARS], ["systemInstructions", MAX_PROMPT_CHARS],
    ["provider", 256], ["model", 256],
  ] as const) {
    if (key in record) result[key] = requestText(record[key], max, `Persona ${key}`);
  }
  if ("temperature" in record) {
    if (typeof record.temperature !== "number" || !Number.isFinite(record.temperature) || record.temperature < 0 || record.temperature > 2) {
      return invalidRequest("Persona temperature is invalid.");
    }
    result.temperature = record.temperature;
  }
  if ("maxTokens" in record) {
    if (!Number.isSafeInteger(record.maxTokens) || (record.maxTokens as number) < 1 || (record.maxTokens as number) > 1_000_000) {
      return invalidRequest("Persona maxTokens is invalid.");
    }
    result.maxTokens = record.maxTokens as number;
  }
  for (const key of ["toolsEnabled", "memoryEnabled", "proactiveEnabled"] as const) {
    if (key in record) {
      if (typeof record[key] !== "boolean") return invalidRequest(`Persona ${key} is invalid.`);
      result[key] = record[key];
    }
  }
  if ("legacyAgentId" in record) {
    result.legacyAgentId = requestNullableText(record.legacyAgentId, 256, "Persona legacyAgentId");
  }
  return result;
}

function checkedWorldbookInput(input: WorldbookInput): WorldbookInput {
  const record = asInputRecord(input, "Worldbook input");
  exactKeys(record, ["name"], ["description", "boundPersonaIds", "sections"], "Worldbook input", true);
  const result: WorldbookInput = {
    name: requestText(record.name, MAX_NAME_CHARS, "Worldbook name", true),
  };
  if ("description" in record) result.description = requestText(record.description, MAX_DESCRIPTION_CHARS, "Worldbook description");
  if ("boundPersonaIds" in record) {
    if (!Array.isArray(record.boundPersonaIds) || record.boundPersonaIds.length > MAX_BINDINGS) {
      return invalidRequest("Worldbook persona bindings are invalid.");
    }
    result.boundPersonaIds = record.boundPersonaIds.map((id) => requestId(id, "Worldbook persona binding"));
  }
  if ("sections" in record) {
    if (!Array.isArray(record.sections) || record.sections.length > MAX_SECTIONS) {
      return invalidRequest("Worldbook sections are invalid.");
    }
    result.sections = record.sections.map((section) => {
      const item = asInputRecord(section, "Worldbook section input");
      exactKeys(item, ["key", "content"], ["enabled"], "Worldbook section input", true);
      const parsed: WorldbookSectionInput = {
        key: requestText(item.key, 300, "Worldbook section key", true),
        content: requestText(item.content, MAX_PROMPT_CHARS, "Worldbook section content", true),
      };
      if ("enabled" in item) {
        if (typeof item.enabled !== "boolean") return invalidRequest("Worldbook section enabled is invalid.");
        parsed.enabled = item.enabled;
      }
      return parsed;
    });
  }
  return result;
}

function checkedMomentInput(input: MomentInput): MomentInput {
  const record = asInputRecord(input, "Moment input");
  exactKeys(record, ["body"], ["authorId", "coverFileId"], "Moment input", true);
  const result: MomentInput = {
    body: requestText(record.body, MAX_BODY_CHARS, "Moment body", true),
  };
  if ("authorId" in record) result.authorId = requestText(record.authorId, MAX_NAME_CHARS, "Moment authorId", true);
  if ("coverFileId" in record) {
    result.coverFileId = record.coverFileId === null ? null : requestId(record.coverFileId, "Moment coverFileId");
  }
  return result;
}

function checkedMomentCommentInput(input: MomentCommentInput): MomentCommentInput {
  const record = asInputRecord(input, "Moment comment input");
  exactKeys(record, ["text"], ["authorId", "replyTo"], "Moment comment input", true);
  const result: MomentCommentInput = {
    text: requestText(record.text, MAX_BODY_CHARS, "Moment comment text", true),
  };
  if ("authorId" in record) result.authorId = requestText(record.authorId, MAX_NAME_CHARS, "Moment comment authorId", true);
  if ("replyTo" in record) result.replyTo = record.replyTo === null ? null : requestId(record.replyTo, "Moment comment replyTo");
  return result;
}

function checkedMomentLikeInput(input: MomentLikeInput): MomentLikeInput {
  const record = asInputRecord(input, "Moment like input");
  exactKeys(record, ["liked"], ["actorId"], "Moment like input", true);
  if (typeof record.liked !== "boolean") return invalidRequest("Moment liked is invalid.");
  const result: MomentLikeInput = { liked: record.liked };
  if ("actorId" in record) result.actorId = requestText(record.actorId, MAX_NAME_CHARS, "Moment actorId", true);
  return result;
}

async function throwHttpError(response: Response): Promise<never> {
  const problem = parseProblemDetails(await jsonPayload(response, "Product catalog error response"));
  if (problem.status !== response.status) invalidResponse("Problem details.status");
  throw new ProductCatalogApiError("http", problem.title, {
    status: problem.status,
    code: problem.code,
    requestId: problem.requestId,
    retryable: problem.retryable,
  });
}

function parseList<T>(value: unknown, parser: (item: unknown) => T, id: (item: T) => string, context: string): T[] {
  if (!Array.isArray(value) || value.length > 2_000) return invalidResponse(context);
  const items = value.map(parser);
  unique(items.map(id), `${context} IDs`);
  return items;
}

class DefaultProductCatalogApi implements ProductCatalogApi {
  constructor(private readonly transport: DesktopTransport) {}

  private async list<T>(profileId: string, path: string, parser: (value: unknown) => T, options: DesktopRequestOptions): Promise<T[]> {
    const response = await this.transport.request(`/api/v1/profiles/${checkedProfileId(profileId)}/${path}`, {
      method: "GET",
      headers: { Accept: "application/json" },
    }, options);
    if (response.status !== 200) return throwHttpError(response);
    return parseList(await jsonPayload(response, `${path} response`), parser, (item) => (item as { id: string }).id, `${path} response`);
  }

  private async get<T extends { revision: number }>(profileId: string, path: string, kind: ProductKind, parser: (value: unknown) => T, options: DesktopRequestOptions): Promise<VersionedProduct<T>> {
    const response = await this.transport.request(`/api/v1/profiles/${checkedProfileId(profileId)}/${path}`, {
      method: "GET",
      headers: { Accept: "application/json" },
    }, options);
    if (response.status !== 200) return throwHttpError(response);
    const value = parser(await jsonPayload(response, `${kind} response`));
    return { value, etag: productEtag(kind, value.revision) };
  }

  private async write<T extends { revision: number }>(profileId: string, path: string, method: "POST" | "PATCH" | "PUT", input: unknown, kind: ProductKind, expectedStatus: 200 | 201, parser: (value: unknown) => T, etag: string | undefined, options: DesktopRequestOptions): Promise<VersionedProduct<T>> {
    const headers: Record<string, string> = { Accept: "application/json", "Content-Type": "application/json" };
    if (etag !== undefined) headers["If-Match"] = checkedProductEtag(etag, kind);
    const response = await this.transport.request(`/api/v1/profiles/${checkedProfileId(profileId)}/${path}`, {
      method,
      headers,
      body: JSON.stringify(input),
    }, options);
    if (response.status !== expectedStatus) return throwHttpError(response);
    const value = parser(await jsonPayload(response, `${kind} response`));
    return { value, etag: requiredMutationEtag(response, kind, value.revision) };
  }

  private async remove(profileId: string, path: string, kind: ProductKind, etag: string, options: DesktopRequestOptions): Promise<void> {
    const response = await this.transport.request(`/api/v1/profiles/${checkedProfileId(profileId)}/${path}`, {
      method: "DELETE",
      headers: { Accept: "application/json", "If-Match": checkedProductEtag(etag, kind) },
    }, options);
    if (response.status !== 204) return throwHttpError(response);
  }

  async listPersonas(profileId: string, query?: string, options: DesktopRequestOptions = {}): Promise<Persona[]> {
    return this.list(profileId, `personas${checkedQuery(query)}`, parsePersona, options);
  }

  async createPersona(profileId: string, input: PersonaInput, options: DesktopRequestOptions = {}): Promise<VersionedProduct<Persona>> {
    return this.write(profileId, "personas", "POST", checkedPersonaInput(input), "persona", 201, parsePersona, undefined, options);
  }

  async getPersona(profileId: string, personaId: string, options: DesktopRequestOptions = {}): Promise<VersionedProduct<Persona>> {
    return this.get(profileId, `personas/${checkedItemId(personaId, "Persona ID")}`, "persona", parsePersona, options);
  }

  async updatePersona(profileId: string, personaId: string, input: PersonaInput, etag: string, options: DesktopRequestOptions = {}): Promise<VersionedProduct<Persona>> {
    return this.write(profileId, `personas/${checkedItemId(personaId, "Persona ID")}`, "PATCH", checkedPersonaInput(input), "persona", 200, parsePersona, etag, options);
  }

  async deletePersona(profileId: string, personaId: string, etag: string, options: DesktopRequestOptions = {}): Promise<void> {
    return this.remove(profileId, `personas/${checkedItemId(personaId, "Persona ID")}`, "persona", etag, options);
  }

  async listWorldbooks(profileId: string, query?: string, options: DesktopRequestOptions = {}): Promise<Worldbook[]> {
    return this.list(profileId, `worldbooks${checkedQuery(query)}`, parseWorldbook, options);
  }

  async createWorldbook(profileId: string, input: WorldbookInput, options: DesktopRequestOptions = {}): Promise<VersionedProduct<Worldbook>> {
    return this.write(profileId, "worldbooks", "POST", checkedWorldbookInput(input), "worldbook", 201, parseWorldbook, undefined, options);
  }

  async getWorldbook(profileId: string, worldbookId: string, options: DesktopRequestOptions = {}): Promise<VersionedProduct<Worldbook>> {
    return this.get(profileId, `worldbooks/${checkedItemId(worldbookId, "Worldbook ID")}`, "worldbook", parseWorldbook, options);
  }

  async updateWorldbook(profileId: string, worldbookId: string, input: WorldbookInput, etag: string, options: DesktopRequestOptions = {}): Promise<VersionedProduct<Worldbook>> {
    return this.write(profileId, `worldbooks/${checkedItemId(worldbookId, "Worldbook ID")}`, "PATCH", checkedWorldbookInput(input), "worldbook", 200, parseWorldbook, etag, options);
  }

  async deleteWorldbook(profileId: string, worldbookId: string, etag: string, options: DesktopRequestOptions = {}): Promise<void> {
    return this.remove(profileId, `worldbooks/${checkedItemId(worldbookId, "Worldbook ID")}`, "worldbook", etag, options);
  }

  async listMoments(profileId: string, options: DesktopRequestOptions = {}): Promise<Moment[]> {
    return this.list(profileId, "moments", parseMoment, options);
  }

  async createMoment(profileId: string, input: MomentInput, options: DesktopRequestOptions = {}): Promise<VersionedProduct<Moment>> {
    return this.write(profileId, "moments", "POST", checkedMomentInput(input), "moment", 201, parseMoment, undefined, options);
  }

  async getMoment(profileId: string, momentId: string, options: DesktopRequestOptions = {}): Promise<VersionedProduct<Moment>> {
    return this.get(profileId, `moments/${checkedItemId(momentId, "Moment ID")}`, "moment", parseMoment, options);
  }

  async updateMoment(profileId: string, momentId: string, input: MomentInput, etag: string, options: DesktopRequestOptions = {}): Promise<VersionedProduct<Moment>> {
    return this.write(profileId, `moments/${checkedItemId(momentId, "Moment ID")}`, "PATCH", checkedMomentInput(input), "moment", 200, parseMoment, etag, options);
  }

  async deleteMoment(profileId: string, momentId: string, etag: string, options: DesktopRequestOptions = {}): Promise<void> {
    return this.remove(profileId, `moments/${checkedItemId(momentId, "Moment ID")}`, "moment", etag, options);
  }

  async addMomentComment(profileId: string, momentId: string, input: MomentCommentInput, etag: string, options: DesktopRequestOptions = {}): Promise<VersionedProduct<Moment>> {
    return this.write(profileId, `moments/${checkedItemId(momentId, "Moment ID")}/comments`, "POST", checkedMomentCommentInput(input), "moment", 200, parseMoment, etag, options);
  }

  async deleteMomentComment(profileId: string, momentId: string, commentId: string, etag: string, options: DesktopRequestOptions = {}): Promise<VersionedProduct<Moment>> {
    return this.writeDeleteResult(profileId, `moments/${checkedItemId(momentId, "Moment ID")}/comments/${checkedItemId(commentId, "Moment comment ID")}`, etag, options);
  }

  async setMomentLike(profileId: string, momentId: string, input: MomentLikeInput, etag: string, options: DesktopRequestOptions = {}): Promise<VersionedProduct<Moment>> {
    return this.write(profileId, `moments/${checkedItemId(momentId, "Moment ID")}/like`, "PUT", checkedMomentLikeInput(input), "moment", 200, parseMoment, etag, options);
  }

  private async writeDeleteResult(profileId: string, path: string, etag: string, options: DesktopRequestOptions): Promise<VersionedProduct<Moment>> {
    const response = await this.transport.request(`/api/v1/profiles/${checkedProfileId(profileId)}/${path}`, {
      method: "DELETE",
      headers: { Accept: "application/json", "If-Match": checkedProductEtag(etag, "moment") },
    }, options);
    if (response.status !== 200) return throwHttpError(response);
    const value = parseMoment(await jsonPayload(response, "moment response"));
    return { value, etag: requiredMutationEtag(response, "moment", value.revision) };
  }
}

export function createProductCatalogApi(
  transport: DesktopTransport = desktopTransport,
): ProductCatalogApi {
  return new DefaultProductCatalogApi(transport);
}

export const productCatalogApi = createProductCatalogApi();
