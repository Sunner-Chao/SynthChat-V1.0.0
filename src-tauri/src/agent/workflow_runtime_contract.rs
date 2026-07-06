use serde_json::{json, Map, Value};

use crate::llm::{
    provider_tool_call_metadata_source_keys, PROVIDER_TOOL_CALL_ID_KEYS,
    PROVIDER_TOOL_CALL_META_KEY, TOOL_CALL_ARGUMENTS_CORRUPTION_KEY,
    TOOL_CALL_ARGUMENTS_CORRUPTION_MARKER,
};

use super::decision_parser::{APPROVED_TOOL_CALL_REPLAY_KEY, DECISION_ORIGIN_META_KEY};
use super::workflow_graph::{
    workflow_node_role_label, WORKFLOW_API_EVENT_NODE_TEMPLATE, WORKFLOW_API_EVENT_SNAPSHOT,
    WORKFLOW_API_EVENT_TRANSITION, WORKFLOW_DETAIL_ALIAS_PAIRS, WORKFLOW_NODE_ORDER,
    WORKFLOW_PHASE_INITIALIZED, WORKFLOW_PHASE_NODE, WORKFLOW_PHASE_TRANSITION,
    WORKFLOW_RUNTIME_KIND_SNAPSHOT, WORKFLOW_RUNTIME_KIND_TRANSITION,
    WORKFLOW_RUNTIME_NODE_KIND_PREFIX, WORKFLOW_RUNTIME_SOURCE, WORKFLOW_REASON_APPROVAL_REQUIRED,
    WORKFLOW_REASON_APPROVAL_RESUMED, WORKFLOW_REASON_CLARIFY_REQUIRES_USER_INPUT,
    WORKFLOW_REASON_DELEGATE_TASK_COMPLETED, WORKFLOW_REASON_DELEGATE_TASK_FAILED,
    WORKFLOW_REASON_DELEGATE_TASK_STARTED, WORKFLOW_REASON_DIRECT_TURN,
    WORKFLOW_REASON_FINAL_ANSWER_CANDIDATE, WORKFLOW_REASON_FUTURE_CHECKPOINT_WAIT,
    WORKFLOW_REASON_GROUP_CONTEXT_READY, WORKFLOW_REASON_NO_GROUP_ROOM_CONTEXT,
    WORKFLOW_REASON_QUEUED_TURN,
    WORKFLOW_REASON_ORDER, WORKFLOW_REASON_RESUME_CHECKPOINT_CONTINUED,
    WORKFLOW_REASON_RESUME_CHECKPOINT_REQUESTED, WORKFLOW_REASON_TOOL_CALLS,
    WORKFLOW_REASON_TOOL_OBSERVATIONS_RECORDED, WORKFLOW_STATUS_ORDER,
};

pub(super) const WORKFLOW_RUNTIME_EVENTS_SCHEMA: &str =
    "synthgraph_workflow_runtime_events_v1";
pub(super) const TOOL_CALL_PROTOCOL_SCHEMA: &str = "synthgraph_tool_call_protocol_v1";

pub(super) const KANBAN_RUNTIME_EVENT_SOURCES: &[&str] = &[
    "agent_queue",
    "agent_runs",
    WORKFLOW_RUNTIME_SOURCE,
    "agent_run.phase_events",
    "agent_run.tool_events",
    "managed_processes",
    "task.events",
];

pub(super) fn kanban_runtime_event_sources_value() -> Value {
    Value::Array(
        KANBAN_RUNTIME_EVENT_SOURCES
            .iter()
            .map(|source| json!(*source))
            .collect(),
    )
}

pub(super) fn agent_runtime_contracts() -> Value {
    let workflow_graph = workflow_graph_runtime_contract();
    let tool_call_protocol = tool_call_protocol_contract();
    json!({
        "workflowGraph": workflow_graph.clone(),
        "workflow_graph": workflow_graph,
        "toolCallProtocol": tool_call_protocol.clone(),
        "tool_call_protocol": tool_call_protocol
    })
}

pub(super) fn insert_agent_runtime_contract_aliases(object: &mut Map<String, Value>) {
    let workflow_graph = workflow_graph_runtime_contract();
    let tool_call_protocol = tool_call_protocol_contract();
    let runtime_contracts = json!({
        "workflowGraph": workflow_graph.clone(),
        "workflow_graph": workflow_graph.clone(),
        "toolCallProtocol": tool_call_protocol.clone(),
        "tool_call_protocol": tool_call_protocol.clone()
    });
    object
        .entry("workflowGraphRuntimeContract")
        .or_insert_with(|| workflow_graph.clone());
    object
        .entry("workflow_graph_runtime_contract")
        .or_insert_with(|| workflow_graph);
    object
        .entry("toolCallProtocolContract")
        .or_insert_with(|| tool_call_protocol.clone());
    object
        .entry("tool_call_protocol_contract")
        .or_insert_with(|| tool_call_protocol);
    object
        .entry("agentRuntimeContracts")
        .or_insert_with(|| runtime_contracts.clone());
    object
        .entry("agent_runtime_contracts")
        .or_insert_with(|| runtime_contracts.clone());
    object
        .entry("runtimeContracts")
        .or_insert_with(|| runtime_contracts.clone());
    object
        .entry("runtime_contracts")
        .or_insert_with(|| runtime_contracts);
}

fn workflow_topology_contract() -> Value {
    json!({
        "entryNode": "queue",
        "bootstrapCurrentNode": "planner",
        "terminalPatterns": [
            "reviewer completed",
            "reviewer skipped",
            "checkpoint waiting",
            "checkpoint completed after resume",
            "any node failed",
            "any node canceled"
        ],
        "edges": [
            {
                "from": "queue",
                "to": "group_room",
                "reasons": [WORKFLOW_REASON_QUEUED_TURN, WORKFLOW_REASON_DIRECT_TURN],
                "source": "bootstrap"
            },
            {
                "from": "group_room",
                "to": "planner",
                "reasons": [
                    WORKFLOW_REASON_GROUP_CONTEXT_READY,
                    WORKFLOW_REASON_NO_GROUP_ROOM_CONTEXT
                ],
                "source": "bootstrap"
            },
            {
                "from": "planner",
                "to": "executor",
                "reasons": [WORKFLOW_REASON_TOOL_CALLS],
                "source": "planner route"
            },
            {
                "from": "executor",
                "to": "planner",
                "reasons": [WORKFLOW_REASON_TOOL_OBSERVATIONS_RECORDED],
                "source": "executor route"
            },
            {
                "from": "executor",
                "to": "approval",
                "reasons": [WORKFLOW_REASON_APPROVAL_REQUIRED],
                "source": "approval gate"
            },
            {
                "from": "approval",
                "to": "planner",
                "reasons": [WORKFLOW_REASON_APPROVAL_RESUMED],
                "source": "approval continuation"
            },
            {
                "from": "executor",
                "to": "checkpoint",
                "reasons": [
                    WORKFLOW_REASON_CLARIFY_REQUIRES_USER_INPUT,
                    WORKFLOW_REASON_FUTURE_CHECKPOINT_WAIT
                ],
                "source": "checkpoint gate"
            },
            {
                "from": "current_node",
                "to": "checkpoint",
                "reasons": [WORKFLOW_REASON_RESUME_CHECKPOINT_REQUESTED],
                "source": "resume management"
            },
            {
                "from": "checkpoint",
                "to": "planner",
                "reasons": [WORKFLOW_REASON_RESUME_CHECKPOINT_CONTINUED],
                "source": "resume management"
            },
            {
                "from": "planner",
                "to": "reviewer",
                "reasons": [WORKFLOW_REASON_FINAL_ANSWER_CANDIDATE],
                "source": "review route"
            },
            {
                "from": "executor",
                "to": "group_room",
                "reasons": [WORKFLOW_REASON_DELEGATE_TASK_STARTED],
                "source": "delegation"
            },
            {
                "from": "group_room",
                "to": "executor",
                "reasons": [
                    WORKFLOW_REASON_DELEGATE_TASK_COMPLETED,
                    WORKFLOW_REASON_DELEGATE_TASK_FAILED
                ],
                "source": "delegation"
            }
        ],
        "purpose": "Describe the stable workflow graph topology exposed by SynthChat. Unknown future nodes, statuses, or reasons remain valid extension points, but clients can use these edges for rendering, filtering, and orchestration diagnostics."
    })
}

fn workflow_state_machine_contract() -> Value {
    json!({
        "driver": "workflow_graph::WorkflowDriver",
        "modeSource": "workflow_graph::workflow_mode_for_run",
        "layering": {
            "position": "outer workflow/graph layer",
            "preservedInnerLoop": "agent_loop::run_chat_turn remains the planner/executor/reviewer execution loop",
            "preservedToolDispatch": "tool_dispatch, MCP dispatch, store persistence, approval_gateway, and provider adapters remain the execution substrate",
            "purpose": "Make orchestration state explicit without replacing the existing Hermes-like local runtime capabilities."
        },
        "nodeDrivers": {
            "queue": {
                "accessor": "WorkflowDriver::queue",
                "nodeType": "WorkflowQueueNode",
                "recorders": ["completed", "skipped", "terminal"],
                "statusWrites": ["completed", "skipped", "failed", "canceled"]
            },
            "group_room": {
                "accessor": "WorkflowDriver::group_room",
                "nodeType": "WorkflowGroupRoomNode",
                "recorders": ["running", "completed", "failed", "skipped"],
                "statusWrites": ["running", "completed", "failed", "skipped"]
            },
            "planner": {
                "accessor": "WorkflowDriver::planner",
                "nodeType": "WorkflowPlannerNode",
                "recorders": ["running", "failed", "route"],
                "statusWrites": ["running", "completed", "failed"]
            },
            "executor": {
                "accessor": "WorkflowDriver::executor",
                "nodeType": "WorkflowExecutorNode",
                "recorders": ["tool_started", "tool_call_bridge_target", "continue_route", "approval_route", "tool_resolution", "parallel_batch_started", "parallel_batch_completed", "failed"],
                "statusWrites": ["running", "completed", "failed"]
            },
            "approval": {
                "accessor": "WorkflowDriver::approval",
                "nodeType": "WorkflowApprovalNode",
                "recorders": ["resumed", "resolved"],
                "statusWrites": ["waiting", "completed", "failed", "canceled"]
            },
            "checkpoint": {
                "accessor": "WorkflowDriver::checkpoint",
                "nodeType": "WorkflowCheckpointNode",
                "recorders": ["completed", "failed", "waiting", "resume_requested_from_current", "resume_continued_to_planner", "future_wait_from_executor"],
                "statusWrites": ["completed", "failed", "waiting"]
            },
            "reviewer": {
                "accessor": "WorkflowDriver::reviewer",
                "nodeType": "WorkflowReviewerNode",
                "recorders": ["completed", "skipped"],
                "statusWrites": ["running", "completed", "skipped"]
            }
        },
        "statusSemantics": {
            "pending": "node is part of the graph but has not run in the current cycle",
            "running": "node currently owns observable work or a delegated subflow",
            "waiting": "node is intentionally paused for approval, checkpoint, user input, or future scheduling",
            "completed": "node completed its current responsibility",
            "skipped": "node was explicitly bypassed while remaining visible in the graph",
            "failed": "node failed and should be treated as a terminal or diagnostic focus",
            "canceled": "node was canceled or denied and should be treated as terminal"
        },
        "terminalPolicy": {
            "terminalStatuses": ["completed", "failed", "canceled", "skipped"],
            "waitingIsTerminalForTurn": ["checkpoint"],
            "preserveCurrentField": "preserveCurrent/preserve_current",
            "currentNodePolicy": "node events normally move currentNode to the node; preserveCurrent prevents skipped or terminal bookkeeping from stealing currentNode; transition events move currentNode to transition.to",
            "queueTerminalPolicy": "queue terminal bookkeeping preserves the active workflow node for completed/failed queue cleanup and lets canceled queue cleanup become current",
            "reviewerTerminalPolicy": "reviewer completed/skipped marks final answer candidate handling as terminal for the current turn",
            "checkpointResumePolicy": "resume management records current node -> checkpoint and checkpoint -> planner transitions around continuation"
        },
        "edgePolicy": {
            "knownEdges": "topology.edges",
            "runtimeAnnotations": ["topologyEdgeKnown/topology_edge_known", "topologyReasonKnown/topology_reason_known", "topologyEdgeSource/topology_edge_source", "topologyEdgeLabel/topology_edge_label"],
            "unknownNodePolicy": "future nodes remain valid extension points and should be rendered instead of rejected",
            "unknownReasonPolicy": "future transition reasons remain valid extension points and should be rendered after known reason ordering"
        },
        "sourceBoundaries": {
            "planner": ["agent_loop::run_chat_turn", "approval_gateway::continue_approved_run"],
            "executor": ["executor_core", "tool_dispatch", "MCP dispatch", "plugin bridge dispatch"],
            "approval": ["approval_gateway", "tool approval store"],
            "checkpoint": ["run_management::resume_agent_run", "state_tools", "future wait gates"],
            "queue": ["agent queue drain", "direct chat turn bootstrap"],
            "group_room": ["workflow bootstrap context", "delegation parent orchestration"],
            "reviewer": ["final answer candidate handling"]
        },
        "clientMergeContract": "clientMergeContract/client_merge_contract",
        "purpose": "Document the explicit state-machine layer that makes queue, group_room, planner, executor, approval, checkpoint, and reviewer observable while preserving the existing runtime substrate."
    })
}

fn workflow_client_merge_contract() -> Value {
    json!({
        "frontendStore": "src/lib/store.ts::mergeWorkflowGraphFromRunEvent",
        "snapshotStrategy": "prefer event.workflowGraph/event.workflow_graph when present, otherwise reconstruct graph from workflow phase events",
        "detailAliasNormalizer": "src/lib/store.ts::normalizeWorkflowDetailAliases recursively mirrors workflow_graph::workflow_detail_with_runtime_aliases for node and transition detail payloads",
        "nodeUpdatePolicy": "node events replace the matching node by node name; preserveCurrent/preserve_current keeps skipped or checkpoint completion events from stealing currentNode",
        "transitionPolicy": "transition events append to graph.transitions and move currentNode to transition.to when present"
    })
}

fn workflow_detail_alias_contract() -> Value {
    Value::Array(
        WORKFLOW_DETAIL_ALIAS_PAIRS
            .iter()
            .map(|(camel, snake)| json!(format!("{camel}/{snake}")))
            .collect(),
    )
}

fn workflow_detail_stable_fields(fields: &[&str]) -> Value {
    let mut values = Vec::<String>::new();
    for &field in fields {
        workflow_push_unique_field(&mut values, field);
        if let Some((_, snake)) = WORKFLOW_DETAIL_ALIAS_PAIRS
            .iter()
            .find(|(camel, _)| *camel == field)
        {
            workflow_push_unique_field(&mut values, *snake);
        }
    }
    Value::Array(values.into_iter().map(Value::String).collect())
}

fn workflow_push_unique_field(values: &mut Vec<String>, field: &str) {
    if !values.iter().any(|value| value == field) {
        values.push(field.to_string());
    }
}

pub(super) fn workflow_graph_runtime_contract() -> Value {
    let runtime_node_kind = format!("{WORKFLOW_RUNTIME_NODE_KIND_PREFIX}<status>");
    let runtime_event_kinds = json!([
        WORKFLOW_RUNTIME_KIND_SNAPSHOT,
        runtime_node_kind.clone(),
        WORKFLOW_RUNTIME_KIND_TRANSITION
    ]);
    let api_event_kinds = json!([
        WORKFLOW_API_EVENT_SNAPSHOT,
        WORKFLOW_API_EVENT_NODE_TEMPLATE,
        WORKFLOW_API_EVENT_TRANSITION
    ]);
    let mut runtime_event_kind_map = serde_json::Map::new();
    runtime_event_kind_map.insert(
        WORKFLOW_RUNTIME_KIND_SNAPSHOT.into(),
        json!(WORKFLOW_API_EVENT_SNAPSHOT),
    );
    runtime_event_kind_map.insert(
        runtime_node_kind.clone(),
        json!(WORKFLOW_API_EVENT_NODE_TEMPLATE),
    );
    runtime_event_kind_map.insert(
        WORKFLOW_RUNTIME_KIND_TRANSITION.into(),
        json!(WORKFLOW_API_EVENT_TRANSITION),
    );
    let node_roles = Value::Object(
        WORKFLOW_NODE_ORDER
            .iter()
            .map(|node| ((*node).to_string(), json!(workflow_node_role_label(node))))
            .collect(),
    );
    json!({
        "schema": WORKFLOW_RUNTIME_EVENTS_SCHEMA,
        "source": WORKFLOW_RUNTIME_SOURCE,
        "nodeOrder": WORKFLOW_NODE_ORDER,
        "statusOrder": WORKFLOW_STATUS_ORDER,
        "transitionReasonOrder": WORKFLOW_REASON_ORDER,
        "nodeRoles": node_roles,
        "topology": workflow_topology_contract(),
        "stateMachine": workflow_state_machine_contract(),
        "state_machine": workflow_state_machine_contract(),
        "eventKinds": runtime_event_kinds,
        "apiRunEventKinds": api_event_kinds,
        "runtimeEventKindMap": Value::Object(runtime_event_kind_map),
        "eventSurfaces": {
            "apiRunEvents": {
                "endpoint": "/v1/runs/{run_id}/events",
                "streaming": true,
                "sse": true,
                "object": "hermes.run.event",
                "types": [
                    WORKFLOW_API_EVENT_SNAPSHOT,
                    WORKFLOW_API_EVENT_NODE_TEMPLATE,
                    WORKFLOW_API_EVENT_TRANSITION
                ],
                "envelopeFields": ["id", "object", "run_id", "type", "created_at", "data"],
                "payloadField": "data"
            },
            "dashboardRuntimeEvents": {
                "endpoint": "/api/plugins/kanban/runtime-events",
                "sseEndpoint": "/api/plugins/kanban/runtime-events/stream",
                "schema": "hermes_kanban_runtime_events_desktop_v1",
                "source": WORKFLOW_RUNTIME_SOURCE,
                "kinds": [
                    WORKFLOW_RUNTIME_KIND_SNAPSHOT,
                    runtime_node_kind,
                    WORKFLOW_RUNTIME_KIND_TRANSITION
                ],
                "envelopeFields": ["id", "kind", "source", "status", "run_id", "conversation_id", "queue_item_id", "payload", "created_at"],
                "payloadField": "payload"
            },
            "tauriRunEvent": {
                "event": "synthchat-agent-run-event",
                "payloadField": "workflowGraph",
                "payloadAliases": ["workflowGraph", "workflow_graph"],
                "phaseDetailSequenceAliases": ["eventSequence", "event_sequence"],
                "mergeStrategy": format!("full snapshot preferred; clients may reconstruct from phase {WORKFLOW_PHASE_INITIALIZED}/{WORKFLOW_PHASE_NODE}/{WORKFLOW_PHASE_TRANSITION} when snapshot is missing")
            }
        },
        "graphRootFields": ["schema", "mode", "requestSource", "request_source", "toolContext", "tool_context", "currentNode", "current_node", "currentStatus", "current_status", "lastEventSequence", "last_event_sequence", "updatedAt", "updated_at", "nodes", "transitions"],
        "summaryFields": ["schema", "mode", "requestSource", "request_source", "toolContext", "tool_context", "currentNode", "current_node", "currentStatus", "current_status", "lastEventSequence", "last_event_sequence", "updatedAt", "updated_at", "nodeCount", "node_count", "transitionCount", "transition_count", "statusCounts", "status_counts", "toolOrigins", "tool_origins"],
        "payloadBuilders": {
            "summary": "workflow_graph::workflow_graph_runtime_summary",
            "runResponseValues": "workflow_graph::workflow_graph_run_response_values",
            "runResponseAliases": "workflow_graph::insert_workflow_graph_run_response_aliases",
            "graphAliasNormalizer": "workflow_graph::workflow_graph_with_runtime_aliases",
            "snapshot": "workflow_graph::workflow_graph_snapshot_runtime_payload",
            "node": "workflow_graph::workflow_graph_node_runtime_payload",
            "transition": "workflow_graph::workflow_graph_transition_runtime_payload"
        },
        "runtimeContractAliasBuilder": "workflow_runtime_contract::insert_agent_runtime_contract_aliases",
        "runtimeContractAliases": [
            "workflowGraphRuntimeContract",
            "workflow_graph_runtime_contract",
            "toolCallProtocolContract",
            "tool_call_protocol_contract",
            "agentRuntimeContracts",
            "agent_runtime_contracts",
            "runtimeContracts",
            "runtime_contracts"
        ],
        "runResponseAliases": {
            "workflowGraph": "full AgentRunRecord.workflowGraph with runtime aliases normalized",
            "workflow_graph": "full AgentRunRecord.workflowGraph snake_case alias with runtime aliases normalized",
            "workflowSummary": "WorkflowRuntimeSummary",
            "workflow_summary": "WorkflowRuntimeSummary snake_case alias"
        },
        "snapshotPayload": {
            "summary": "WorkflowRuntimeSummary",
            "workflowSummary": "WorkflowRuntimeSummary camelCase alias",
            "workflow_summary": "WorkflowRuntimeSummary snake_case alias",
            "graph": "AgentRunRecord.workflowGraph with runtime aliases normalized",
            "workflowGraph": "AgentRunRecord.workflowGraph camelCase alias with runtime aliases normalized",
            "workflow_graph": "AgentRunRecord.workflowGraph snake_case alias with runtime aliases normalized"
        },
        "graphPayloadAliasGuarantee": {
            "appliesTo": ["runResponseAliases.workflowGraph", "runResponseAliases.workflow_graph", "snapshotPayload.graph", "snapshotPayload.workflowGraph", "snapshotPayload.workflow_graph"],
            "rootAliases": ["requestSource/request_source", "toolContext/tool_context", "currentNode/current_node", "currentStatus/current_status", "lastEventSequence/last_event_sequence", "updatedAt/updated_at"],
            "nodeAliases": ["eventSequence/event_sequence", "updatedAt/updated_at", "role"],
            "transitionAliases": ["eventSequence/event_sequence", "updatedAt/updated_at", "topologyEdgeKnown/topology_edge_known", "topologyReasonKnown/topology_reason_known", "topologyEdgeSource/topology_edge_source", "topologyEdgeLabel/topology_edge_label"],
            "detailAliases": workflow_detail_alias_contract(),
            "purpose": "Clients that prefer snake_case can consume the full graph payload directly, not only WorkflowRuntimeSummary."
        },
        "clientMergeContract": workflow_client_merge_contract(),
        "client_merge_contract": workflow_client_merge_contract(),
        "nodePayload": {
            "node": "queue|group_room|planner|executor|approval|checkpoint|reviewer|future_node",
            "role": "queue admission|group context|decision planning|tool execution|human approval gate|state checkpoint|final review|custom workflow node",
            "status": "pending|running|completed|waiting|failed|canceled|skipped|future_status",
            "detail": "object with runtime detail aliases normalized",
            "eventSequence": "number|null",
            "event_sequence": "number|null snake_case alias",
            "graphSummary": "WorkflowRuntimeSummary",
            "graph_summary": "WorkflowRuntimeSummary snake_case alias"
        },
        "transitionPayload": {
            "from": "WorkflowNodeName|null",
            "to": "WorkflowNodeName|null",
            "reason": "string",
            "topologyEdgeKnown": "boolean|null, true when the edge matches topology.edges",
            "topology_edge_known": "boolean|null snake_case alias",
            "topologyReasonKnown": "boolean|null, true when reason is in transitionReasonOrder",
            "topology_reason_known": "boolean|null snake_case alias",
            "topologyEdgeSource": "string|null, source from topology.edges when known",
            "topology_edge_source": "string|null snake_case alias",
            "topologyEdgeLabel": "string|null, human-readable from -> to (reason)",
            "topology_edge_label": "string|null snake_case alias",
            "detail": "object with runtime detail aliases normalized",
            "eventSequence": "number|null",
            "event_sequence": "number|null snake_case alias",
            "graphSummary": "WorkflowRuntimeSummary",
            "graph_summary": "WorkflowRuntimeSummary snake_case alias"
        },
        "detailContracts": {
            "queue.admission": {
                "nodeStatuses": ["completed", "skipped", "failed", "canceled"],
                "transitionReasons": [WORKFLOW_REASON_QUEUED_TURN, WORKFLOW_REASON_DIRECT_TURN],
                "stableFields": workflow_detail_stable_fields(&[
                    "mode",
                    "requestSource",
                    "toolContext",
                    "queueItemId",
                    "reason",
                    "admission",
                    "queueStatus",
                    "queueLifecycle",
                    "preserveCurrent",
                    "error"
                ]),
                "queuedTurn": {
                    "nodeStatus": "completed",
                    "admission": WORKFLOW_REASON_QUEUED_TURN,
                    "queueStatus": "claimed",
                    "queueLifecycle": "dequeued_for_run"
                },
                "directTurn": {
                    "nodeStatus": "skipped",
                    "admission": WORKFLOW_REASON_DIRECT_TURN,
                    "queueStatus": "not_queued",
                    "queueLifecycle": "not_applicable"
                },
                "terminalUpdate": {
                    "preserveCurrentByQueueStatus": {
                        "completed": true,
                        "failed": true,
                        "canceled": false
                    },
                    "queueLifecycleValues": ["turn_completed", "turn_failed", "canceled"],
                    "nodeStatusByQueueStatus": {
                        "completed": "completed",
                        "failed": "failed",
                        "canceled": "canceled"
                    }
                }
            },
            "group_room.context": {
                "nodeStatuses": ["completed", "skipped"],
                "transitionReasons": [
                    WORKFLOW_REASON_GROUP_CONTEXT_READY,
                    WORKFLOW_REASON_NO_GROUP_ROOM_CONTEXT
                ],
                "stableFields": workflow_detail_stable_fields(&[
                    "mode",
                    "requestSource",
                    "toolContext",
                    "source",
                    "conversationKind",
                    "roomId",
                    "channelId",
                    "chatId",
                    "threadId",
                    "groupId",
                    "context",
                    "reason"
                ]),
                "completed": {
                    "nodeStatus": "completed",
                    "transition": "group_room -> planner",
                    "transitionReason": WORKFLOW_REASON_GROUP_CONTEXT_READY,
                    "transitionDetail": {"groupRoom": "context_ready"}
                },
                "skipped": {
                    "nodeStatus": "skipped",
                    "reason": WORKFLOW_REASON_NO_GROUP_ROOM_CONTEXT,
                    "transition": "group_room -> planner",
                    "transitionReason": WORKFLOW_REASON_NO_GROUP_ROOM_CONTEXT,
                    "transitionDetail": {"groupRoom": "not_applicable"}
                }
            },
            "bootstrap.initial_nodes": {
                "nodeStatuses": ["pending"],
                "appliesTo": ["planner", "executor", "approval", "checkpoint", "reviewer"],
                "stableFields": workflow_detail_stable_fields(&["mode", "requestSource", "toolContext"]),
                "currentNodeAfterBootstrap": "planner",
                "eventSequence": 0
            },
            "approval.lifecycle": {
                "nodeStatuses": ["waiting", "completed", "failed", "canceled"],
                "transitionReasons": [
                    WORKFLOW_REASON_APPROVAL_REQUIRED,
                    WORKFLOW_REASON_APPROVAL_RESUMED
                ],
                "stableFields": workflow_detail_stable_fields(&[
                    "mode",
                    "iteration",
                    "requestedName",
                    "serverId",
                    "toolName",
                    "toolKind",
                    "sourceLabel",
                    "approvalId",
                    "reason",
                    "status",
                    "error"
                ]),
                "waiting": {
                    "nodeStatus": "waiting",
                    "transition": "executor -> approval",
                    "transitionReason": WORKFLOW_REASON_APPROVAL_REQUIRED
                },
                "resumed": {
                    "nodeStatus": "completed",
                    "transition": "approval -> planner",
                    "transitionReason": WORKFLOW_REASON_APPROVAL_RESUMED
                },
                "resolvedWithoutResume": {
                    "nodeStatuses": ["completed", "failed", "canceled"],
                    "statusMapping": {
                        "approved": "completed",
                        "failed": "failed",
                        "denied": "canceled",
                        "canceled": "canceled"
                    },
                    "appliesTo": "denied, canceled, or failed approval updates that do not continue the run"
                }
            },
            "checkpoint.lifecycle": {
                "nodeStatuses": ["completed", "waiting", "failed"],
                "transitionReasons": [
                    WORKFLOW_REASON_CLARIFY_REQUIRES_USER_INPUT,
                    WORKFLOW_REASON_FUTURE_CHECKPOINT_WAIT,
                    WORKFLOW_REASON_RESUME_CHECKPOINT_REQUESTED,
                    WORKFLOW_REASON_RESUME_CHECKPOINT_CONTINUED
                ],
                "stableFields": workflow_detail_stable_fields(&[
                    "mode",
                    "state",
                    "summary",
                    "kind",
                    "checkpointScope",
                    "checkpointId",
                    "checkpointState",
                    "checkpointSummary",
                    "checkpointIteration",
                    "previousState",
                    "runState",
                    "preserveCurrent",
                    "iteration",
                    "mutationKind",
                    "targetSummary",
                    "toolName",
                    "value"
                ]),
                "manualCheckpoint": {
                    "kind": "manual_checkpoint",
                    "nodeStatus": "completed"
                },
                "automaticMutationCheckpoint": {
                    "kind": "automatic_mutation_checkpoint",
                    "nodeStatus": "completed",
                    "checkpointScope": "pre_mutation"
                },
                "clarifyPause": {
                    "kind": "clarify_pause",
                    "nodeStatus": "waiting",
                    "checkpointScope": "user_input",
                    "transition": "executor -> checkpoint"
                },
                "futureWait": {
                    "nodeStatus": "waiting",
                    "checkpointScope": "future",
                    "transition": "executor -> checkpoint",
                    "transitionReason": WORKFLOW_REASON_FUTURE_CHECKPOINT_WAIT
                },
                "resumeCheckpoint": {
                    "kind": "resume_checkpoint",
                    "checkpointScope": "resume",
                    "waitingState": "resume_started",
                    "completedState": "resumed",
                    "failedState": "resume_failed",
                    "requestedTransition": "current node -> checkpoint",
                    "requestedTransitionReason": WORKFLOW_REASON_RESUME_CHECKPOINT_REQUESTED,
                    "continuedTransition": "checkpoint -> planner",
                    "continuedTransitionReason": WORKFLOW_REASON_RESUME_CHECKPOINT_CONTINUED
                }
            },
            "planner.lifecycle": {
                "nodeStatuses": ["running", "completed", "failed"],
                "transitionReasons": [
                    WORKFLOW_REASON_TOOL_CALLS,
                    WORKFLOW_REASON_FINAL_ANSWER_CANDIDATE
                ],
                "stableFields": workflow_detail_stable_fields(&[
                    "mode",
                    "iteration",
                    "action",
                    "toolCount",
                    "tools",
                    "toolProtocol",
                    "toolOrigins",
                    "toolCallIds",
                    "toolCalls",
                    "errorKind",
                    "error"
                ]),
                "actions": ["tool", "final"],
                "toolProtocol": "canonical_tool_call_v1",
                "toolOriginValues": ["provider_native", "planner_json", "hermes_markup"],
                "toolCallSummaryFields": ["name", "origin", "id", "providerNative", "provider_native"],
                "failureKinds": [
                    "context_compression",
                    "iteration_budget_exhausted",
                    "llm_error",
                    "llm_recovery_exhausted",
                    "no_final_answer",
                    "provider_turn_aborted",
                    "tool_approval_required",
                    "tool_schema_validation",
                    "tool_unavailable",
                    "tool_request"
                ]
            },
            "executor.lifecycle": {
                "nodeStatuses": ["running", "completed", "failed"],
                "entryPoints": [
                    "agent_loop planner tool execution",
                    "approval_gateway approval continuation",
                    "direct SynthChat MCP bridge",
                    "parallel tool batch wrapper",
                    "python plugin bridge dispatch"
                ],
                "transitionReasons": [
                    WORKFLOW_REASON_TOOL_OBSERVATIONS_RECORDED,
                    WORKFLOW_REASON_APPROVAL_REQUIRED,
                    WORKFLOW_REASON_CLARIFY_REQUIRES_USER_INPUT
                ],
                "stableFields": workflow_detail_stable_fields(&[
                    "mode",
                    "iteration",
                    "toolCount",
                    "tools",
                    "toolProtocol",
                    "toolOrigins",
                    "toolCallIds",
                    "toolCalls",
                    "parallel",
                    "stage",
                    "succeeded",
                    "failed",
                    "halted",
                    "resolution",
                    "available",
                    "requestedName",
                    "serverId",
                    "toolName",
                    "toolKind",
                    "sourceLabel",
                    "definitionName",
                    "requiresApproval",
                    "source",
                    "directBridge",
                    "approvedToolCallReplay",
                    "bridgeStatus",
                    "bridgeRejectionReason",
                    "bridgeStage",
                    "lastBridgeTarget",
                    "reason",
                    "error"
                ]),
                "bridgeStatusValues": ["dispatch_ready", "approval_required", "context_blocked", "unavailable"],
                "resolutionValues": ["internal", "mcp", "unavailable"],
                "stageValues": [
                    "tool_started",
                    "tool_call_bridge_target",
                    "parallel_batch_started",
                    "parallel_batch_completed",
                    "base_policy",
                    "scheduled_policy",
                    "smart_policy",
                    "subagent_policy",
                    "approval_policy"
                ]
            },
            "reviewer.lifecycle": {
                "nodeStatuses": ["running", "completed", "skipped"],
                "transitionReasons": [WORKFLOW_REASON_FINAL_ANSWER_CANDIDATE],
                "stableFields": workflow_detail_stable_fields(&[
                    "mode",
                    "iteration",
                    "messageId",
                    "model",
                    "providerId",
                    "reason"
                ]),
                "skipReasons": ["no_final_answer", "iteration_budget_exhausted", "future_skip_reason"]
            },
            "timeout.failure": {
                "nodeStatuses": ["failed"],
                "appliesTo": "the workflow current node when the agent run timeout fires",
                "stableFields": workflow_detail_stable_fields(&["mode", "errorKind", "reason", "timeoutSeconds"]),
                "errorKind": "agent_run_timeout"
            },
            "abort.current_node": {
                "nodeStatuses": ["canceled"],
                "appliesTo": "the workflow current node when a non-terminal agent run is aborted before a more specific failed/completed/canceled/skipped terminal node is recorded",
                "stableFields": workflow_detail_stable_fields(&["aborted", "runState", "reason"]),
                "status": "canceled"
            },
            "group_room.delegate_task": {
                "nodeStatuses": ["running", "completed", "failed"],
                "transitionReasons": [
                    WORKFLOW_REASON_DELEGATE_TASK_STARTED,
                    WORKFLOW_REASON_DELEGATE_TASK_COMPLETED,
                    WORKFLOW_REASON_DELEGATE_TASK_FAILED
                ],
                "stableFields": workflow_detail_stable_fields(&[
                    "mode",
                    "source",
                    "phase",
                    "batch",
                    "requestedChildren",
                    "existingChildren",
                    "parentDepth",
                    "childDepth",
                    "maxSubagents",
                    "maxSubagentDepth",
                    "maxConcurrentChildren",
                    "strategy",
                    "orchestratorEnabled",
                    "subagentAutoApprove",
                    "inheritMcpToolsets",
                    "children",
                    "ok",
                    "completedChildren",
                    "failedChildren",
                    "abortedChildren",
                    "unknownChildren",
                    "results",
                    "error"
                ]),
                "phaseValues": ["started", "completed", "failed"],
                "childSummaryFields": workflow_detail_stable_fields(&[
                    "childIndex",
                    "role",
                    "taskPreview",
                    "toolsets",
                    "canDelegate",
                    "maxIterations",
                    "transport",
                    "acpCommand",
                    "acpSessionMode"
                ]),
                "resultSummaryFields": workflow_detail_stable_fields(&[
                    "status",
                    "childRunId",
                    "childConversationId",
                    "role",
                    "maxIterations",
                    "transport",
                    "taskPreview",
                    "resultPreview",
                    "errorPreview",
                    "hasDiagnosticArtifact"
                ]),
                "transitionDetailFields": workflow_detail_stable_fields(&[
                    "source",
                    "phase",
                    "batch",
                    "requestedChildren",
                    "existingChildren",
                    "parentDepth",
                    "ok",
                    "completedChildren",
                    "failedChildren",
                    "abortedChildren",
                    "unknownChildren",
                    "error"
                ])
            }
        },
        "ordering": "event.created_at is node/transition updatedAt when present, otherwise graph updatedAt, then run updatedAt; cursor id is assigned after merged runtime sort",
        "purpose": "Expose the explicit workflow/graph layer through API run events, dashboard runtime events, and Tauri run snapshots without requiring clients to parse generic phase detail payloads."
    })
}

pub(super) fn tool_call_protocol_contract() -> Value {
    json!({
        "schema": TOOL_CALL_PROTOCOL_SCHEMA,
        "canonicalShape": "AgentToolCall",
        "canonicalFields": {
            "id": "provider call id when present",
            "name": "resolved tool name",
            "arguments": "JSON object payload with provider metadata preserved until execution",
            "origin": "provider_native|planner_json|hermes_markup",
            "providerMeta": "provider-native metadata copied from the source call"
        },
        "acceptedOrigins": ["provider_native", "planner_json", "hermes_markup"],
        "canonicalizationPipeline": [
            {
                "stage": "provider_adapter_normalization",
                "inputOrigins": ["provider_native"],
                "entryPoints": [
                    "llm provider adapters",
                    "llm::normalize_provider_tool_arguments"
                ],
                "output": "LlmReply.content.tool_calls with provider metadata attached under the normalized metadata key"
            },
            {
                "stage": "planner_decision_parsing",
                "inputOrigins": ["planner_json", "provider_native", "hermes_markup"],
                "entryPoints": [
                    "decision_parser::parse_agent_decision",
                    "decision_parser::normalize_agent_decision"
                ],
                "output": "planner decision JSON normalized to action=tool with a canonical list of tool requests"
            },
            {
                "stage": "canonical_tool_call_projection",
                "inputOrigins": ["planner_json", "provider_native", "hermes_markup"],
                "entryPoints": [
                    "decision_parser::canonical_tool_calls_from_decision"
                ],
                "output": "Vec<AgentToolCall> with id, name, arguments, origin, and provider_meta"
            }
        ],
        "providerAdapterBoundary": {
            "llmReplyContentBridge": true,
            "plannerEntryPoints": [
                "agent_loop.run_chat_turn -> parse_agent_decision(reply.content)",
                "approval_gateway.continue_approved_run -> parse_agent_decision(reply.content)"
            ],
            "normalizedProviders": [
                "responses",
                "openai_chat_completions",
                "anthropic",
                "gemini",
                "bedrock"
            ],
            "normalizedContentShape": {"tool_calls": [{"type": "function", "id": "provider_call_id", "function": {"name": "tool_name", "arguments": "json-string|object"}}]},
            "argumentNormalization": {
                "providerHelper": "llm::normalize_provider_tool_arguments",
                "plannerHelper": "decision_parser::parse_tool_arguments_json",
                "emptyString": "{}",
                "noneLiteral": "{}",
                "repairPolicy": "provider transports repair common JSON damage before tool calls enter LlmReply.content; the shared planner parser still owns final JSON-string/object normalization",
                "corruptionMarkerKey": TOOL_CALL_ARGUMENTS_CORRUPTION_KEY,
                "corruptionMarkerMessage": TOOL_CALL_ARGUMENTS_CORRUPTION_MARKER
            },
            "providerDataRole": "provider_data stores reasoning/replay metadata for future provider requests; executable tool calls enter the planner through LlmReply.content and the shared decision parser."
        },
        "acceptedInputShapes": {
            "plannerJson": [
                {"action": "tool", "tool": "tool_name", "payload": "object"},
                {"useTool": true, "toolName": "tool_name", "args": "object"},
                {"use_tool": true, "tool_name": "tool_name", "input": "object"},
                {"name": "tool_name", "parameters": "object"},
                {"action": "tool_call", "function": {"name": "tool_name", "arguments": "json-string|object"}},
                {"action": "function_call", "function": {"name": "tool_name", "arguments": "json-string|object"}},
                {"function_call": {"name": "tool_name", "arguments": "json-string|object"}},
                {"tool_calls": [{"name": "tool_name", "arguments": "json-string|object"}]},
                {"toolCalls": [{"tool": "tool_name", "payload": "object"}]},
                {"tools": [{"name": "tool_name", "args": "object"}]},
                {"calls": [{"name": "tool_name", "input": "object"}]},
                {"function_calls": [{"function": {"name": "tool_name", "arguments": "json-string|object"}}]}
            ],
            "fieldAliases": {
                "actionKeys": ["action", "type", "decision"],
                "toolActionValues": ["tool", "use_tool", "call_tool", "tools", "tool_call", "function_call", "function_calls"],
                "useToolKeys": ["useTool", "use_tool"],
                "singleCallNameKeys": ["tool", "toolName", "tool_name", "name", "function", "function_call"],
                "singleCallArgumentKeys": ["payload", "arguments", "args", "input", "parameters", "function.arguments", "function_call.arguments"],
                "multiCallArrayKeys": ["toolCalls", "tool_calls", "tools", "calls", "function_calls"],
                "functionObjectKeys": ["function", "function_call"]
            },
            "providerNative": {
                "sourceKeys": provider_tool_call_metadata_source_keys(),
                "normalizedMetadataKey": PROVIDER_TOOL_CALL_META_KEY,
                "callIdLookupKeys": PROVIDER_TOOL_CALL_ID_KEYS,
                "metadataPolicy": "preserved through planning, stripped before schema validation and direct execution, inherited by tool_call target events"
            },
            "hermesMarkup": {
                "shape": "<tool_call><function=tool_name><parameter=name>value</parameter></function></tool_call>",
                "decisionOriginMetadataKey": DECISION_ORIGIN_META_KEY,
                "originValue": "hermes_markup"
            }
        },
        "validation": {
            "plannerValidationEntry": "validated_tool_requests_from_decision_with_error",
            "plannerCanonicalValidationEntry": "validated_tool_calls_from_decision_with_error",
            "sharedSchemaValidator": "validate_tool_call_payload",
            "definitionResolution": "planner prompts and validation resolve tools over context-visible internal, MCP, and plugin tool definitions from visible_tool_definitions_for_agent",
            "internalToolSchemaSource": "tool_registry::internal_tool_input_schema for SynthChat-native discovery and bridge tools; MCP and plugin tools retain provider schemas",
            "schemaCombinators": ["allOf", "anyOf", "oneOf"],
            "schemaCombinatorPolicy": "allOf may be empty; anyOf and oneOf must contain at least one schema and report schema_validation when empty",
            "additionalPropertiesPolicy": "false rejects every undeclared object key even when properties is omitted; object schemas validate undeclared keys as typed extras",
            "payloadNormalization": "arguments strings are parsed as JSON and validate_tool_call_payload rejects every executable payload that is not a JSON object, even when the target schema is empty",
            "errorKinds": ["tool_unavailable", "schema_validation", "approval_required"],
            "metadataStripping": format!("{PROVIDER_TOOL_CALL_META_KEY} is removed before validating against the target tool schema")
        },
        "validationPipeline": [
            {
                "stage": "definition_resolution",
                "entryPoint": "decision_parser::resolve_planner_tool_definition",
                "policy": "resolve the requested name against the same visible internal, MCP, and plugin tool definitions exposed to planner prompts",
                "errorKind": "tool_unavailable"
            },
            {
                "stage": "payload_schema_validation",
                "entryPoint": "decision_parser::validate_tool_call_payload",
                "policy": "strip provider metadata, reject non-object executable payloads, then validate against the resolved tool schema",
                "errorKind": "schema_validation"
            },
            {
                "stage": "bridge_target_validation",
                "entryPoint": "decision_parser::validate_tool_call_bridge_target",
                "policy": "tool_call targets must resolve, avoid recursive bridge tools, pass the target schema, and use the normal executor approval route for approval-required or risky internal targets",
                "errorKind": "approval_required"
            },
            {
                "stage": "dispatch_boundary_revalidation",
                "entryPoint": "tool_dispatch::execute_recovery_internal_tool",
                "policy": "direct tool_call bridge dispatch repeats target schema, context, approval, and availability checks before internal or MCP execution",
                "errorKind": "schema_validation|approval_required|tool_unavailable"
            }
        ],
        "workflowGraphObservability": {
            "source": WORKFLOW_RUNTIME_SOURCE,
            "plannerDetailFields": ["toolProtocol", "tool_protocol", "toolOrigins", "tool_origins", "toolCallIds", "tool_call_ids", "toolCalls", "tool_calls"],
            "executorDetailFields": ["toolProtocol", "tool_protocol", "toolOrigins", "tool_origins", "toolCallIds", "tool_call_ids", "toolCalls", "tool_calls"],
            "transitionReason": WORKFLOW_REASON_TOOL_CALLS,
            "protocolValue": "canonical_tool_call_v1",
            "summaryPolicy": "workflow graph stores tool names, origins, provider call ids, and provider-native marker booleans; executable argument payloads remain in the validated planner requests and tool events."
        },
        "bridgeToolCall": {
            "name": "tool_call",
            "payloadShape": {"name": "target_tool", "arguments": "object|string"},
            "targetAliases": ["name", "tool"],
            "argumentAliases": ["arguments", "args", "payload", "input", "parameters"],
            "blockedTargets": ["tool_search", "tool_describe", "tool_call"],
            "targetValidation": "the bridge target must be available, must not require approval for direct bridge execution, internal risky targets must use the normal executor route, and its target payload must pass the same schema validator before execution",
            "directExecutionValidation": "tool_dispatch::execute_recovery_internal_tool validates resolved tool_call target payloads before direct internal or MCP dispatch",
            "directContextBoundary": "direct tool_call bridge execution checks tool_allowed_in_context before MCP dispatch",
            "directApprovalBoundary": format!("direct tool_call bridge execution rejects MCP targets whose ToolDefinition.requires_approval=true and internal targets whose resolved payload requires approval by tool_approval_reason; approved internal tool_call replay is the only exception and must come from the trusted executor approval-continuation context; {APPROVED_TOOL_CALL_REPLAY_KEY}=true is stripped as internal replay metadata before target schema validation, execution, trace payloads, and raw event payloads, and is never trusted as authorization when supplied by planner payload"),
            "workflowGraphStage": {
                "node": "executor",
                "status": "running",
                "stage": "tool_call_bridge_target",
                "detailFields": ["source", "directBridge", "direct_bridge", "requestedName", "requested_name", "serverId", "server_id", "toolName", "tool_name", "toolKind", "tool_kind", "sourceLabel", "source_label", "requiresApproval", "requires_approval", "approvedToolCallReplay", "approved_tool_call_replay", "bridgeStatus", "bridge_status", "bridgeRejectionReason", "bridge_rejection_reason"],
                "bridgeStatusValues": ["dispatch_ready", "approval_required", "context_blocked", "unavailable"],
                "completionCarryForward": ["directBridge", "direct_bridge", "requestedName", "requested_name", "serverId", "server_id", "toolName", "tool_name", "toolKind", "tool_kind", "sourceLabel", "source_label", "requiresApproval", "requires_approval", "approvedToolCallReplay", "approved_tool_call_replay", "bridgeStatus", "bridge_status", "bridgeRejectionReason", "bridge_rejection_reason", "bridgeStage", "bridge_stage", "lastBridgeTarget", "last_bridge_target"],
                "records": "resolved MCP targets, resolved internal targets, and unavailable bridge targets before direct dispatch or policy rejection; when executor completion or parallel batch completion overwrites the node, the latest bridge target after the latest workflow transition is carried forward so snapshots remain inspectable without leaking stale targets from prior cycles"
            },
            "approvedReplay": {
                "trustedContext": "ExecutorInternalToolExecutionContext.approved_tool_call_replay",
                "markerKey": APPROVED_TOOL_CALL_REPLAY_KEY,
                "scope": "internal tool_call approval replay only; direct MCP targets whose ToolDefinition.requires_approval=true remain blocked",
                "markerPolicy": "approval_gateway sets the marker only while replaying an approved internal tool_call; tool_dispatch strips it before target schema validation, target execution, trace payloads, and raw event payloads",
                "authorizationPolicy": "tool_dispatch trusts only the executor approval-continuation context flag; a planner-supplied marker is removed and never authorizes execution"
            },
            "riskPolicy": "tool_call delegates risk evaluation to the resolved target tool and payload"
        },
        "executionBoundary": {
            "internalTools": format!("tool_dispatch strips provider metadata and {APPROVED_TOOL_CALL_REPLAY_KEY} before direct internal execution"),
            "mcpTools": "tool_registry strips provider metadata before MCP execution",
            "approvals": "provider call ids remain available for approval/replay event correlation"
        },
        "purpose": "Document the single canonical tool-call protocol boundary shared by planner JSON, Hermes markup, provider-native tool calls, OpenAI function-call shapes, internal tool dispatch, MCP dispatch, approval replay, and the tool_call bridge."
    })
}
