export type AppSection =
  | "chat"
  | "contacts"
  | "discover"
  | "moments"
  | "personas"
  | "memory"
  | "worldbooks"
  | "plugins"
  | "mcp"
  | "agents"
  | "skills"
  | "settings";

export interface WebConfig {
  port: number;
  password: string;
  publicEnabled: boolean;
  publicPort: number;
  publicSecret: string;
}

export interface WeatherConfig {
  qweatherApiKey: string;
  qweatherApiHost: string;
  defaultLocation: string;
  timeoutSeconds: number;
}

export interface ChatConfig {
  skipEnvCheck?: boolean;
  agentEngine?: "legacy" | "langgraph" | string;
  maxContextRounds: number;
  shortContextMode?: "messages" | "tokens";
  shortContextTokenBudget?: number;
  shortContextAbortOnSummaryFailure?: boolean;
  shortContextSummaryProviderId?: string;
  shortContextSummaryModel?: string;
  busyInputMode?: "queue" | "steer" | "interrupt" | string;
  autoTitleEnabled?: boolean;
  queueWaitSeconds: number;
  messageDedupEnabled?: boolean;
  messageDedupWindowSeconds?: number;
  delegationMaxConcurrentChildren?: number;
  delegationStrategy?: string;
  delegationOrchestratorEnabled?: boolean;
  delegationSubagentAutoApprove?: boolean;
  delegationInheritMcpToolsets?: boolean;
  delegationSubagentProviderId?: string;
  delegationSubagentModel?: string;
  auxiliaryTaskAssignments?: Record<string, Record<string, unknown>>;
  agentRunTimeoutSeconds?: number;
  uiMessageLimit?: number;
  artifactScanLimit?: number;
  uiMessagePreviewChars?: number;
  uiStreamCharsPerSecond?: number;
  thinkingMinVisibleMs?: number;
  petCloudDurationSeconds?: number;
  bottomFollowThresholdPx?: number;
  activePollIntervalMs?: number;
  idlePollIntervalMs?: number;
  intentAnalyzerMode?: "embedding" | "llm" | string;
  intentAnalyzerProviderId?: string;
  intentAnalyzerModel?: string;
  intentEmbeddingMinConfidence?: number;
  intentLlmMinConfidence?: number;
  intentLlmTimeoutSeconds?: number;
  intentLlmMaxTokens?: number;
  intentLlmPrompt?: string;
  toolRouterMode?: "llm_unified" | string;
  toolUseEnforcement?: "auto" | "off" | string;
  toolRouterLlmEnabled?: boolean;
  toolRouterLlmMinConfidence?: number;
  toolRouterLlmTimeoutSeconds?: number;
  toolRouterLlmMaxTokens?: number;
  toolRouterLlmPrompt?: string;
  toolParallelEnabled?: boolean;
  toolParallelLimit?: number;
  sendMessageToolEnabled?: boolean;
  toolApprovalMode?: "risky" | "smart" | "always" | "never" | string;
  cronApprovalMode?: "deny" | "approve" | string;
  trustedToolPatterns?: string[];
  trustedCommandPatterns?: string[];
  hooks?: Record<string, Array<Record<string, unknown>>>;
  hooksAutoAccept?: boolean;
  llmCredentialPoolStrategy?: "fill_first" | "round_robin" | "random" | "least_used" | string;
  toolEnvPassthrough?: string[];
  toolMutationCheckpointEnabled?: boolean;
  llmRetryCount?: number;
  llmRetryBackoffMs?: number;
  responsesReasoningReplayEnabled?: boolean;
  fastModeEnabled?: boolean;
  runtimeFooterEnabled?: boolean;
  statusbarEnabled?: boolean;
  toolProgressDisplay?: "off" | "new" | "all" | "verbose" | string;
  displaySkin?: string;
  busyIndicatorStyle?: "kaomoji" | "emoji" | "unicode" | "ascii" | string;
  codexRuntime?: "auto" | "codex_app_server" | string;
  toolCallRetryCount?: number;
  toolCallRetryBackoffMs?: number;
  toolResultPersistThresholdChars?: number;
  toolResultPreviewChars?: number;
  toolObservationTurnBudgetChars?: number;
  toolObservationTailBudgetChars?: number;
  toolOutputMaxBytes?: number;
  toolOutputMaxLines?: number;
  toolOutputMaxLineLength?: number;
  toolGuardrailWarningsEnabled?: boolean;
  toolGuardrailHardStopEnabled?: boolean;
  toolGuardrailExactFailureWarnAfter?: number;
  toolGuardrailSameToolFailureWarnAfter?: number;
  toolGuardrailNoProgressWarnAfter?: number;
  toolGuardrailExactFailureLimit?: number;
  toolGuardrailSameToolFailureLimit?: number;
  toolGuardrailNoProgressLimit?: number;
  backgroundMemoryReviewEnabled?: boolean;
  backgroundMemoryReviewMinMessages?: number;
  backgroundSkillReviewEnabled?: boolean;
  backgroundSkillReviewAutoCreateEnabled?: boolean;
  backgroundSkillCuratorEnabled?: boolean;
  backgroundSkillCuratorIntervalHours?: number;
  skillHotReloadEnabled?: boolean;
  skillHotReloadIntervalSeconds?: number;
  historyCleanupEnabled?: boolean;
  historyRetentionDays?: number;
  maxStoredMessagesPerConversation?: number;
  maxStoredAgentRuns?: number;
  maxStoredToolTraces?: number;
}

export interface VideoSummaryConfig {
  enabled: boolean;
  modelsDir: string;
  transcriber: string;
  ytDlpCommand: string;
  cookie: string;
  cookieFile: string;
  ffmpegBinPath: string;
  fasterWhisperModel: string;
  fasterWhisperModelDir: string;
  fasterWhisperDevice: string;
  fasterWhisperComputeType: string;
  senseVoiceModelDir: string;
  senseVoiceDevice: string;
  timeoutSeconds: number;
  ytdlpInfoTimeoutSeconds: number;
  downloadTimeoutSeconds: number;
  outputDir: string;
}

export interface ReplyConfig {
  typingDelayEnabled?: boolean;
  typingSpeed: number;
  typingSpeedRandomMin: number;
  typingSpeedRandomMax: number;
  splitByNewline: boolean;
  showTypingIndicator: boolean;
  typingIndicatorRefreshSeconds?: number;
}

export interface MomentsConfig {
  publishers: string[];
  repliers: string[];
  autoReplyEnabled: boolean;
}

export interface PlatformRuntimeConfig {
  enabled?: boolean;
  autoStart?: boolean;
  timeoutSeconds?: number;
  [key: string]: unknown;
}

export interface AppConfig {
  logLevel: "debug" | "info" | "warning" | "error";
  chat: ChatConfig;
  reply: ReplyConfig;
  web: WebConfig;
  weather: WeatherConfig;
  homeassistant?: PlatformRuntimeConfig;
  feishu?: PlatformRuntimeConfig;
  yuanbao?: PlatformRuntimeConfig;
  telegram?: PlatformRuntimeConfig;
  slack?: PlatformRuntimeConfig;
  mattermost?: PlatformRuntimeConfig;
  matrix?: PlatformRuntimeConfig;
  signal?: PlatformRuntimeConfig;
  email?: PlatformRuntimeConfig;
  sms?: PlatformRuntimeConfig;
  dingtalk?: PlatformRuntimeConfig;
  whatsapp?: PlatformRuntimeConfig;
  qqbot?: PlatformRuntimeConfig;
  bluebubbles?: PlatformRuntimeConfig;
  messagingGateway?: PlatformRuntimeConfig;
  spotify?: PlatformRuntimeConfig;
  webhook?: PlatformRuntimeConfig;
  discord?: PlatformRuntimeConfig;
  moments: MomentsConfig;
  videoSummary: VideoSummaryConfig;
  telemetryEnabled: boolean;
}

export interface HermesCredentialPoolEntryStatus {
  providerId: string;
  index: number;
  id?: string;
  label: string;
  authType?: string;
  source?: string;
  state: string;
  expiresAt?: string;
  baseUrl?: string;
}

export interface AddHermesCredentialPoolEntryRequest {
  provider: string;
  label?: string;
  apiKey: string;
  baseUrl?: string;
  authType?: string;
  expiresAt?: string;
}

export interface StateSnapshotManifest {
  id: string;
  label?: string;
  createdAt?: string;
  statePath?: string;
}

export interface StateSnapshotRestoreResult {
  restored: StateSnapshotManifest;
  preRestore: StateSnapshotManifest;
}

export interface WorkspaceSnapshotManifest {
  id: string;
  label?: string;
  createdAt?: string;
  root?: string;
  snapshotPath?: string;
  filesPath?: string;
  fileCount?: number;
  totalBytes?: number;
  skippedFiles?: number;
  skippedDirs?: number;
  truncated?: boolean;
}

export interface WorkspaceSnapshotRestoreResult {
  restored: WorkspaceSnapshotManifest;
  preRestore: WorkspaceSnapshotManifest;
  restoredFiles?: number;
  removedNewFiles?: number;
  deleteNewFiles?: boolean;
  note?: string;
}

export interface Conversation {
  id: string;
  title: string;
  updatedAt: string;
  lastMessage: string;
  personaId?: string;
  wechatAccountId?: string | null;
  agentId?: string;
}

export interface ChatMessage {
  id: string;
  conversationId: string;
  role: "user" | "assistant" | "system" | "tool";
  content: string;
  createdAt: string;
  source?: string;
  accountId?: string | null;
  providerData?: unknown | null;
}

export interface ChatAttachment {
  id: string;
  fileName: string;
  mimeType: string;
  fileSize: number;
  path: string;
}

export interface ToolEvent {
  status?: "running" | "completed" | "failed" | "canceled" | string;
  referenceId?: string;
  callId?: string;
  runId?: string;
  checkpointId?: string | null;
  dependsOn?: string[];
  mutexGroup?: string;
  eventType: string;
  serverId: string;
  toolName: string;
  ok: boolean;
  timedOut: boolean;
  elapsedMs: number;
  kind?: "read" | "edit" | "execute" | "search" | "fetch" | "think" | "other" | string;
  title: string;
  summary: string;
  path?: string | null;
  exists?: boolean | null;
  mimeType?: string | null;
  text?: string | null;
  error?: string | null;
  raw?: unknown;
}

export interface ToolEventEnvelope {
  type: "toolEvent";
  reference?: {
    referenceId?: string;
    callId?: string;
    tool?: string;
    ok?: boolean;
    path?: string | null;
    checkpointId?: string | null;
  };
  modelSummary?: string;
  event: ToolEvent;
}

export type WorkflowNodeName =
  | "queue"
  | "group_room"
  | "planner"
  | "executor"
  | "approval"
  | "checkpoint"
  | "reviewer"
  | (string & {});

export type WorkflowNodeStatus =
  | "pending"
  | "running"
  | "completed"
  | "waiting"
  | "failed"
  | "canceled"
  | "skipped"
  | (string & {});

export const WORKFLOW_REASON_QUEUED_TURN = "queued_turn" as const;
export const WORKFLOW_REASON_DIRECT_TURN = "direct_turn" as const;
export const WORKFLOW_REASON_GROUP_CONTEXT_READY = "group_context_ready" as const;
export const WORKFLOW_REASON_NO_GROUP_ROOM_CONTEXT = "no_group_room_context" as const;
export const WORKFLOW_REASON_TOOL_CALLS = "tool_calls" as const;
export const WORKFLOW_REASON_TOOL_OBSERVATIONS_RECORDED = "tool_observations_recorded" as const;
export const WORKFLOW_REASON_APPROVAL_REQUIRED = "approval_required" as const;
export const WORKFLOW_REASON_APPROVAL_RESUMED = "approval_resumed" as const;
export const WORKFLOW_REASON_CLARIFY_REQUIRES_USER_INPUT = "clarify_requires_user_input" as const;
export const WORKFLOW_REASON_FUTURE_CHECKPOINT_WAIT = "future_checkpoint_wait" as const;
export const WORKFLOW_REASON_RESUME_CHECKPOINT_REQUESTED = "resume_checkpoint_requested" as const;
export const WORKFLOW_REASON_RESUME_CHECKPOINT_CONTINUED = "resume_checkpoint_continued" as const;
export const WORKFLOW_REASON_FINAL_ANSWER_CANDIDATE = "final_answer_candidate" as const;
export const WORKFLOW_REASON_DELEGATE_TASK_STARTED = "delegate_task_started" as const;
export const WORKFLOW_REASON_DELEGATE_TASK_COMPLETED = "delegate_task_completed" as const;
export const WORKFLOW_REASON_DELEGATE_TASK_FAILED = "delegate_task_failed" as const;

export type WorkflowTransitionReason =
  | typeof WORKFLOW_REASON_QUEUED_TURN
  | typeof WORKFLOW_REASON_DIRECT_TURN
  | typeof WORKFLOW_REASON_GROUP_CONTEXT_READY
  | typeof WORKFLOW_REASON_NO_GROUP_ROOM_CONTEXT
  | typeof WORKFLOW_REASON_TOOL_CALLS
  | typeof WORKFLOW_REASON_TOOL_OBSERVATIONS_RECORDED
  | typeof WORKFLOW_REASON_APPROVAL_REQUIRED
  | typeof WORKFLOW_REASON_APPROVAL_RESUMED
  | typeof WORKFLOW_REASON_CLARIFY_REQUIRES_USER_INPUT
  | typeof WORKFLOW_REASON_FUTURE_CHECKPOINT_WAIT
  | typeof WORKFLOW_REASON_RESUME_CHECKPOINT_REQUESTED
  | typeof WORKFLOW_REASON_RESUME_CHECKPOINT_CONTINUED
  | typeof WORKFLOW_REASON_FINAL_ANSWER_CANDIDATE
  | typeof WORKFLOW_REASON_DELEGATE_TASK_STARTED
  | typeof WORKFLOW_REASON_DELEGATE_TASK_COMPLETED
  | typeof WORKFLOW_REASON_DELEGATE_TASK_FAILED
  | (string & {});

export const WORKFLOW_TRANSITION_REASON_ORDER: readonly WorkflowTransitionReason[] = [
  WORKFLOW_REASON_QUEUED_TURN,
  WORKFLOW_REASON_DIRECT_TURN,
  WORKFLOW_REASON_GROUP_CONTEXT_READY,
  WORKFLOW_REASON_NO_GROUP_ROOM_CONTEXT,
  WORKFLOW_REASON_TOOL_CALLS,
  WORKFLOW_REASON_TOOL_OBSERVATIONS_RECORDED,
  WORKFLOW_REASON_APPROVAL_REQUIRED,
  WORKFLOW_REASON_APPROVAL_RESUMED,
  WORKFLOW_REASON_CLARIFY_REQUIRES_USER_INPUT,
  WORKFLOW_REASON_FUTURE_CHECKPOINT_WAIT,
  WORKFLOW_REASON_RESUME_CHECKPOINT_REQUESTED,
  WORKFLOW_REASON_RESUME_CHECKPOINT_CONTINUED,
  WORKFLOW_REASON_FINAL_ANSWER_CANDIDATE,
  WORKFLOW_REASON_DELEGATE_TASK_STARTED,
  WORKFLOW_REASON_DELEGATE_TASK_COMPLETED,
  WORKFLOW_REASON_DELEGATE_TASK_FAILED
];

export const WORKFLOW_GRAPH_SCHEMA = "synthgraph_workflow_v1" as const;
export const WORKFLOW_RUNTIME_EVENTS_SCHEMA = "synthgraph_workflow_runtime_events_v1" as const;
export const TOOL_CALL_PROTOCOL_SCHEMA = "synthgraph_tool_call_protocol_v1" as const;
export const WORKFLOW_RUNTIME_SOURCE = "agent_run.workflow_graph" as const;
export const WORKFLOW_PHASE_INITIALIZED = "workflow_graph_initialized" as const;
export const WORKFLOW_PHASE_NODE = "workflow_node" as const;
export const WORKFLOW_PHASE_TRANSITION = "workflow_transition" as const;
export const WORKFLOW_RUNTIME_KIND_PREFIX = "workflow_" as const;
export const WORKFLOW_RUNTIME_KIND_SNAPSHOT = "workflow_snapshot" as const;
export const WORKFLOW_RUNTIME_KIND_TRANSITION = WORKFLOW_PHASE_TRANSITION;
export const WORKFLOW_RUNTIME_NODE_KIND_PREFIX = "workflow_node_" as const;
export const WORKFLOW_NODE_ORDER: readonly string[] = ["queue", "group_room", "planner", "executor", "approval", "checkpoint", "reviewer"];
export const WORKFLOW_STATUS_ORDER: readonly string[] = ["failed", "canceled", "waiting", "running", "pending", "completed", "skipped"];
export const WORKFLOW_STATUS_LABELS: Record<string, string> = {
  pending: "pending",
  running: "running",
  completed: "completed",
  waiting: "waiting",
  failed: "failed",
  canceled: "canceled",
  skipped: "skipped"
};
export const WORKFLOW_NODE_ROLE_LABELS: Record<string, string> = {
  queue: "queue admission",
  group_room: "group context",
  planner: "decision planning",
  executor: "tool execution",
  approval: "human approval gate",
  checkpoint: "state checkpoint",
  reviewer: "final review"
};

export function workflowNodeRoleLabel(node?: string | null): string {
  if (!node) return "";
  return WORKFLOW_NODE_ROLE_LABELS[node] ?? "custom workflow node";
}

export function workflowNodeDisplayLabel(node?: string | null): string {
  return node ? node.replace(/_/g, " ") : "-";
}

export function workflowStatusDisplayLabel(status?: string | null): string {
  if (!status) return "";
  return WORKFLOW_STATUS_LABELS[status] ?? status.replace(/_/g, " ");
}

export const WORKFLOW_TRANSITION_REASON_LABELS: Record<string, string> = {
  queued_turn: "queued turn",
  direct_turn: "direct turn",
  group_context_ready: "group context ready",
  no_group_room_context: "no group context",
  tool_calls: "tool calls",
  tool_observations_recorded: "tool observations recorded",
  approval_required: "approval required",
  approval_resumed: "approval resumed",
  clarify_requires_user_input: "clarify requires user input",
  future_checkpoint_wait: "future checkpoint wait",
  resume_checkpoint_requested: "resume checkpoint requested",
  resume_checkpoint_continued: "resume checkpoint continued",
  final_answer_candidate: "final answer candidate",
  delegate_task_started: "delegate task started",
  delegate_task_completed: "delegate task completed",
  delegate_task_failed: "delegate task failed"
};

export function workflowTransitionReasonLabel(reason?: string | null): string {
  if (!reason) return "transition";
  return WORKFLOW_TRANSITION_REASON_LABELS[reason] ?? reason.replace(/_/g, " ");
}

export interface WorkflowGraphNode {
  node: WorkflowNodeName;
  role?: string | null;
  status: WorkflowNodeStatus;
  detail?: unknown;
  eventSequence?: number | null;
  event_sequence?: number | null;
  updatedAt?: string | null;
  updated_at?: string | null;
}

export interface WorkflowGraphTransition {
  from?: WorkflowNodeName | null;
  to?: WorkflowNodeName | null;
  reason?: WorkflowTransitionReason | null;
  topologyEdgeKnown?: boolean | null;
  topology_edge_known?: boolean | null;
  topologyReasonKnown?: boolean | null;
  topology_reason_known?: boolean | null;
  topologyEdgeSource?: string | null;
  topology_edge_source?: string | null;
  topologyEdgeLabel?: string | null;
  topology_edge_label?: string | null;
  detail?: unknown;
  eventSequence?: number | null;
  event_sequence?: number | null;
  updatedAt?: string | null;
  updated_at?: string | null;
}

export interface WorkflowGraph {
  schema?: typeof WORKFLOW_GRAPH_SCHEMA | (string & {});
  mode?: "chat_turn" | "approval_continuation" | "recovered" | string;
  requestSource?: string;
  request_source?: string;
  toolContext?: string;
  tool_context?: string;
  currentNode?: WorkflowNodeName | null;
  current_node?: WorkflowNodeName | null;
  currentStatus?: WorkflowNodeStatus | null;
  current_status?: WorkflowNodeStatus | null;
  nodes?: WorkflowGraphNode[];
  transitions?: WorkflowGraphTransition[];
  lastEventSequence?: number | null;
  last_event_sequence?: number | null;
  updatedAt?: string | null;
  updated_at?: string | null;
}

export interface WorkflowDetailContract {
  nodeStatuses?: string[];
  entryPoints?: string[];
  transitionReasons?: WorkflowTransitionReason[];
  stableFields?: string[];
  phaseValues?: string[];
  childSummaryFields?: string[];
  resultSummaryFields?: string[];
  transitionDetailFields?: string[];
  [key: string]: unknown;
}

export interface WorkflowApiRunEventSurface {
  endpoint?: "/v1/runs/{run_id}/events" | string;
  streaming?: boolean;
  sse?: boolean;
  object?: "hermes.run.event" | string;
  types?: string[];
  envelopeFields?: string[];
  payloadField?: "data" | string;
  [key: string]: unknown;
}

export interface WorkflowDashboardRuntimeEventSurface {
  endpoint?: "/api/plugins/kanban/runtime-events" | string;
  sseEndpoint?: "/api/plugins/kanban/runtime-events/stream" | string;
  schema?: "hermes_kanban_runtime_events_desktop_v1" | string;
  source?: typeof WORKFLOW_RUNTIME_SOURCE | (string & {});
  kinds?: string[];
  envelopeFields?: string[];
  payloadField?: "payload" | string;
  [key: string]: unknown;
}

export interface WorkflowTauriRunEventSurface {
  event?: "synthchat-agent-run-event" | string;
  payloadField?: "workflowGraph" | string;
  payloadAliases?: string[];
  phaseDetailSequenceAliases?: string[];
  mergeStrategy?: string;
  [key: string]: unknown;
}

export interface WorkflowRuntimeEventSurfaces {
  apiRunEvents?: WorkflowApiRunEventSurface;
  dashboardRuntimeEvents?: WorkflowDashboardRuntimeEventSurface;
  tauriRunEvent?: WorkflowTauriRunEventSurface;
  [key: string]: unknown;
}

export interface WorkflowTopologyEdgeContract {
  from?: WorkflowNodeName | string;
  to?: WorkflowNodeName | string;
  reasons?: WorkflowTransitionReason[];
  source?: string;
  [key: string]: unknown;
}

export interface WorkflowTopologyContract {
  entryNode?: WorkflowNodeName | string;
  bootstrapCurrentNode?: WorkflowNodeName | string;
  terminalPatterns?: string[];
  edges?: WorkflowTopologyEdgeContract[];
  purpose?: string;
  [key: string]: unknown;
}

export interface WorkflowStateMachineNodeDriverContract {
  accessor?: string;
  nodeType?: string;
  recorders?: string[];
  statusWrites?: string[];
  [key: string]: unknown;
}

export interface WorkflowStateMachineContract {
  driver?: string;
  modeSource?: string;
  layering?: Record<string, unknown>;
  nodeDrivers?: Record<string, WorkflowStateMachineNodeDriverContract>;
  statusSemantics?: Record<string, string>;
  terminalPolicy?: Record<string, unknown>;
  edgePolicy?: Record<string, unknown>;
  sourceBoundaries?: Record<string, string[]>;
  clientMergeContract?: string;
  purpose?: string;
  [key: string]: unknown;
}

export interface WorkflowGraphPayloadAliasGuarantee {
  appliesTo?: string[];
  rootAliases?: string[];
  nodeAliases?: string[];
  transitionAliases?: string[];
  detailAliases?: string[];
  purpose?: string;
  [key: string]: unknown;
}

export interface WorkflowClientMergeContract {
  frontendStore?: string;
  snapshotStrategy?: string;
  detailAliasNormalizer?: string;
  nodeUpdatePolicy?: string;
  transitionPolicy?: string;
  [key: string]: unknown;
}

export interface WorkflowGraphRuntimeContract {
  schema?: typeof WORKFLOW_RUNTIME_EVENTS_SCHEMA | (string & {});
  source?: typeof WORKFLOW_RUNTIME_SOURCE | (string & {});
  nodeOrder?: string[];
  statusOrder?: string[];
  transitionReasonOrder?: WorkflowTransitionReason[];
  nodeRoles?: Record<string, string>;
  topology?: WorkflowTopologyContract | null;
  stateMachine?: WorkflowStateMachineContract | null;
  state_machine?: WorkflowStateMachineContract | null;
  eventKinds?: string[];
  apiRunEventKinds?: string[];
  runtimeEventKindMap?: Record<string, string>;
  eventSurfaces?: WorkflowRuntimeEventSurfaces | null;
  graphRootFields?: string[];
  summaryFields?: string[];
  payloadBuilders?: Record<string, string>;
  runtimeContractAliasBuilder?: string;
  runtimeContractAliases?: string[];
  runResponseAliases?: Record<string, string>;
  snapshotPayload?: Record<string, unknown>;
  graphPayloadAliasGuarantee?: WorkflowGraphPayloadAliasGuarantee;
  clientMergeContract?: WorkflowClientMergeContract;
  client_merge_contract?: WorkflowClientMergeContract;
  nodePayload?: Record<string, unknown>;
  transitionPayload?: Record<string, unknown>;
  detailContracts?: Record<string, WorkflowDetailContract>;
  ordering?: string;
  purpose?: string;
}

export interface ToolCallArgumentNormalizationContract {
  providerHelper?: string;
  plannerHelper?: string;
  emptyString?: "{}" | string;
  noneLiteral?: "{}" | string;
  repairPolicy?: string;
  corruptionMarkerKey?: string;
  corruptionMarkerMessage?: string;
  [key: string]: unknown;
}

export interface ToolCallProviderAdapterBoundary {
  llmReplyContentBridge?: boolean;
  plannerEntryPoints?: string[];
  normalizedProviders?: string[];
  normalizedContentShape?: Record<string, unknown>;
  argumentNormalization?: ToolCallArgumentNormalizationContract;
  providerDataRole?: string;
  [key: string]: unknown;
}

export interface ToolCallProviderNativeInputShape {
  sourceKeys?: string[];
  normalizedMetadataKey?: string;
  callIdLookupKeys?: string[];
  metadataPolicy?: string;
  [key: string]: unknown;
}

export interface ToolCallHermesMarkupInputShape {
  shape?: string;
  decisionOriginMetadataKey?: string;
  originValue?: "hermes_markup" | string;
  [key: string]: unknown;
}

export interface ToolCallPlannerFieldAliases {
  actionKeys?: string[];
  toolActionValues?: string[];
  useToolKeys?: string[];
  singleCallNameKeys?: string[];
  singleCallArgumentKeys?: string[];
  multiCallArrayKeys?: string[];
  functionObjectKeys?: string[];
  [key: string]: unknown;
}

export interface ToolCallAcceptedInputShapes {
  plannerJson?: Record<string, unknown>[];
  fieldAliases?: ToolCallPlannerFieldAliases;
  providerNative?: ToolCallProviderNativeInputShape;
  hermesMarkup?: ToolCallHermesMarkupInputShape;
  [key: string]: unknown;
}

export interface ToolCallCanonicalizationPipelineStage {
  stage?: string;
  inputOrigins?: string[];
  entryPoints?: string[];
  output?: string;
  [key: string]: unknown;
}

export interface ToolCallValidationContract {
  plannerValidationEntry?: string;
  plannerCanonicalValidationEntry?: string;
  sharedSchemaValidator?: string;
  definitionResolution?: string;
  internalToolSchemaSource?: string;
  schemaCombinators?: string[];
  schemaCombinatorPolicy?: string;
  additionalPropertiesPolicy?: string;
  payloadNormalization?: string;
  errorKinds?: string[];
  metadataStripping?: string;
  [key: string]: unknown;
}

export interface ToolCallValidationPipelineStage {
  stage?: string;
  entryPoint?: string;
  policy?: string;
  errorKind?: string;
  [key: string]: unknown;
}

export interface ToolCallWorkflowGraphObservabilityContract {
  source?: typeof WORKFLOW_RUNTIME_SOURCE | (string & {});
  plannerDetailFields?: string[];
  executorDetailFields?: string[];
  transitionReason?: WorkflowTransitionReason | string;
  protocolValue?: string;
  summaryPolicy?: string;
  [key: string]: unknown;
}

export interface ToolCallApprovedReplayContract {
  trustedContext?: string;
  markerKey?: string;
  scope?: string;
  markerPolicy?: string;
  authorizationPolicy?: string;
  [key: string]: unknown;
}

export interface BridgeToolCallWorkflowGraphStageContract {
  node?: WorkflowNodeName | string;
  status?: WorkflowNodeStatus | string;
  stage?: string;
  detailFields?: string[];
  bridgeStatusValues?: string[];
  completionCarryForward?: string[];
  records?: string;
  [key: string]: unknown;
}

export interface BridgeToolCallContract {
  name?: "tool_call" | string;
  payloadShape?: Record<string, unknown>;
  targetAliases?: string[];
  argumentAliases?: string[];
  blockedTargets?: string[];
  targetValidation?: string;
  directExecutionValidation?: string;
  directContextBoundary?: string;
  directApprovalBoundary?: string;
  workflowGraphStage?: BridgeToolCallWorkflowGraphStageContract;
  approvedReplay?: ToolCallApprovedReplayContract;
  riskPolicy?: string;
  [key: string]: unknown;
}

export interface ToolCallExecutionBoundary {
  internalTools?: string;
  mcpTools?: string;
  approvals?: string;
  [key: string]: unknown;
}

export interface ToolCallProtocolContract {
  schema?: typeof TOOL_CALL_PROTOCOL_SCHEMA | (string & {});
  canonicalShape?: string;
  canonicalFields?: Record<string, unknown>;
  acceptedOrigins?: string[];
  canonicalizationPipeline?: ToolCallCanonicalizationPipelineStage[];
  providerAdapterBoundary?: ToolCallProviderAdapterBoundary;
  acceptedInputShapes?: ToolCallAcceptedInputShapes;
  validation?: ToolCallValidationContract;
  validationPipeline?: ToolCallValidationPipelineStage[];
  workflowGraphObservability?: ToolCallWorkflowGraphObservabilityContract;
  bridgeToolCall?: BridgeToolCallContract;
  executionBoundary?: ToolCallExecutionBoundary;
  purpose?: string;
}

export interface AgentRuntimeContracts {
  workflowGraph?: WorkflowGraphRuntimeContract | null;
  workflow_graph?: WorkflowGraphRuntimeContract | null;
  toolCallProtocol?: ToolCallProtocolContract | null;
  tool_call_protocol?: ToolCallProtocolContract | null;
}

export type AgentRuntimeContractsCarrier = {
  workflowGraphRuntimeContract?: WorkflowGraphRuntimeContract | null;
  workflow_graph_runtime_contract?: WorkflowGraphRuntimeContract | null;
  toolCallProtocolContract?: ToolCallProtocolContract | null;
  tool_call_protocol_contract?: ToolCallProtocolContract | null;
  agentRuntimeContracts?: AgentRuntimeContracts | null;
  agent_runtime_contracts?: AgentRuntimeContracts | null;
  runtimeContracts?: AgentRuntimeContracts | null;
  runtime_contracts?: AgentRuntimeContracts | null;
};

function agentRuntimeContractsPresent(
  contracts?: AgentRuntimeContracts | null
): contracts is AgentRuntimeContracts {
  return Boolean(
    contracts?.workflowGraph
      ?? contracts?.workflow_graph
      ?? contracts?.toolCallProtocol
      ?? contracts?.tool_call_protocol
  );
}

export function agentRuntimeContractsValue(
  source?: AgentRuntimeContractsCarrier | null
): AgentRuntimeContracts | null {
  return [
    source?.agentRuntimeContracts,
    source?.agent_runtime_contracts,
    source?.runtimeContracts,
    source?.runtime_contracts
  ].find(agentRuntimeContractsPresent) ?? null;
}

export function workflowGraphRuntimeContractValue(
  source?: AgentRuntimeContractsCarrier | null
): WorkflowGraphRuntimeContract | null {
  const contracts = agentRuntimeContractsValue(source);
  return source?.workflowGraphRuntimeContract
    ?? source?.workflow_graph_runtime_contract
    ?? contracts?.workflowGraph
    ?? contracts?.workflow_graph
    ?? null;
}

export function toolCallProtocolContractValue(
  source?: AgentRuntimeContractsCarrier | null
): ToolCallProtocolContract | null {
  const contracts = agentRuntimeContractsValue(source);
  return source?.toolCallProtocolContract
    ?? source?.tool_call_protocol_contract
    ?? contracts?.toolCallProtocol
    ?? contracts?.tool_call_protocol
    ?? null;
}

export interface WorkflowRuntimeSummary {
  schema?: typeof WORKFLOW_GRAPH_SCHEMA | (string & {});
  mode?: WorkflowGraph["mode"] | string | null;
  requestSource?: string | null;
  request_source?: string | null;
  toolContext?: string | null;
  tool_context?: string | null;
  currentNode?: WorkflowNodeName | null;
  current_node?: WorkflowNodeName | null;
  currentStatus?: WorkflowNodeStatus | null;
  current_status?: WorkflowNodeStatus | null;
  lastEventSequence?: number | null;
  last_event_sequence?: number | null;
  updatedAt?: string | null;
  updated_at?: string | null;
  nodeCount?: number;
  node_count?: number;
  transitionCount?: number;
  transition_count?: number;
  statusCounts?: Record<string, number>;
  status_counts?: Record<string, number>;
  toolOrigins?: string[];
  tool_origins?: string[];
}

export interface WorkflowSnapshotRuntimePayload {
  summary?: WorkflowRuntimeSummary | null;
  workflowSummary?: WorkflowRuntimeSummary | null;
  workflow_summary?: WorkflowRuntimeSummary | null;
  graph?: WorkflowGraph | null;
  workflowGraph?: WorkflowGraph | null;
  workflow_graph?: WorkflowGraph | null;
}

export interface WorkflowNodeRuntimePayload {
  node?: WorkflowNodeName | null;
  role?: string | null;
  status?: WorkflowNodeStatus | null;
  detail?: unknown;
  eventSequence?: number | null;
  event_sequence?: number | null;
  graphSummary?: WorkflowRuntimeSummary | null;
  graph_summary?: WorkflowRuntimeSummary | null;
}

export interface WorkflowTransitionRuntimePayload {
  from?: WorkflowNodeName | null;
  to?: WorkflowNodeName | null;
  reason?: WorkflowTransitionReason | null;
  topologyEdgeKnown?: boolean | null;
  topology_edge_known?: boolean | null;
  topologyReasonKnown?: boolean | null;
  topology_reason_known?: boolean | null;
  topologyEdgeSource?: string | null;
  topology_edge_source?: string | null;
  topologyEdgeLabel?: string | null;
  topology_edge_label?: string | null;
  detail?: unknown;
  eventSequence?: number | null;
  event_sequence?: number | null;
  graphSummary?: WorkflowRuntimeSummary | null;
  graph_summary?: WorkflowRuntimeSummary | null;
}

export type WorkflowRuntimeEventKind =
  | typeof WORKFLOW_RUNTIME_KIND_SNAPSHOT
  | typeof WORKFLOW_RUNTIME_KIND_TRANSITION
  | `${typeof WORKFLOW_RUNTIME_NODE_KIND_PREFIX}${WorkflowNodeStatus}`;

export type AgentRuntimeEventKind = WorkflowRuntimeEventKind | (string & {});

export interface AgentRunRecord {
  runId: string;
  conversationId: string;
  personaId: string;
  agentId: string;
  parentRunId?: string | null;
  subagentIndex?: number | null;
  subagentDepth?: number | null;
  subagentCanDelegate?: boolean | null;
  subagentRole?: string | null;
  subagentTask?: string | null;
  subagentToolsets?: string[];
  subagentMaxIterations?: number | null;
  queueItemId?: string | null;
  userRequest?: string;
  state: string;
  startedAt: string;
  updatedAt: string;
  lastActivityAt?: string | null;
  lastActivityDesc?: string | null;
  completedAt?: string | null;
  error?: string | null;
  toolEvents: ToolEvent[];
  phaseEvents?: AgentRunPhase[];
  checkpoints?: AgentCheckpointRecord[];
  pendingSteers?: string[];
  workflowGraph?: WorkflowGraph | null;
  workflow_graph?: WorkflowGraph | null;
}

export type AgentRunWorkflowGraphCarrier = {
  workflowGraph?: WorkflowGraph | null;
  workflow_graph?: WorkflowGraph | null;
};

export function agentRunWorkflowGraph(run?: AgentRunWorkflowGraphCarrier | null): WorkflowGraph | null {
  return run?.workflowGraph ?? run?.workflow_graph ?? null;
}

export function workflowGraphCurrentNodeValue(graph?: WorkflowGraph | null): WorkflowNodeName | null {
  return graph?.currentNode
    ?? graph?.current_node
    ?? graph?.nodes?.find((node) => node.status === "running")?.node
    ?? null;
}

export function workflowGraphCurrentStatusValue(
  graph?: WorkflowGraph | null,
  currentNode: WorkflowNodeName | null = workflowGraphCurrentNodeValue(graph)
): WorkflowNodeStatus | null {
  return graph?.currentStatus
    ?? graph?.current_status
    ?? graph?.nodes?.find((node) => node.node === currentNode)?.status
    ?? null;
}

export function workflowGraphRequestSourceValue(graph?: WorkflowGraph | null): string | null {
  return graph?.requestSource ?? graph?.request_source ?? null;
}

export function workflowGraphToolContextValue(graph?: WorkflowGraph | null): string | null {
  return graph?.toolContext ?? graph?.tool_context ?? null;
}

export function workflowGraphLastEventSequenceValue(graph?: WorkflowGraph | null): number | null {
  return graph?.lastEventSequence ?? graph?.last_event_sequence ?? null;
}

export function workflowGraphUpdatedAtValue(graph?: WorkflowGraph | null): string | null {
  return graph?.updatedAt ?? graph?.updated_at ?? null;
}

export function workflowTransitionSequenceValue(transition: WorkflowGraphTransition): number | null {
  return transition.eventSequence ?? transition.event_sequence ?? null;
}

export function workflowTransitionUpdatedAtValue(transition: WorkflowGraphTransition): string | null {
  return transition.updatedAt ?? transition.updated_at ?? null;
}

export function workflowRuntimeSummaryCurrentNodeValue(
  summary?: WorkflowRuntimeSummary | null
): WorkflowNodeName | null {
  return summary?.currentNode ?? summary?.current_node ?? null;
}

export function workflowRuntimeSummaryCurrentStatusValue(
  summary?: WorkflowRuntimeSummary | null
): WorkflowNodeStatus | null {
  return summary?.currentStatus ?? summary?.current_status ?? null;
}

export function workflowRuntimeSummaryRequestSourceValue(summary?: WorkflowRuntimeSummary | null): string | null {
  return summary?.requestSource ?? summary?.request_source ?? null;
}

export function workflowRuntimeSummaryToolContextValue(summary?: WorkflowRuntimeSummary | null): string | null {
  return summary?.toolContext ?? summary?.tool_context ?? null;
}

export function workflowRuntimeSummaryToolOriginsValue(summary?: WorkflowRuntimeSummary | null): string[] {
  return summary?.toolOrigins ?? summary?.tool_origins ?? [];
}

export function workflowRuntimeSummaryNodeCountValue(summary?: WorkflowRuntimeSummary | null): number | null {
  return summary?.nodeCount ?? summary?.node_count ?? null;
}

export function workflowRuntimeSummaryTransitionCountValue(summary?: WorkflowRuntimeSummary | null): number | null {
  return summary?.transitionCount ?? summary?.transition_count ?? null;
}

export function workflowRuntimePayloadEventSequenceValue(
  payload?: WorkflowNodeRuntimePayload | WorkflowTransitionRuntimePayload | null
): number | null {
  return payload?.eventSequence ?? payload?.event_sequence ?? null;
}

function workflowRecordValue(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function isWorkflowRuntimeSummaryValue(value: unknown): value is WorkflowRuntimeSummary {
  const record = workflowRecordValue(value);
  if (!record) return false;
  return Boolean(
    record.currentNode
      ?? record.current_node
      ?? record.currentStatus
      ?? record.current_status
      ?? record.requestSource
      ?? record.request_source
      ?? record.toolContext
      ?? record.tool_context
      ?? record.toolOrigins
      ?? record.tool_origins
      ?? record.nodeCount
      ?? record.node_count
      ?? record.transitionCount
      ?? record.transition_count
      ?? record.statusCounts
      ?? record.status_counts
  );
}

function isWorkflowGraphValue(value: unknown): value is WorkflowGraph {
  const record = workflowRecordValue(value);
  if (!record) return false;
  return record.schema === WORKFLOW_GRAPH_SCHEMA
    || Array.isArray(record.nodes)
    || Array.isArray(record.transitions)
    || Boolean(record.currentNode ?? record.current_node);
}

export function workflowSnapshotRuntimeSummaryValue(
  payload?: WorkflowSnapshotRuntimePayload | null
): WorkflowRuntimeSummary | null {
  const summary = payload?.summary ?? payload?.workflowSummary ?? payload?.workflow_summary;
  if (summary) return summary;
  return isWorkflowRuntimeSummaryValue(payload) ? payload : null;
}

export function workflowSnapshotRuntimeGraphValue(
  payload?: WorkflowSnapshotRuntimePayload | null
): WorkflowGraph | null {
  const graph = payload?.graph ?? payload?.workflowGraph ?? payload?.workflow_graph;
  if (graph) return graph;
  return isWorkflowGraphValue(payload) ? payload : null;
}

export interface ToolArtifactRecord {
  runId: string;
  fileName: string;
  path: string;
  sizeBytes: number;
  modifiedAt?: string | null;
  contentPreview?: string | null;
}

export interface AgentCheckpointRecord {
  checkpointId: string;
  runId: string;
  iteration: number;
  createdAt: string;
  state: string;
  completedCallIds?: string[];
  eventRefs?: string[];
  summary: string;
}

export interface AgentRunEvent {
  runId: string;
  conversationId: string;
  personaId: string;
  agentId: string;
  parentRunId?: string | null;
  subagentIndex?: number | null;
  subagentDepth?: number | null;
  subagentCanDelegate?: boolean | null;
  subagentRole?: string | null;
  subagentTask?: string | null;
  subagentToolsets?: string[];
  subagentMaxIterations?: number | null;
  queueItemId?: string | null;
  state: string;
  message?: ChatMessage | null;
  toolEvent?: ToolEvent | null;
  accumulatedToolEvents?: ToolEvent[];
  accumulatedPhases?: AgentRunPhase[];
  phase?: string | null;
  detail?: unknown | null;
  workflowGraph?: WorkflowGraph | null;
  workflow_graph?: WorkflowGraph | null;
  error?: string | null;
  updatedAt: string;
  lastActivityAt?: string | null;
  lastActivityDesc?: string | null;
}

export interface AgentRunPhase {
  phase: string;
  detail?: unknown | null;
  updatedAt: string;
}

export interface ManagedProcessEvent {
  type: "completed" | "stopped" | "watch_match" | "watch_disabled" | string;
  processId: string;
  label?: string | null;
  command?: string | null;
  cwd?: string | null;
  pid?: number | null;
  conversationId?: string | null;
  runId?: string | null;
  detail?: Record<string, unknown> | null;
  createdAt: string;
}

export interface ManagedProcessSnapshot {
  id: string;
  sessionId?: string;
  session_id?: string;
  label?: string | null;
  command?: string | null;
  cwd?: string | null;
  pid?: number | null;
  backend?: string | null;
  envType?: string | null;
  env_type?: string | null;
  status?: "running" | "exited" | "stopped" | string;
  conversationId?: string | null;
  conversation_id?: string | null;
  runId?: string | null;
  run_id?: string | null;
  startedAt?: string | null;
  started_at?: string | null;
  finishedAt?: string | null;
  finished_at?: string | null;
  exitCode?: number | null;
  exit_code?: number | null;
  notifyOnComplete?: boolean;
  notify_on_complete?: boolean;
  watchPatterns?: string[];
  watch_patterns?: string[];
  watchStats?: Record<string, unknown>;
  watch_stats?: Record<string, unknown>;
  stdoutTail?: string[];
  stdout_tail?: string[];
  stderrTail?: string[];
  stderr_tail?: string[];
  [key: string]: unknown;
}

export interface PlannerTraceRecord {
  id: string;
  runId: string;
  conversationId: string;
  personaId: string;
  agentId: string;
  iteration: number;
  createdAt: string;
  input: string;
  output: string;
  parsedStep: string;
  error?: string | null;
}

export interface ToolRouterTraceRecord {
  id: string;
  createdAt: string;
  conversationId: string;
  personaId: string;
  semanticIntent: string;
  userRequest: string;
  prompt: string;
  output: string;
  decision?: unknown | null;
  status: string;
  error?: string | null;
}

export interface AgentQueuedRequest {
  id: string;
  conversationId: string;
  personaId: string;
  userMessageId: string;
  content: string;
  status: "pending" | "running" | "completed" | "failed" | "canceled" | string;
  createdAt: string;
  updatedAt: string;
  startedAt?: string | null;
  completedAt?: string | null;
  error?: string | null;
}

export interface AgentRuntimeEvent {
  id: number;
  kind: AgentRuntimeEventKind;
  source: string;
  status?: string | null;
  conversationId?: string | null;
  conversation_id?: string | null;
  runId?: string | null;
  run_id?: string | null;
  queueItemId?: string | null;
  queue_item_id?: string | null;
  taskId?: string | null;
  task_id?: string | null;
  processId?: string | null;
  process_id?: string | null;
  payload?: unknown;
  createdAt?: string | null;
  created_at?: string | null;
}

export interface AgentRuntimeEventStream {
  schema: "hermes_kanban_runtime_events_desktop_v1" | string;
  status: string;
  action: string;
  events: AgentRuntimeEvent[];
  cursor: number;
  count: number;
  total: number;
  since: number;
  limit: number;
  pollIntervalMs?: number;
  websocketEmbedded?: boolean;
  nativeRuntimeEventBridge?: boolean;
  sources?: string[];
  workflowGraphRuntimeContract?: WorkflowGraphRuntimeContract | null;
  workflow_graph_runtime_contract?: WorkflowGraphRuntimeContract | null;
  toolCallProtocolContract?: ToolCallProtocolContract | null;
  tool_call_protocol_contract?: ToolCallProtocolContract | null;
  agentRuntimeContracts?: AgentRuntimeContracts | null;
  agent_runtime_contracts?: AgentRuntimeContracts | null;
  runtimeContracts?: AgentRuntimeContracts | null;
  runtime_contracts?: AgentRuntimeContracts | null;
}

export interface KanbanDispatchDrainResult {
  schema: "hermes_kanban_dispatch_drain_desktop_v1" | string;
  status: string;
  action: "kanban-dispatch-drain" | string;
  dispatch: Record<string, unknown>;
  drainRequested: boolean;
  drain_requested?: boolean;
  drained: AgentQueuedRequest[];
  drainedCount: number;
  drained_count?: number;
  nativeDispatcherDrainBridge?: boolean;
  boundary?: string;
}

export interface AgentControlCommand {
  name: string;
  aliases: string[];
  argsHint: string;
  category: string;
  description: string;
}

export interface ScheduledAgentJob {
  id: string;
  name: string;
  conversationId?: string | null;
  personaId: string;
  prompt: string;
  scheduleKind: "once" | "interval" | "cron" | string;
  intervalMinutes?: number | null;
  cronExpr?: string | null;
  runAt?: string | null;
  enabledToolsets: string[];
  disabledToolsets: string[];
  enabled: boolean;
  status: "scheduled" | "paused" | "completed" | "failed" | string;
  nextRunAt?: string | null;
  lastRunAt?: string | null;
  lastCompletedAt?: string | null;
  lastRunStatus?: string | null;
  lastOutput?: string | null;
  lastOutputPath?: string | null;
  lastError?: string | null;
  runCount: number;
  createdAt: string;
  updatedAt: string;
}

export interface ScheduledJobOutputRecord {
  fileName: string;
  path: string;
  modifiedAt: string;
  sizeBytes: number;
  status: string;
}

export interface AgentTodoItem {
  id: string;
  runId: string;
  conversationId: string;
  content: string;
  status: "pending" | "in_progress" | "completed" | "blocked" | string;
  createdAt: string;
  updatedAt: string;
}

export interface ToolTraceEntry {
  id: string;
  createdAt: string;
  serverId: string;
  toolName: string;
  ok: boolean;
  timedOut: boolean;
  elapsedMs: number;
  payload: unknown;
  event: ToolEvent;
  error?: string | null;
}

export interface ToolDefinition {
  name: string;
  displayName: string;
  description: string;
  source: string;
  serverId: string;
  toolName: string;
  inputSchema: unknown;
  requiresApproval: boolean;
}

export interface ToolApprovalRequest {
  id: string;
  createdAt: string;
  updatedAt: string;
  status: "pending" | "approved" | "completed" | "failed" | "denied" | string;
  conversationId?: string | null;
  personaId?: string | null;
  agentId?: string | null;
  runId?: string | null;
  serverId: string;
  toolName: string;
  payload: unknown;
  reason: string;
  result?: unknown | null;
  error?: string | null;
}

export interface SendChatRequest {
  conversationId?: string | null;
  personaId?: string | null;
  agentId?: string | null;
  content: string;
  providerData?: unknown | null;
}

export interface LlmProvider {
  id: string;
  name: string;
  providerType?: "openai_compatible" | "openai_responses" | "anthropic" | "gemini" | string;
  preset?: string;
  baseUrl: string;
  appendChatPath?: boolean;
  apiKeyEnv: string;
  apiKey?: string | null;
  model: string;
  enabled: boolean;
  timeoutSeconds: number;
  promptCacheMode?: "auto" | "on" | "off" | string;
  promptCacheTtl?: "5m" | "1h" | string;
  promptCacheLayout?: "auto" | "native" | "envelope" | string;
  models?: Record<string, Record<string, unknown>>;
}

export interface ModelCapabilities {
  provider_id?: string;
  model_id?: string;
  models_dev_provider_id?: string;
  supports_tools?: boolean;
  supports_vision?: boolean;
  supports_reasoning?: boolean;
  supports_pdf?: boolean;
  supports_audio_input?: boolean;
  supports_structured_output?: boolean;
  open_weights?: boolean;
  input_modalities?: string[];
  output_modalities?: string[];
  context_window?: number | null;
  max_output_tokens?: number | null;
  model_family?: string;
  status?: string;
  knowledge_cutoff?: string;
  source?: string;
}

export interface ProfileConfig {
  name: string;
  avatarPath?: string | null;
}

export interface AccountConfig {
  id: string;
  note: string;
  linkedPersona: string;
  online: boolean;
  createdAt: string;
  botToken?: string;
  ilinkUserId?: string;
  getUpdatesBuf?: string;
  loginBaseUrl?: string;
  lastLoginAt?: string;
  lastWechatUserId?: string;
  lastContextToken?: string;
  lastInboundAt?: string;
  rawLoginStatus?: unknown;
}

export interface ImageProvider {
  id: string;
  name: string;
  providerType: string;
  baseUrl: string;
  apiKeyEnv: string;
  apiKey?: string | null;
  model: string;
  enabled: boolean;
  timeoutSeconds: number;
  useSystemProxy?: boolean;
}

export interface VideoProvider {
  id: string;
  name: string;
  providerType: string;
  baseUrl: string;
  apiKeyEnv: string;
  apiKey?: string | null;
  model: string;
  enabled: boolean;
  timeoutSeconds: number;
  submitPath: string;
  statusPath: string;
  idPath: string;
  statusField: string;
  resultPath: string;
  completedStatuses: string[];
  failedStatuses: string[];
  pollIntervalSeconds: number;
  maxPollSeconds: number;
  downloadResult: boolean;
}

export interface SearchProvider {
  id: string;
  name: string;
  providerType: string;
  baseUrl: string;
  apiKeyEnv: string;
  apiKey?: string | null;
  enabled: boolean;
  timeoutSeconds: number;
}

export interface VisionProvider {
  id: string;
  name: string;
  providerType: string;
  baseUrl: string;
  apiKeyEnv: string;
  apiKey?: string | null;
  model: string;
  enabled: boolean;
  timeoutSeconds: number;
}

export interface BrowserProvider {
  id: string;
  name: string;
  providerType: string;
  baseUrl: string;
  apiKeyEnv: string;
  apiKey?: string | null;
  projectId?: string;
  recordSessions?: boolean;
  enabled: boolean;
  timeoutSeconds: number;
}

export interface ThemeConfig {
  id: string;
  name: string;
  mode: "light" | "dark" | "auto";
  active: boolean;
  css: string;
  createdAt: string;
  updatedAt: string;
}

export interface EmojiGroup {
  id: string;
  name: string;
  emotions: string[];
  images: string[];
  emotionImages?: Record<string, string[]>;
}

export interface WechatConfig {
  baseUrl: string;
  timeoutSeconds: number;
}

export interface WechatQrStartResult {
  qrcode: string;
  qrImage?: string | null;
  baseUrl: string;
  raw: unknown;
}

export interface WechatQrStatusResult {
  status: string;
  message?: string | null;
  account?: AccountConfig | null;
  host?: string | null;
  raw: unknown;
}

export interface WechatLinkSummary {
  accountId: string;
  personaId: string;
  personaName: string;
  accountNote: string;
  online: boolean;
}

export interface WechatInboundResult {
  messages: ChatMessage[];
  delivered: boolean;
  deliveryError?: string | null;
}

export interface WechatProcessedInbound {
  userId: string;
  text: string;
  conversationId?: string | null;
  delivered: boolean;
  deliveryError?: string | null;
}

export interface WechatPollResult {
  account: AccountConfig;
  processed: WechatProcessedInbound[];
  receivedCount: number;
  skippedCount: number;
  updatedBuffer: boolean;
  raw: unknown;
}

export interface MomentComment {
  id: string;
  personaId: string;
  text: string;
  replyTo?: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface MomentPost {
  id: string;
  personaId: string;
  body: string;
  coverPath?: string;
  likedBy: string[];
  comments: MomentComment[];
  createdAt: string;
  updatedAt: string;
}

export interface Persona {
  id: string;
  name: string;
  agentId?: string;
  avatarPath?: string | null;
  systemPrompt: string;
  characterPrompt: string;
  outputExamples: string;
  systemInstructions: string;
  llmProvider: string;
  llmModel: string;
  temperature: number;
  maxTokens: number;
  toolPolicy: {
    enabled: boolean;
    timeoutSeconds: number;
    maxIterations: number;
    maxFailureReplans: number;
    retryCount?: number;
    retryBackoffMs?: number;
  };
  emojiEnabled?: boolean;
  emojiGroup?: string;
  emojiSendProbability?: number;
  memory?: {
    enabled: boolean;
    triggerRounds: number;
    maxMemories: number;
    includeInPrompt: boolean;
  };
  proactive?: {
    enabled: boolean;
    minIdleHours: number;
    maxIdleHours: number;
    maxConsecutive: number;
    prompt: string;
    quietHours: {
      enabled: boolean;
      start: string;
      end: string;
    };
  };
  voiceReply?: {
    enabled: boolean;
    engine: string;
    language: string;
    voice: string;
    volume: string;
    pitch: string;
    pythonPath: string;
    modelDir: string;
    sampleRate: number;
    speed: number;
    oral: number;
    laugh: number;
    breakLevel: number;
    speakerSeed: number;
    speakerEmbedding: string;
    temperature: number;
    topP: number;
    topK: number;
    refineTextEnabled: boolean;
    refinePrompt: string;
    refineTemperature: number;
  };
  imageGeneration?: {
    enabled: boolean;
    provider: string;
    model: string;
    stylePrefix: string;
    artStyle: string;
    negativePrompt: string;
    negativeEnabled: boolean;
    refMode: "avatar" | "custom" | "none";
  };
}

export interface MemoryEntry {
  id: string;
  personaId: string;
  target?: "memory" | "user" | "session" | string;
  summary: string;
  importance: number;
  createdAt: string;
  updatedAt: string;
}

export interface MemoryStatus {
  personaId: string;
  personaName: string;
  enabled: boolean;
  includeInPrompt: boolean;
  triggerRounds: number;
  maxMemories: number;
  total: number;
  promptSafe: number;
  blockedBySecurityScan: number;
  promptInjected: number;
}

export interface ConversationDeleteMemorySettlingResult {
  status: "scheduled" | "settled" | "skipped" | "failed" | string;
  reason?: string | null;
  memoryCount: number;
}

export interface WorldbookSection {
  id: string;
  key: string;
  content: string;
  enabled: boolean;
}

export interface Worldbook {
  id: string;
  name: string;
  description: string;
  boundPersonas: string[];
  sections: WorldbookSection[];
  createdAt: string;
  updatedAt: string;
}

export interface PluginSummary {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  providedTools: string[];
  providedCapabilities?: string[];
  providedHooks?: string[];
  requiresEnv?: string[];
  missingEnv?: string[];
  envConfigured?: boolean;
  version?: string;
  author?: string;
  source?: string;
  homepageUrl?: string;
  kind?: string;
  path?: string;
  manifestPath?: string;
  entryPoint?: string;
}

export interface PluginAuxiliaryTaskSummary {
  pluginId: string;
  pluginName: string;
  key: string;
  displayName: string;
  description: string;
  defaults?: Record<string, unknown>;
}

export interface AgentAuxiliaryTaskSummary {
  key: string;
  displayName: string;
  description: string;
  source: string;
  pluginId?: string;
  pluginName?: string;
  defaults?: Record<string, unknown>;
}

export interface AgentAuxiliaryTaskAssignment {
  key: string;
  displayName: string;
  description: string;
  source: string;
  pluginId?: string;
  pluginName?: string;
  provider: string;
  model: string;
  baseUrl: string;
  apiKey: string;
  timeout: number;
  extraBody?: Record<string, unknown>;
}

export interface McpServer {
  id: string;
  name: string;
  transport?: "stdio" | "streamable_http" | "sse";
  command: string;
  args: string[];
  env?: Record<string, string>;
  url?: string;
  protocol: "oneShotJson" | "mcpJsonRpc" | "mcpJsonRpcLine";
  enabled: boolean;
  timeoutSeconds: number;
  supportsParallelToolCalls?: boolean;
  persistentSession?: boolean;
  keepAlive?: boolean;
  keepAliveIntervalSeconds?: number;
  keepAliveTimeoutSeconds?: number;
}

export interface CapabilityAdapter {
  name: string;
  description: string;
  mcpServer: string;
  mcpTool: string;
  parameters: unknown;
  paramMapping: Record<string, string>;
  injectFields: Record<string, string>;
  enabled: boolean;
}

export interface AgentConfig {
  enabled: boolean;
  mcpEnabled: boolean;
  skillsEnabled: boolean;
  allowShell: boolean;
  maxSubagents: number;
  maxSubagentDepth: number;
  maxToolIterations: number;
  skillsDir: string;
  enabledSkills: string[];
  enabledMcpServers: string[];
  enabledToolsets: string[];
  disabledToolsets: string[];
}

export interface SkillSummary {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  path: string;
}

export interface ProactiveStatus {
  personaId: string;
  personaName: string;
  enabled: boolean;
  conversationId?: string | null;
  lastUserAt: number;
  secondsSinceLastUser: number;
  lastReplyAt: number;
  secondsSinceLastReply: number;
  waitSeconds: number;
  readyInSeconds: number;
  consecutiveCount: number;
  maxConsecutive: number;
  inQuietHours: boolean;
  petVisionSuspended?: boolean;
  canFire: boolean;
  blockedReason: string;
}

export interface McpCallResult {
  ok: boolean;
  timedOut: boolean;
  elapsedMs: number;
  stdout: string;
  stderr: string;
  error?: string | null;
}

export interface McpToolInfo {
  name: string;
  description?: string | null;
  inputSchema?: unknown;
}

export interface McpListToolsResult {
  ok: boolean;
  timedOut: boolean;
  elapsedMs: number;
  tools: McpToolInfo[];
  raw?: unknown;
  error?: string | null;
}

export interface AgentDefinition {
  id: string;
  name: string;
  description: string;
  workspaceDir: string;
  llmProvider: string;
  llmModel: string;
  enabled: boolean;
  isDefault: boolean;
  mcpEnabled: boolean;
  skillsEnabled: boolean;
  allowShell: boolean;
  maxSubagents: number;
  maxSubagentDepth: number;
  maxToolIterations: number;
  skillsDir: string;
  enabledSkills: string[];
  enabledMcpServers: string[];
  enabledToolsets: string[];
  disabledToolsets: string[];
  createdAt: string;
  updatedAt: string;
}

export interface EnhancedSkillSummary {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  path: string;
  version: string;
  author: string;
  icon: string;
  isCore: boolean;
  isBundled: boolean;
  source: string;
  agentId: string;
  config: Record<string, string>;
}

export interface SkillBundle {
  id: string;
  name: string;
  description: string;
  skillIds: string[];
}

export interface MarketplaceSkill {
  id: string;
  name: string;
  description: string;
  version: string;
  author: string;
  downloadUrl: string;
  icon: string;
  tags: string[];
}

export interface SkillAuditFinding {
  severity: string;
  category: string;
  message: string;
  file: string;
  line?: number | null;
}

export interface SkillAuditReport {
  skillId: string;
  name: string;
  path: string;
  status: string;
  checkedFiles: number;
  findings: SkillAuditFinding[];
}

export interface SkillInstallRecord {
  skillId: string;
  name: string;
  source: string;
  identifier: string;
  installPath: string;
  auditStatus: string;
  installedAt: string;
}

export interface SkillAuditLogEntry {
  type?: string;
  createdAt?: string;
  skillId?: string;
  name?: string;
  source?: string;
  identifier?: string;
  installPath?: string;
  auditStatus?: string;
  findingCount?: number;
  removedFiles?: boolean;
  [key: string]: unknown;
}

export interface SkillTap {
  repo: string;
  path: string;
}

export interface SkillTapStatus {
  repo: string;
  path: string;
  status: string;
  entryCount: number;
  detail: string;
}

export interface SkillUpdateCheck {
  skillId: string;
  name: string;
  status: string;
  detail: string;
}

export interface SkillCuratorOverlap {
  umbrella: string;
  skillIds: string[];
  reason: string;
}

export interface SkillCuratorArchiveCandidate {
  skillId: string;
  name: string;
  reason: string;
}

export interface SkillCuratorReport {
  generatedAt: string;
  reportPath: string;
  totalSkills: number;
  externalSkills: number;
  bundledSkills: number;
  auditAttention: number;
  overlapClusters: SkillCuratorOverlap[];
  archiveCandidates: SkillCuratorArchiveCandidate[];
  recommendations: string[];
}

export interface SkillCuratorArchiveRecord {
  archiveId: string;
  skillId: string;
  name: string;
  originalPath: string;
  archivePath: string;
  reason: string;
  archivedAt: string;
  restoredAt?: string | null;
  installRecord: SkillInstallRecord;
}

export interface SkillCuratorState {
  paused: boolean;
  pinnedSkillIds: string[];
  archived: SkillCuratorArchiveRecord[];
  lastRunAt?: string | null;
  lastReportPath?: string | null;
  runCount: number;
  updatedAt: string;
}

export interface EnhancedPluginSummary {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  providedTools: string[];
  version: string;
  author: string;
  icon: string;
  source: string;
  homepageUrl: string;
}

export interface TokenUsageStats {
  promptTokens: number;
  completionTokens: number;
  totalTokens: number;
  callCount: number;
}

export interface TokenUsageResponse {
  promptTokens: number;
  completionTokens: number;
  totalTokens: number;
  callCount: number;
  byProvider?: Record<string, TokenUsageStats>;
  byModel?: Record<string, TokenUsageStats>;
}

export interface AppBuildInfo {
  productName: string;
  version: string;
  identifier: string;
  target: string;
  updateManifestUrl: string;
}

export interface AppUpdateCheck {
  currentVersion: string;
  latestVersion: string;
  updateAvailable: boolean;
  downloadUrl?: string | null;
  releaseUrl?: string | null;
  notes?: string | null;
  publishedAt?: string | null;
  sourceUrl: string;
  checkedAt: string;
}

export interface AppUpdateInstallResult {
  installerPath: string;
  helperScriptPath: string;
  mode: string;
  message: string;
}

export interface ModelCatalogEntry {
  id: string;
  name: string;
  family?: string;
  capabilities?: ModelCapabilities;
}

export interface DetectedModelList {
  ok: boolean;
  source: "live" | "catalog" | string;
  providerId: string;
  providerType: string;
  baseUrl: string;
  models: ModelCatalogEntry[];
  error?: string | null;
}

// ── Environment Check Types ──

export interface CheckItem {
  id: string;
  name: string;
  status: "ok" | "missing" | "not_running" | "installing" | "starting" | "error";
  detail: string;
  fixAction?: string | null;
  fixLabel?: string | null;
}

export interface EnvCheckResult {
  items: CheckItem[];
  allPassed: boolean;
}

export interface ActionResult {
  success: boolean;
  message: string;
  detail?: string | null;
}

export interface InstallProgressEvent {
  id: string;
  stage: string;
  message: string;
  percent?: number;
}
