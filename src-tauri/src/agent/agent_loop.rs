use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::{
    error::AppResult,
    model_catalog,
    models::{
        AgentCheckpointRecord, AgentDefinition, AgentQueuedRequest, AgentRunPhaseRecord,
        AgentRunRecord, ChatMessage, Conversation, LlmProvider, Persona, SendChatRequest,
    },
    skills as skill_library,
    store::AppStore,
};

use super::workflow_graph::{
    workflow_human_gate_detail, WorkflowPlannerNode, WORKFLOW_REASON_CLARIFY_REQUIRES_USER_INPUT,
};
use super::*;

// Tracks conversation IDs currently executing an inline WeChat queue drain.
// When set, the admission guard is skipped to avoid deadlocking on the same
// chat_turn_lock that the parent turn already holds.
static WECHAT_DRAIN_ACTIVE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

// Task-local flag set only inside drain_wechat_queue_this_turn's async scope.
// The admission-guard bypass requires BOTH the global flag AND this task-local
// to be true, ensuring that a concurrent external turn arriving while a drain
// is in progress cannot skip the lock — only the drain task itself can.
tokio::task_local! {
    static WECHAT_DRAIN_TASK_ACTIVE: bool;
}

fn mark_wechat_drain(conversation_id: &str) {
    WECHAT_DRAIN_ACTIVE
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap()
        .insert(conversation_id.to_string());
}

fn unmark_wechat_drain(conversation_id: &str) {
    if let Some(set) = WECHAT_DRAIN_ACTIVE.get() {
        set.lock().unwrap().remove(conversation_id);
    }
}

fn is_wechat_drain_active(conversation_id: &str) -> bool {
    WECHAT_DRAIN_ACTIVE
        .get()
        .map(|s| s.lock().unwrap().contains(conversation_id))
        .unwrap_or(false)
}

const PET_WINDOW_LABEL: &str = "pet";
const DESKTOP_STREAM_EVENT_MIN_INTERVAL: Duration = Duration::from_millis(150);
const DESKTOP_STREAM_EVENT_MIN_BYTES: usize = 96;

// Maximum per-conversation lock entries kept in memory. When this limit is
// reached the HashMap is pruned by evicting idle entries — those where no turn
// currently holds the Arc (strong_count == 1, only the map itself). This
// prevents unbounded growth across long-lived sessions without external crates.
const CHAT_TURN_LOCKS_MAX_CAPACITY: usize = 1024;

static CHAT_TURN_LOCKS: OnceLock<std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

fn emit_pet_event(app: &AppHandle, payload: Value) {
    let _ = app.emit("synthchat-pet-event", payload.clone());
    let _ = app.emit_to(PET_WINDOW_LABEL, "synthchat-pet-event", payload);
}

fn emit_assistant_stream_event(
    app: &AppHandle,
    conversation_id: &str,
    persona_id: Option<&str>,
    source: &str,
    message: &ChatMessage,
    delta: &str,
    is_final: bool,
    preview_chars: Option<usize>,
) {
    let event_message = crate::preview_message_for_ui(message.clone(), preview_chars);
    let _ = app.emit(
        "synthchat-chat-event",
        json!({
            "type": "assistant_stream",
            "source": source,
            "personaId": persona_id,
            "conversationId": conversation_id,
            "message": event_message,
            "delta": delta,
            "isLast": is_final,
        }),
    );
    let pet_message = crate::preview_message_for_ui(message.clone(), preview_chars);
    emit_pet_event(
        app,
        json!({
            "type": if is_final { "assistant_stream_done" } else { "assistant_stream_delta" },
            "source": source,
            "personaId": persona_id,
            "conversationId": conversation_id,
            "message": pet_message,
            "delta": delta,
            "isLast": is_final,
        }),
    );
}

fn stream_thinking_provider_data(summary: &str, streaming: bool) -> Value {
    json!({
        "thinkingCards": [{
            "provider": "llm",
            "kind": "thinking",
            "title": "模型思考",
            "summary": summary,
            "redacted": false,
            "streaming": streaming
        }]
    })
}

fn finalize_streaming_thinking_cards(provider_data: &mut Option<Value>) -> bool {
    let Some(root) = provider_data.as_mut() else {
        return false;
    };
    let mut changed = false;
    finalize_streaming_thinking_card_array(root.get_mut("thinkingCards"), &mut changed);
    if let Some(responses) = root.get_mut("responses").and_then(Value::as_object_mut) {
        finalize_streaming_thinking_card_array(responses.get_mut("thinkingCards"), &mut changed);
    }
    if let Some(anthropic) = root.get_mut("anthropic").and_then(Value::as_object_mut) {
        finalize_streaming_thinking_card_array(anthropic.get_mut("thinkingCards"), &mut changed);
    }
    changed
}

fn finalize_streaming_thinking_card_array(value: Option<&mut Value>, changed: &mut bool) {
    let Some(cards) = value.and_then(Value::as_array_mut) else {
        return;
    };
    for card in cards {
        let Some(card) = card.as_object_mut() else {
            continue;
        };
        if card
            .get("streaming")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            card.insert("streaming".into(), json!(false));
            *changed = true;
        }
    }
}

fn provider_data_has_thinking_cards(provider_data: &Option<Value>) -> bool {
    let Some(root) = provider_data.as_ref() else {
        return false;
    };
    [
        root.get("thinkingCards"),
        root.pointer("/responses/thinkingCards"),
        root.pointer("/anthropic/thinkingCards"),
    ]
    .into_iter()
    .flatten()
    .any(|value| value.as_array().is_some_and(|items| !items.is_empty()))
}

fn emit_streaming_thinking_finished(
    app: &AppHandle,
    message: &Arc<Mutex<ChatMessage>>,
    source: &str,
    persona_id: &str,
    conversation_id: &str,
    preview_chars: Option<usize>,
) {
    let message = {
        let mut message = message
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !finalize_streaming_thinking_cards(&mut message.provider_data) {
            return;
        }
        message.clone()
    };
    let message = crate::preview_message_for_ui(message, preview_chars);
    let _ = app.emit(
        "synthchat-chat-event",
        json!({
            "type": "assistant_thinking_stream",
            "source": source,
            "personaId": persona_id,
            "conversationId": conversation_id,
            "message": message,
            "delta": "",
            "isLast": true,
        }),
    );
}

fn append_pending_stream_delta(buffer: &Arc<Mutex<String>>, delta: &str) -> usize {
    let mut buffer = buffer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    buffer.push_str(delta);
    buffer.len()
}

fn take_pending_stream_delta(buffer: &Arc<Mutex<String>>) -> String {
    let mut buffer = buffer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::mem::take(&mut *buffer)
}

fn should_emit_desktop_stream_update(
    first_delta: bool,
    last_emit_at: &Arc<Mutex<Option<Instant>>>,
    pending_bytes: usize,
) -> bool {
    let now = Instant::now();
    let mut last_emit_at = last_emit_at
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let due = last_emit_at
        .map(|last| now.duration_since(last) >= DESKTOP_STREAM_EVENT_MIN_INTERVAL)
        .unwrap_or(true);
    if first_delta || pending_bytes >= DESKTOP_STREAM_EVENT_MIN_BYTES || due {
        *last_emit_at = Some(now);
        true
    } else {
        false
    }
}

#[derive(Clone)]
struct DesktopVisibleStreamState {
    message: Arc<Mutex<ChatMessage>>,
    parser: Arc<Mutex<PlannerVisibleContentStream>>,
    thinking_buffer: Arc<Mutex<String>>,
    pending_thinking_delta: Arc<Mutex<String>>,
    pending_answer_delta: Arc<Mutex<String>>,
    last_thinking_emit_at: Arc<Mutex<Option<Instant>>>,
    last_answer_emit_at: Arc<Mutex<Option<Instant>>>,
    emitted_any_delta: Arc<AtomicBool>,
    emitted_answer_text: Arc<AtomicBool>,
    conversation_id: String,
    source: String,
    preview_chars: Option<usize>,
}

impl DesktopVisibleStreamState {
    fn new(conversation_id: &str, source: &str, preview_chars: Option<usize>) -> Self {
        Self {
            message: Arc::new(Mutex::new(ChatMessage::new(
                conversation_id.to_string(),
                "assistant",
                String::new(),
                source,
            ))),
            parser: Arc::new(Mutex::new(PlannerVisibleContentStream::default())),
            thinking_buffer: Arc::new(Mutex::new(String::new())),
            pending_thinking_delta: Arc::new(Mutex::new(String::new())),
            pending_answer_delta: Arc::new(Mutex::new(String::new())),
            last_thinking_emit_at: Arc::new(Mutex::new(None)),
            last_answer_emit_at: Arc::new(Mutex::new(None)),
            emitted_any_delta: Arc::new(AtomicBool::new(false)),
            emitted_answer_text: Arc::new(AtomicBool::new(false)),
            conversation_id: conversation_id.to_string(),
            source: source.to_string(),
            preview_chars,
        }
    }

    fn reset_for_next_llm_call(&self) {
        if !self.emitted_any_delta.swap(false, Ordering::SeqCst) {
            return;
        }
        self.emitted_answer_text.store(false, Ordering::SeqCst);
        if let Ok(mut message) = self.message.lock() {
            *message = ChatMessage::new(
                self.conversation_id.clone(),
                "assistant",
                String::new(),
                &self.source,
            );
        }
        if let Ok(mut parser) = self.parser.lock() {
            *parser = PlannerVisibleContentStream::default();
        }
        if let Ok(mut buffer) = self.thinking_buffer.lock() {
            buffer.clear();
        }
        if let Ok(mut buffer) = self.pending_thinking_delta.lock() {
            buffer.clear();
        }
        if let Ok(mut buffer) = self.pending_answer_delta.lock() {
            buffer.clear();
        }
        if let Ok(mut timestamp) = self.last_thinking_emit_at.lock() {
            *timestamp = None;
        }
        if let Ok(mut timestamp) = self.last_answer_emit_at.lock() {
            *timestamp = None;
        }
    }

    fn emit_provider_thinking_if_idle(
        &self,
        app: Option<&AppHandle>,
        persona_id: &str,
        provider_data: &Option<Value>,
    ) {
        if self.emitted_any_delta.load(Ordering::SeqCst)
            || !provider_data_has_thinking_cards(provider_data)
        {
            return;
        }
        let Some(app) = app else {
            return;
        };
        self.emitted_any_delta.store(true, Ordering::SeqCst);
        let message = {
            let mut message = self
                .message
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            message.created_at = now_iso();
            message.provider_data = provider_data.clone();
            message.clone()
        };
        let message = crate::preview_message_for_ui(message, self.preview_chars);
        let _ = app.emit(
            "synthchat-chat-event",
            json!({
                "type": "assistant_thinking_stream",
                "source": &self.source,
                "personaId": persona_id,
                "conversationId": &self.conversation_id,
                "message": message,
                "delta": "",
                "isLast": false,
            }),
        );
    }

    fn finish_thinking_segment(&self, app: Option<&AppHandle>, persona_id: &str) {
        let Some(app) = app else {
            return;
        };
        emit_streaming_thinking_finished(
            app,
            &self.message,
            &self.source,
            persona_id,
            &self.conversation_id,
            self.preview_chars,
        );
    }
}

fn desktop_visible_stream_callback(
    existing: Option<crate::llm::LlmDeltaCallback>,
    app: Option<&AppHandle>,
    conversation_id: &str,
    persona_id: &str,
    source: &str,
    emit_pet_events: bool,
    preview_chars: Option<usize>,
) -> (
    Option<crate::llm::LlmDeltaCallback>,
    DesktopVisibleStreamState,
) {
    let stream_state = DesktopVisibleStreamState::new(conversation_id, source, preview_chars);
    let Some(app) = app.cloned() else {
        return (existing, stream_state);
    };
    let callback_message = Arc::clone(&stream_state.message);
    let callback_started = Arc::clone(&stream_state.emitted_any_delta);
    let callback_answer_started = Arc::clone(&stream_state.emitted_answer_text);
    let callback_existing = existing.clone();
    let callback_parser = Arc::clone(&stream_state.parser);
    let callback_thinking_buffer = Arc::clone(&stream_state.thinking_buffer);
    let callback_pending_thinking_delta = Arc::clone(&stream_state.pending_thinking_delta);
    let callback_pending_answer_delta = Arc::clone(&stream_state.pending_answer_delta);
    let callback_last_thinking_emit_at = Arc::clone(&stream_state.last_thinking_emit_at);
    let callback_last_answer_emit_at = Arc::clone(&stream_state.last_answer_emit_at);
    let conversation_id = conversation_id.to_string();
    let persona_id = persona_id.to_string();
    let source = source.to_string();
    let callback: crate::llm::LlmDeltaCallback = Arc::new(move |kind, delta| {
        if let Some(existing) = callback_existing.as_ref() {
            existing(kind, delta)?;
        }
        if kind == crate::llm::LlmStreamDeltaKind::Thinking {
            if delta.is_empty() {
                return Ok(());
            }
            let summary = {
                let mut buffer = callback_thinking_buffer
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                buffer.push_str(delta);
                buffer.clone()
            };
            let is_first_visible_delta = !callback_started.swap(true, Ordering::SeqCst);
            let message = {
                let mut message = callback_message
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if is_first_visible_delta {
                    message.created_at = now_iso();
                }
                message.provider_data = Some(stream_thinking_provider_data(&summary, true));
                message.clone()
            };
            let pending_bytes =
                append_pending_stream_delta(&callback_pending_thinking_delta, delta);
            if !should_emit_desktop_stream_update(
                is_first_visible_delta,
                &callback_last_thinking_emit_at,
                pending_bytes,
            ) {
                return Ok(());
            }
            let event_delta = take_pending_stream_delta(&callback_pending_thinking_delta);
            let message = crate::preview_message_for_ui(message, preview_chars);
            let _ = app.emit(
                "synthchat-chat-event",
                json!({
                    "type": "assistant_thinking_stream",
                    "source": source,
                    "personaId": persona_id,
                    "conversationId": conversation_id,
                    "message": message,
                    "delta": event_delta,
                    "isLast": false,
                }),
            );
            return Ok(());
        }
        emit_streaming_thinking_finished(
            &app,
            &callback_message,
            &source,
            &persona_id,
            &conversation_id,
            preview_chars,
        );
        let visible_delta = callback_parser
            .lock()
            .map(|mut parser| parser.push(delta))
            .unwrap_or_default();
        if visible_delta.is_empty() {
            return Ok(());
        }
        let is_first_visible_delta = !callback_started.swap(true, Ordering::SeqCst);
        callback_answer_started.store(true, Ordering::SeqCst);
        let message = {
            let mut message = callback_message
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if is_first_visible_delta {
                message.created_at = now_iso();
            }
            message.content.push_str(&visible_delta);
            message.clone()
        };
        let pending_bytes =
            append_pending_stream_delta(&callback_pending_answer_delta, &visible_delta);
        if !should_emit_desktop_stream_update(
            is_first_visible_delta,
            &callback_last_answer_emit_at,
            pending_bytes,
        ) {
            return Ok(());
        }
        let event_delta = take_pending_stream_delta(&callback_pending_answer_delta);
        let event_message = crate::preview_message_for_ui(message.clone(), preview_chars);
        let pet_delta = event_delta.clone();
        let _ = app.emit(
            "synthchat-chat-event",
            json!({
                "type": "assistant_stream",
                "source": source,
                "personaId": persona_id,
                "conversationId": conversation_id,
                "message": event_message,
                "delta": event_delta,
                "isLast": false,
            }),
        );
        if emit_pet_events {
            let pet_message = crate::preview_message_for_ui(message, preview_chars);
            emit_pet_event(
                &app,
                json!({
                    "type": "assistant_stream_delta",
                    "source": source,
                    "personaId": persona_id,
                    "conversationId": conversation_id,
                    "message": pet_message,
                    "delta": pet_delta,
                    "isLast": false,
                }),
            );
        }
        Ok(())
    });
    (Some(callback), stream_state)
}

#[derive(Debug, Default)]
struct PlannerVisibleContentStream {
    raw: String,
    emitted: String,
    completed_non_visible: bool,
}

impl PlannerVisibleContentStream {
    fn push(&mut self, delta: &str) -> String {
        if delta.is_empty() {
            return String::new();
        }
        if self.completed_non_visible {
            if delta.trim_start().is_empty() {
                return String::new();
            }
            self.raw.clear();
            self.emitted.clear();
            self.completed_non_visible = false;
        }
        self.raw.push_str(delta);
        if let Ok(value) = serde_json::from_str::<Value>(self.raw.trim_start()) {
            self.completed_non_visible = final_content_from_planner_value(&value).is_none();
        }
        let visible = planner_visible_final_content_prefix(&self.raw);
        if visible.len() <= self.emitted.len() || !visible.is_char_boundary(self.emitted.len()) {
            return String::new();
        }
        let next = visible[self.emitted.len()..].to_string();
        self.emitted = visible;
        next
    }
}

fn planner_visible_final_content_prefix(raw: &str) -> String {
    let trimmed_start = raw.trim_start();
    if !trimmed_start.starts_with('{') {
        return raw.to_string();
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed_start) {
        return final_content_from_planner_value(&value).unwrap_or_default();
    }
    extract_partial_final_content_from_json(trimmed_start).unwrap_or_default()
}

fn final_content_from_planner_value(value: &Value) -> Option<String> {
    if planner_value_looks_like_tool_call(value) {
        return None;
    }
    let action = value
        .get("action")
        .or_else(|| value.get("type"))
        .or_else(|| value.get("decision"))
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(
        action.as_str(),
        "tool" | "use_tool" | "call_tool" | "tools" | "tool_call"
    ) {
        return None;
    }
    value
        .get("content")
        .or_else(|| value.get("answer"))
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn planner_value_looks_like_tool_call(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.get("toolCalls").and_then(Value::as_array).is_some()
        || object.get("tool_calls").and_then(Value::as_array).is_some()
        || object
            .get("function_calls")
            .and_then(Value::as_array)
            .is_some()
    {
        return true;
    }
    if object
        .get("function")
        .or_else(|| object.get("function_call"))
        .and_then(Value::as_object)
        .and_then(|function| function.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|name| !name.is_empty())
    {
        return true;
    }
    let has_name = object
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|name| !name.is_empty());
    let has_arguments = ["arguments", "args", "payload", "input", "parameters"]
        .iter()
        .any(|key| object.get(*key).is_some());
    has_name && has_arguments
}

fn extract_partial_final_content_from_json(raw: &str) -> Option<String> {
    if !raw_contains_final_action(raw) || raw_contains_tool_action(raw) {
        return None;
    }
    for key in ["content", "answer", "message"] {
        if let Some(value) = extract_partial_json_string_value(raw, key) {
            return Some(value);
        }
    }
    None
}

fn raw_contains_final_action(raw: &str) -> bool {
    let Some(action) = extract_partial_json_string_value(raw, "action") else {
        return false;
    };
    matches!(
        action.trim().to_ascii_lowercase().as_str(),
        "final" | "answer" | "respond" | "finish" | "done"
    )
}

fn raw_contains_tool_action(raw: &str) -> bool {
    let Some(action) = extract_partial_json_string_value(raw, "action") else {
        return false;
    };
    matches!(
        action.trim().to_ascii_lowercase().as_str(),
        "tool" | "use_tool" | "call_tool" | "tools" | "tool_call"
    )
}

fn extract_partial_json_string_value(raw: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let key_index = raw.find(&needle)?;
    let after_key = &raw[key_index + needle.len()..];
    let colon_index = after_key.find(':')?;
    let after_colon = after_key[colon_index + 1..].trim_start();
    let mut chars = after_colon.char_indices();
    let (_, quote) = chars.next()?;
    if quote != '"' {
        return None;
    }
    let mut value = String::new();
    let mut escaped = false;
    let mut iter = after_colon[quote.len_utf8()..].chars();
    while let Some(ch) = iter.next() {
        if escaped {
            match ch {
                '"' => value.push('"'),
                '\\' => value.push('\\'),
                '/' => value.push('/'),
                'b' => value.push('\u{0008}'),
                'f' => value.push('\u{000C}'),
                'n' => value.push('\n'),
                'r' => value.push('\r'),
                't' => value.push('\t'),
                'u' => {
                    let mut hex = String::new();
                    for _ in 0..4 {
                        if let Some(digit) = iter.next() {
                            hex.push(digit);
                        }
                    }
                    if hex.len() == 4 {
                        if let Ok(code) = u16::from_str_radix(&hex, 16) {
                            if let Some(decoded) = char::from_u32(code as u32) {
                                value.push(decoded);
                            }
                        }
                    }
                }
                other => value.push(other),
            }
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some(value),
            other => value.push(other),
        }
    }
    Some(value)
}

/// Resolve the user-facing source label for a chat turn from its request
/// `provider_data.source`, defaulting to "desktop". Mirrors the source logic
/// used when persisting the user message.
fn turn_source_label(request: &SendChatRequest) -> String {
    request
        .provider_data
        .as_ref()
        .and_then(|data| data.get("source"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|source| !source.is_empty())
        .unwrap_or("desktop")
        .to_string()
}

fn should_emit_pet_thinking_event(source: &str) -> bool {
    let source = source.trim();
    source != "pet-vision" && source != "proactive-internal"
}

fn pre_persisted_user_message_id(provider_data: Option<&Value>) -> Option<String> {
    provider_data
        .and_then(|data| {
            data.get("prePersistedUserMessageId")
                .or_else(|| data.get("pre_persisted_user_message_id"))
        })
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn pre_persisted_user_message(
    store: &AppStore,
    conversation_id: &str,
    content: &str,
    provider_data: Option<&Value>,
) -> AppResult<Option<ChatMessage>> {
    let Some(message_id) = pre_persisted_user_message_id(provider_data) else {
        return Ok(None);
    };
    let content = content.trim();
    Ok(store
        .messages(conversation_id, None)?
        .into_iter()
        .find(|message| {
            message.id == message_id && message.role == "user" && message.content.trim() == content
        }))
}

fn ensure_turn_user_message_persisted(
    store: &AppStore,
    conversation_id: &str,
    user: &ChatMessage,
) -> AppResult<()> {
    if user.role != "user" || user.source == "proactive-internal" {
        return Ok(());
    }
    let exists = store
        .messages(conversation_id, None)?
        .iter()
        .any(|message| message.id == user.id);
    if !exists {
        store.merge_conversation_messages_by_id(conversation_id, &[user.clone()])?;
    }
    Ok(())
}

async fn fail_chat_turn_before_llm(
    store: &AppStore,
    conversation: &Conversation,
    user: &ChatMessage,
    run_id: &str,
    workflow_planner: WorkflowPlannerNode,
    desktop_app: Option<&AppHandle>,
    silent_pet_vision: bool,
    error: &str,
    source: &str,
) -> AppResult<Vec<ChatMessage>> {
    workflow_planner.failed(
        store,
        run_id,
        None,
        WorkflowPlannerErrorKind::LlmError,
        error,
    )?;
    let mut failed_run = store.agent_run(run_id)?;
    failed_run.state = "failed".into();
    failed_run.error = Some(error.to_string());
    failed_run.updated_at = now_iso();
    failed_run.completed_at = Some(failed_run.updated_at.clone());
    let saved_failed_run = store.save_agent_run(failed_run)?;
    run_session_finished_hooks(
        store,
        &saved_failed_run,
        json!({
            "source": source,
            "error": error,
        }),
    )
    .await;

    let mut assistant_message = ChatMessage::new(
        conversation.id.clone(),
        "assistant",
        format!("本轮对话无法开始：{error}"),
        "desktop-agent-error",
    );
    if silent_pet_vision {
        assistant_message.source = "pet-vision".into();
        mark_message_visible_for_pet_vision(&mut assistant_message);
    }
    ensure_turn_user_message_persisted(store, &conversation.id, user)?;
    let assistant = store.append_message(assistant_message)?;
    emit_agent_run_record(desktop_app, &saved_failed_run, Some(&assistant));
    Ok(vec![user.clone(), assistant])
}

fn request_provider_data_bool(request: &SendChatRequest, key: &str) -> bool {
    request
        .provider_data
        .as_ref()
        .and_then(|data| data.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn is_pet_vision_request(request: &SendChatRequest) -> bool {
    turn_source_label(request) == "pet-vision"
}

fn is_pet_vision_silent_request(request: &SendChatRequest) -> bool {
    is_pet_vision_request(request) && request_provider_data_bool(request, "silent")
}

fn is_pet_vision_silent_provider_data(source: &str, provider_data: Option<&Value>) -> bool {
    let source = source.trim();
    let provider_source = provider_data
        .and_then(|data| data.get("source"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    let silent = provider_data
        .and_then(|data| data.get("silent"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    (source == "pet-vision" || provider_source == "pet-vision") && silent
}

fn chat_turn_lock(conversation_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    let locks = CHAT_TURN_LOCKS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut locks = locks.lock().unwrap_or_else(|p| p.into_inner());
    // Evict idle entries when at capacity. An entry is "idle" when its Arc
    // strong_count equals 1 — only the HashMap holds a reference, meaning no
    // active turn is waiting on or currently holding that lock. The entire
    // eviction + insert sequence runs under the std::sync::Mutex, so there is
    // no race between the count check and the removal.
    if locks.len() >= CHAT_TURN_LOCKS_MAX_CAPACITY {
        locks.retain(|_, arc| Arc::strong_count(arc) > 1);
    }
    locks
        .entry(conversation_id.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

pub(super) fn persona_tool_policy_max_iterations(persona: &Persona) -> Option<u32> {
    let value = persona
        .tool_policy
        .get("maxIterations")
        .or_else(|| persona.tool_policy.get("max_iterations"))?;
    if let Some(number) = value.as_u64() {
        return Some((number as u32).max(1).min(90));
    }
    if let Some(number) = value.as_f64().filter(|number| number.is_finite()) {
        return Some((number.round() as u32).max(1).min(90));
    }
    value
        .as_str()
        .and_then(|text| text.trim().parse::<u32>().ok())
        .map(|number| number.max(1).min(90))
}

pub(super) fn apply_persona_tool_policy_to_agent(persona: &Persona, agent: &mut AgentDefinition) {
    if let Some(max_iterations) = persona_tool_policy_max_iterations(persona) {
        agent.max_tool_iterations = max_iterations;
    }
}

fn is_pet_vision_silent_queue_item(item: &AgentQueuedRequest) -> bool {
    is_pet_vision_silent_provider_data(&item.source, item.provider_data.as_ref())
}

fn mark_message_visible_for_pet_vision(message: &mut ChatMessage) {
    let mut root = match message.provider_data.take() {
        Some(Value::Object(object)) => object,
        Some(value) => {
            let mut object = serde_json::Map::new();
            object.insert("originalProviderData".into(), value);
            object
        }
        None => serde_json::Map::new(),
    };
    root.insert("source".into(), Value::String("pet-vision".into()));
    root.insert("silent".into(), Value::Bool(false));
    root.insert("visibility".into(), Value::String("desktop-and-pet".into()));
    message.provider_data = Some(Value::Object(root));
}

#[derive(Debug, Clone)]
struct AttachmentMetadata {
    id: String,
    file_name: String,
    mime_type: String,
    path: String,
    file_size: Option<u64>,
}

fn attachment_mime_from_path(path: &str) -> String {
    match std::path::Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png".into(),
        Some("jpg" | "jpeg") => "image/jpeg".into(),
        Some("gif") => "image/gif".into(),
        Some("webp") => "image/webp".into(),
        Some("bmp") => "image/bmp".into(),
        Some("svg") => "image/svg+xml".into(),
        Some("pdf") => "application/pdf".into(),
        Some("txt" | "md" | "csv" | "log") => "text/plain".into(),
        Some("json") => "application/json".into(),
        _ => "application/octet-stream".into(),
    }
}

fn attachment_file_name(path: &str, fallback: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn attachment_record_to_metadata(value: &Value) -> Option<AttachmentMetadata> {
    if value
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| !matches!(kind, "attachment" | "image" | "file"))
    {
        return None;
    }
    let path = value
        .get("path")
        .or_else(|| value.get("filePath"))
        .or_else(|| value.get("file_path"))
        .or_else(|| value.get("localPath"))
        .or_else(|| value.get("local_path"))
        .or_else(|| value.get("sourcePath"))
        .or_else(|| value.get("source_path"))
        .or_else(|| value.get("tempPath"))
        .or_else(|| value.get("temp_path"))
        .or_else(|| value.get("thumbPath"))
        .or_else(|| value.get("thumb_path"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())?
        .to_string();
    let mime_type = value
        .get("mimeType")
        .or_else(|| value.get("mime_type"))
        .or_else(|| value.get("contentType"))
        .or_else(|| value.get("content_type"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|mime| !mime.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| attachment_mime_from_path(&path));
    let file_name = value
        .get("fileName")
        .or_else(|| value.get("file_name"))
        .or_else(|| value.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| attachment_file_name(&path, "attachment"));
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| file_name.clone());
    let file_size = value
        .get("fileSize")
        .or_else(|| value.get("file_size"))
        .or_else(|| value.get("size"))
        .and_then(Value::as_u64);
    Some(AttachmentMetadata {
        id,
        file_name,
        mime_type,
        path,
        file_size,
    })
}

fn collect_attachment_metadata(
    content: &str,
    provider_data: Option<&Value>,
) -> Vec<AttachmentMetadata> {
    let mut attachments = Vec::<AttachmentMetadata>::new();
    let mut seen = HashSet::<String>::new();
    let mut push = |attachment: AttachmentMetadata| {
        let key = format!(
            "{}::{}::{}",
            attachment.path, attachment.file_name, attachment.mime_type
        );
        if seen.insert(key) {
            attachments.push(attachment);
        }
    };
    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            if let Some(attachment) = attachment_metadata_from_media_marker(trimmed) {
                push(attachment);
            }
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            if let Some(attachment) = attachment_record_to_metadata(&value) {
                push(attachment);
            }
        }
    }
    if let Some(data) = provider_data {
        for key in [
            "attachments",
            "attachmentContexts",
            "attachment_contexts",
            "mediaFiles",
            "media_files",
        ] {
            match data.get(key) {
                Some(Value::Array(items)) => {
                    for item in items {
                        if let Some(attachment) = attachment_record_to_metadata(item) {
                            push(attachment);
                        }
                    }
                }
                Some(item) => {
                    if let Some(attachment) = attachment_record_to_metadata(item) {
                        push(attachment);
                    }
                }
                None => {}
            }
        }
    }
    attachments
}

fn attachment_metadata_is_image(attachment: &AttachmentMetadata) -> bool {
    let mime = attachment.mime_type.trim().to_ascii_lowercase();
    mime.starts_with("image/")
        || attachment_mime_from_path(&attachment.path)
            .to_ascii_lowercase()
            .starts_with("image/")
}

fn image_attachment_count_for_request(content: &str, provider_data: Option<&Value>) -> usize {
    collect_attachment_metadata(content, provider_data)
        .iter()
        .filter(|attachment| attachment_metadata_is_image(attachment))
        .count()
}

fn llm_candidates_support_image_input(providers: &[LlmProvider], persona: &Persona) -> bool {
    providers.iter().any(|provider| {
        let mut effective_provider = provider.clone();
        if !persona.llm_model.trim().is_empty() {
            effective_provider.model = persona.llm_model.trim().to_string();
        }
        model_catalog::provider_model_capabilities(&effective_provider).supports_vision
    })
}

fn image_input_preflight_blocker(
    store: &AppStore,
    content: &str,
    provider_data: Option<&Value>,
    providers: &[LlmProvider],
    persona: &Persona,
) -> AppResult<Option<(String, usize)>> {
    let image_count = image_attachment_count_for_request(content, provider_data);
    if image_count == 0 {
        return Ok(None);
    }
    if llm_candidates_support_image_input(providers, persona) {
        return Ok(None);
    }
    if store.enabled_vision_provider()?.is_some() {
        return Ok(None);
    }
    Ok(Some((
        "当前无法识图：图片已上传，但当前模型未声明视觉能力，且未配置视觉分析服务商。请启用视觉模型或配置 vision provider 后重试。"
            .into(),
        image_count,
    )))
}

fn attachment_metadata_from_media_marker(trimmed: &str) -> Option<AttachmentMetadata> {
    let rest = trimmed.strip_prefix("[media attached:")?;
    let (path, rest) = parse_media_attachment_path(rest.trim())?;
    let rest = rest.trim_start();
    let (mime_type, label) = if let Some(after_open) = rest.strip_prefix('(') {
        let (mime, after_close) = after_open.split_once(')')?;
        (mime.trim(), after_close.trim())
    } else {
        ("", rest)
    };
    let label = label
        .trim_start_matches(']')
        .trim_end_matches(']')
        .trim()
        .trim_matches('"')
        .trim_matches('`')
        .trim();
    let file_name = if label.is_empty() {
        attachment_file_name(&path, "attachment")
    } else {
        label.to_string()
    };
    let mime_type = if mime_type.is_empty() {
        attachment_mime_from_path(&path)
    } else {
        mime_type.to_string()
    };
    Some(AttachmentMetadata {
        id: file_name.clone(),
        file_name,
        mime_type,
        path,
        file_size: None,
    })
}

fn parse_media_attachment_path(value: &str) -> Option<(String, &str)> {
    let value = value.trim_start();
    let mut chars = value.chars();
    let first = chars.next()?;
    if matches!(first, '"' | '\'' | '`') {
        let end = value[first.len_utf8()..].find(first)? + first.len_utf8();
        let path = value[first.len_utf8()..end].trim().to_string();
        let rest = &value[end + first.len_utf8()..];
        return (!path.is_empty()).then_some((path, rest));
    }
    let end = value
        .find(" (")
        .or_else(|| value.find(']'))
        .unwrap_or(value.len());
    let path = value[..end].trim().to_string();
    let rest = &value[end..];
    (!path.is_empty()).then_some((path, rest))
}

fn attachment_identity(value: &Value) -> String {
    let path = value
        .get("path")
        .or_else(|| value.get("filePath"))
        .or_else(|| value.get("file_path"))
        .or_else(|| value.get("localPath"))
        .or_else(|| value.get("local_path"))
        .or_else(|| value.get("sourcePath"))
        .or_else(|| value.get("source_path"))
        .or_else(|| value.get("tempPath"))
        .or_else(|| value.get("temp_path"))
        .or_else(|| value.get("thumbPath"))
        .or_else(|| value.get("thumb_path"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let file_name = value
        .get("fileName")
        .or_else(|| value.get("file_name"))
        .or_else(|| value.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mime_type = value
        .get("mimeType")
        .or_else(|| value.get("mime_type"))
        .or_else(|| value.get("contentType"))
        .or_else(|| value.get("content_type"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    format!("{path}::{file_name}::{mime_type}")
}

pub(super) fn provider_data_with_attachment_metadata(
    content: &str,
    provider_data: Option<Value>,
) -> Option<Value> {
    let attachments = collect_attachment_metadata(content, provider_data.as_ref());
    if attachments.is_empty() {
        return provider_data;
    }
    let mut root = match provider_data {
        Some(Value::Object(object)) => object,
        Some(value) => {
            let mut object = serde_json::Map::new();
            object.insert("originalProviderData".into(), value);
            object
        }
        None => serde_json::Map::new(),
    };
    let mut values = root
        .get("attachments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut seen = values
        .iter()
        .map(attachment_identity)
        .collect::<HashSet<_>>();
    for attachment in attachments {
        let mut value = json!({
            "type": "attachment",
            "id": attachment.id,
            "path": attachment.path,
            "fileName": attachment.file_name,
            "mimeType": attachment.mime_type,
        });
        if let Some(file_size) = attachment.file_size {
            value["fileSize"] = json!(file_size);
        }
        if seen.insert(attachment_identity(&value)) {
            values.push(value);
        }
    }
    root.insert("attachments".into(), Value::Array(values));
    Some(Value::Object(root))
}

pub async fn run_chat_turn(
    store: &AppStore,
    request: SendChatRequest,
    app: Option<&AppHandle>,
) -> AppResult<Vec<ChatMessage>> {
    // The desktop window is the hub: every user-facing turn — desktop, pet,
    // wechat, proactive — flows through here. Emit a single authoritative pair
    // of lifecycle events (turn_started / turn_finished) so the frontend can
    // drive the "thinking" UI from one source of truth instead of inferring it
    // from a scatter of per-source events. turn_finished is guaranteed to fire
    // on every exit path (success, early return, or error).
    let conversation_id = request
        .conversation_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    let source = turn_source_label(&request);
    let persona_id = request.persona_id.clone();
    if let (Some(app), Some(conversation_id)) = (app, conversation_id.as_deref()) {
        let _ = app.emit(
            "synthchat-chat-event",
            json!({
                "type": "turn_started",
                "source": source,
                "personaId": persona_id,
                "conversationId": conversation_id,
            }),
        );
        if should_emit_pet_thinking_event(&source) {
            emit_pet_event(
                app,
                json!({
                    "type": "thinking_started",
                    "source": source,
                    "personaId": persona_id,
                    "conversationId": conversation_id,
                }),
            );
        }
    }

    let result =
        run_chat_turn_with_app(store, request, ToolExecutionContext::Interactive, app).await;

    if let Some(app) = app {
        // Prefer the conversation id reported by the result (covers the case
        // where the request had no id and the backend created one).
        let resolved_conversation_id = result
            .as_ref()
            .ok()
            .and_then(|messages| messages.first())
            .map(|message| message.conversation_id.clone())
            .or_else(|| conversation_id.clone());
        // Carry the final assistant message so listeners (e.g. the pet window)
        // can react immediately instead of polling. This makes the hub the
        // single, timely source of "the reply is ready" for every source.
        let assistant_message = result.as_ref().ok().and_then(|messages| {
            messages
                .iter()
                .rev()
                .find(|message| message.role == "assistant")
                .cloned()
        });
        let event_preview_chars = store
            .config()
            .ok()
            .map(|config| config.chat.ui_message_preview_chars);
        let assistant_message = assistant_message
            .map(|message| crate::preview_message_for_ui(message, event_preview_chars));
        if let Some(resolved_conversation_id) = resolved_conversation_id {
            let _ = app.emit(
                "synthchat-chat-event",
                json!({
                    "type": "turn_finished",
                    "source": source,
                    "personaId": persona_id,
                    "conversationId": resolved_conversation_id,
                    "ok": result.is_ok(),
                    "message": assistant_message,
                }),
            );
            if should_emit_pet_thinking_event(&source) {
                emit_pet_event(
                    app,
                    json!({
                        "type": "thinking_finished",
                        "source": source,
                        "personaId": persona_id,
                        "conversationId": resolved_conversation_id,
                        "ok": result.is_ok(),
                        "message": assistant_message,
                    }),
                );
            }
        }
    }

    result
}

pub(super) async fn run_chat_turn_in_context(
    store: &AppStore,
    request: SendChatRequest,
    tool_context: ToolExecutionContext,
) -> AppResult<Vec<ChatMessage>> {
    run_chat_turn_with_app(store, request, tool_context, None).await
}

pub(super) async fn run_chat_turn_with_app(
    store: &AppStore,
    request: SendChatRequest,
    tool_context: ToolExecutionContext,
    app: Option<&AppHandle>,
) -> AppResult<Vec<ChatMessage>> {
    run_chat_turn_with_toolset_policy(store, request, tool_context, None, None, app).await
}

pub(super) async fn run_chat_turn_with_toolset_policy(
    store: &AppStore,
    request: SendChatRequest,
    tool_context: ToolExecutionContext,
    enabled_toolsets: Option<Vec<String>>,
    disabled_toolsets: Option<Vec<String>>,
    app: Option<&AppHandle>,
) -> AppResult<Vec<ChatMessage>> {
    run_chat_turn_with_toolset_policy_and_iteration_limit(
        store,
        request,
        tool_context,
        enabled_toolsets,
        disabled_toolsets,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        app,
    )
    .await
}

pub(super) async fn run_chat_turn_with_toolset_policy_and_iteration_limit(
    store: &AppStore,
    request: SendChatRequest,
    tool_context: ToolExecutionContext,
    enabled_toolsets: Option<Vec<String>>,
    disabled_toolsets: Option<Vec<String>>,
    max_tool_iterations: Option<u32>,
    provider_id_override: Option<String>,
    model_override: Option<String>,
    base_url_override: Option<String>,
    timeout_seconds_override: Option<u64>,
    subagent_auto_approve: Option<bool>,
    workspace_dir_override: Option<String>,
    enabled_skills: Option<Vec<String>>,
    stream_delta_callback: Option<crate::llm::LlmDeltaCallback>,
    app: Option<&AppHandle>,
) -> AppResult<Vec<ChatMessage>> {
    let admission_guard = match request
        .conversation_id
        .as_deref()
        .map(str::trim)
        .filter(|conversation_id| !conversation_id.is_empty())
    {
        // Skip the lock only when this is the internal WeChat drain task AND
        // the global drain flag confirms the parent turn holds the lock. A
        // concurrent external turn sees is_drain_task=false even if the global
        // flag is set, so it always acquires the lock.
        Some(conversation_id)
            if WECHAT_DRAIN_TASK_ACTIVE.try_with(|v| *v).unwrap_or(false)
                && is_wechat_drain_active(conversation_id) =>
        {
            None
        }
        Some(conversation_id) => Some(chat_turn_lock(conversation_id).lock_owned().await),
        None => None,
    };
    let silent_pet_vision = is_pet_vision_silent_request(&request);
    let desktop_app = app;
    let conversation = match request.conversation_id.as_deref() {
        Some(id) if !id.trim().is_empty() => store.conversation(id)?,
        _ => store.create_conversation(None, request.persona_id.clone())?,
    };
    let (mut persona, mut agent) =
        resolve_chat_turn_persona_and_agent(store, &conversation, &request)?;
    let chat_config = store.config()?.chat;
    let event_preview_chars = Some(chat_config.ui_message_preview_chars);
    let request_source = turn_source_label(&request);
    let assistant_stream_source = if silent_pet_vision {
        "pet-vision"
    } else {
        "desktop-agent"
    };
    let emit_pet_stream_events = request_source != "wechat";
    let (stream_delta_callback, desktop_stream_state) = desktop_visible_stream_callback(
        stream_delta_callback,
        desktop_app,
        &conversation.id,
        &persona.id,
        assistant_stream_source,
        emit_pet_stream_events,
        event_preview_chars,
    );
    apply_persona_tool_policy_to_agent(&persona, &mut agent);
    apply_acp_session_mcp_scope(store, &conversation, &mut agent)?;
    if let Some(toolsets) = enabled_toolsets {
        agent.enabled_toolsets = toolsets;
    }
    if let Some(toolsets) = disabled_toolsets {
        merge_disabled_toolset_overrides(&mut agent.disabled_toolsets, toolsets);
    }
    if let Some(limit) = max_tool_iterations {
        agent.max_tool_iterations = limit.max(1).min(90);
    }
    if let Some(provider_id) = provider_id_override.filter(|value| !value.trim().is_empty()) {
        persona.llm_provider = provider_id;
    }
    if let Some(model) = model_override.filter(|value| !value.trim().is_empty()) {
        persona.llm_model = model;
    }
    if let Some(workspace_dir) = workspace_dir_override.filter(|value| !value.trim().is_empty()) {
        agent.workspace_dir = workspace_dir;
    }
    if let Some(skills) = enabled_skills {
        agent.enabled_skills = skills;
    }
    if let Some(control) =
        handle_agent_control_command(store, &conversation, &persona, &request.content, app).await?
    {
        let user = store.append_message(ChatMessage::new(
            conversation.id.clone(),
            "user",
            request.content.clone(),
            "desktop-control",
        ))?;
        let assistant = store.append_message(control)?;
        return Ok(vec![user, assistant]);
    }
    let direct_skill_invocation =
        build_direct_skill_slash_invocation_for_content(store, &conversation, &request.content)?;
    let effective_request_content = if let Some(invocation) = direct_skill_invocation {
        invocation.message
    } else {
        clarification_response_context_for_turn(store, &conversation.id, &request.content)?
            .unwrap_or_else(|| request.content.clone())
    };
    if silent_pet_vision
        && store
            .active_agent_run_for_conversation(&conversation.id)?
            .is_some()
    {
        return Ok(Vec::new());
    }
    if let Some(messages) =
        handle_busy_conversation_input_for_request(store, &conversation, &persona, &request, app)?
    {
        return Ok(messages);
    }
    let enriched_user_content = expand_context_references(
        &agent,
        &effective_request_content,
        chat_config.short_context_token_budget,
        Some(&store.data_dir().join("attachments")),
    )
    .await?;
    let pre_persisted_user = pre_persisted_user_message(
        store,
        &conversation.id,
        &request.content,
        request.provider_data.as_ref(),
    )?;
    let user_was_pre_persisted = pre_persisted_user.is_some();
    let user = if let Some(user) = pre_persisted_user {
        user
    } else {
        let mut user_message = ChatMessage::new(
            conversation.id.clone(),
            "user",
            request.content.clone(),
            request
                .provider_data
                .as_ref()
                .and_then(|data| data.get("source"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|source| !source.is_empty())
                .unwrap_or("desktop"),
        );
        user_message.provider_data =
            provider_data_with_attachment_metadata(&request.content, request.provider_data.clone());
        store.append_message(user_message)?
    };
    let silent_user_message = user.source == "proactive-internal" || silent_pet_vision;
    if let Some(app) = desktop_app.filter(|_| !silent_user_message && !user_was_pre_persisted) {
        let event_user = crate::preview_message_for_ui(user.clone(), event_preview_chars);
        let _ = app.emit(
            "synthchat-chat-event",
            json!({
                "type": "new_message",
                "source": user.source,
                "personaId": persona.id,
                "conversationId": conversation.id,
                "message": event_user,
                "isLast": false,
            }),
        );
    }

    let requested_api_run_id = request
        .provider_data
        .as_ref()
        .and_then(|data| data.get("apiServer").or_else(|| data.get("api_server")))
        .and_then(|api_server| {
            api_server
                .get("runId")
                .or_else(|| api_server.get("run_id"))
                .and_then(serde_json::Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if let Some(existing) = requested_api_run_id
        .as_deref()
        .and_then(|run_id| store.agent_run(run_id).ok())
        .filter(|run| matches!(run.state.as_str(), "completed" | "failed" | "aborted"))
    {
        return Err(AppError::BadRequest(format!(
            "agent run {} is already terminal: {}",
            existing.run_id, existing.state
        )));
    }
    let mut run = AgentRunRecord::new(
        conversation.id.clone(),
        persona.id.clone(),
        agent.id.clone(),
    );
    if let Some(requested_run_id) = requested_api_run_id {
        run.run_id = requested_run_id.to_string();
    }
    run.user_request = effective_request_content.clone();
    run.queue_item_id = request.queue_item_id.clone();
    let workflow_driver = WorkflowDriver::new(WorkflowMode::ChatTurn);
    let workflow_planner = workflow_driver.planner();
    let executor_core = ExecutorCore::new(workflow_driver.executor());
    let workflow_reviewer = workflow_driver.reviewer();
    workflow_driver.bootstrap(&mut run, &request, &request_source, tool_context);
    if silent_pet_vision {
        run.phase_events.push(AgentRunPhaseRecord {
            phase: "request_visibility".into(),
            detail: json!({
                "source": "pet-vision",
                "silentUserMessage": true,
                "visibility": "desktop-tools-and-assistant",
            }),
            updated_at: now_iso(),
        });
    }
    run.state = "running".into();
    let saved_run = store.save_agent_run(run.clone())?;
    drop(admission_guard);
    emit_agent_run_record(desktop_app, &saved_run, None);
    run_session_lifecycle_hooks(
        store,
        "on_session_start",
        &saved_run,
        json!({"source": "chat_turn"}),
    )
    .await;

    let mut history = store.messages(&conversation.id, Some(30))?;
    let mut effective_persona = effective_llm_persona(&persona, &agent);
    let selected_provider = match selected_provider_id(&persona, &agent) {
        Some(provider_id) => provider_id,
        None => {
            return fail_chat_turn_before_llm(
                store,
                &conversation,
                &user,
                &saved_run.run_id,
                workflow_planner,
                desktop_app,
                silent_pet_vision,
                "请先在通讯录中为当前角色选择对话服务商。",
                "llm_provider_missing",
            )
            .await;
        }
    };
    if effective_persona.llm_model.trim().is_empty() {
        return fail_chat_turn_before_llm(
            store,
            &conversation,
            &user,
            &saved_run.run_id,
            workflow_planner,
            desktop_app,
            silent_pet_vision,
            "请先在通讯录中为当前角色选择模型 ID。",
            "llm_model_missing",
        )
        .await;
    }
    let mut providers = store.provider_candidates(Some(selected_provider))?;
    if let Some(correction) =
        reconcile_model_family_provider(&mut effective_persona, &mut providers)
    {
        let detail = json!({
            "providerHint": correction.provider_hint,
            "fromProviderId": correction.from_provider_id,
            "toProviderId": correction.to_provider_id,
            "requestedModel": correction.requested_model,
            "effectiveModel": correction.effective_model,
        });
        run.phase_events.push(AgentRunPhaseRecord {
            phase: "llm_route_corrected".into(),
            detail: detail.clone(),
            updated_at: now_iso(),
        });
        run.updated_at = now_iso();
        append_parent_phase_event(store, &saved_run.run_id, "llm_route_corrected", detail)?;
    }
    if let Some(base_url) = base_url_override
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
    {
        let selected = if effective_persona.llm_provider.trim().is_empty() {
            None
        } else {
            Some(effective_persona.llm_provider.trim().to_string())
        };
        let provider_index = selected
            .as_deref()
            .and_then(|id| providers.iter().position(|provider| provider.id == id))
            .unwrap_or(0);
        if let Some(provider) = providers.get_mut(provider_index) {
            provider.base_url = base_url;
        }
    }
    if let Some((blocker, image_attachment_count)) = image_input_preflight_blocker(
        store,
        &request.content,
        request.provider_data.as_ref(),
        &providers,
        &effective_persona,
    )? {
        append_parent_phase_event(
            store,
            &saved_run.run_id,
            "completion_gate_blocked",
            json!({
                "reason": "image_input_without_vision_route",
                "imageAttachmentCount": image_attachment_count,
                "image_attachment_count": image_attachment_count,
            }),
        )?;
        let mut blocked_run = store.agent_run(&saved_run.run_id)?;
        blocked_run.state = "completed".into();
        blocked_run.touch_activity("completion gate blocked: image input without vision route");
        blocked_run.completed_at = Some(blocked_run.updated_at.clone());
        let saved_blocked_run = store.save_agent_run(blocked_run)?;
        run_session_finished_hooks(
            store,
            &saved_blocked_run,
            json!({"source": "completion_gate_blocked", "reason": "image_input_without_vision_route"}),
        )
        .await;
        let mut assistant_message = ChatMessage::new(
            conversation.id.clone(),
            "assistant",
            blocker,
            "desktop-agent",
        );
        if silent_pet_vision {
            assistant_message.source = "pet-vision".into();
            mark_message_visible_for_pet_vision(&mut assistant_message);
        }
        ensure_turn_user_message_persisted(store, &conversation.id, &user)?;
        let assistant = store.append_message(assistant_message)?;
        emit_agent_run_record(desktop_app, &saved_blocked_run, Some(&assistant));
        return Ok(vec![user, assistant]);
    }
    let mut observations = Vec::new();
    let mut assistant_text = String::new();
    let mut assistant_provider_data: Option<Value> = None;
    let mut assistant_model: Option<String> = None;
    let mut assistant_provider_id: Option<String> = None;
    let mut reviewer_skip_reason: Option<&'static str> = None;
    let mut planner_recovery_exhausted: Option<&'static str> = None;
    let mut assistant_prompt_tokens = 0usize;
    let skill_blocks =
        crate::skills::prompt_blocks_for_request(store, &agent, &effective_request_content)?;
    let memory_blocks =
        memory_prompt_blocks_for_query(store, &persona, &effective_request_content)?;
    let mut short_context = store.short_context(&conversation.id)?;
    let mcp_tools = available_mcp_tool_definitions(store, &agent)?;
    let visible_tools = visible_tool_definitions_for_agent(store, &agent, tool_context)?;
    let available_tools_for_validation = visible_tools.clone();
    let prompt_mcp_tools = visible_tools
        .iter()
        .filter(|tool| tool.source != "internal")
        .cloned()
        .collect::<Vec<_>>();
    on_memory_turn_start(
        store,
        &saved_run.run_id,
        &conversation.id,
        &persona,
        &effective_request_content,
        memory_blocks.len(),
        available_tools_for_validation.len(),
    )?;
    let run_started_at = Instant::now();
    let run_timeout_seconds =
        timeout_seconds_override.unwrap_or(chat_config.agent_run_timeout_seconds);
    let post_tool_quiet_timeout_seconds = chat_config.agent_post_tool_quiet_timeout_seconds;
    let mut tool_guardrails = ToolLoopGuardrails::new(&chat_config);
    let mut failed_file_mutations: HashMap<String, String> = HashMap::new();
    let mut llm_recoveries_attempted: HashSet<String> = HashSet::new();
    let mut empty_llm_recovery_attempts: HashMap<String, u32> = HashMap::new();
    let mut iteration_budget = IterationBudget::new(agent.max_tool_iterations.max(1).min(90));
    if chat_config.short_context_abort_on_summary_failure && short_context.last_compress_aborted {
        append_parent_phase_event(
            store,
            &saved_run.run_id,
            "context_compression_frozen",
            json!({
                "lastSummaryError": short_context.last_summary_error,
                "lastSummaryDroppedCount": short_context.last_summary_dropped_count,
                "summaryFailureCooldownUntilMs": short_context.summary_failure_cooldown_until_ms,
            }),
        )?;
        workflow_planner.failed(
            store,
            &saved_run.run_id,
            None,
            WorkflowPlannerErrorKind::ContextCompression,
            "Context compression is frozen after summary failure.",
        )?;
        run = store.agent_run(&saved_run.run_id)?;
        run.state = "failed".into();
        run.error = Some("Context compression is frozen after summary failure.".into());
        run.updated_at = now_iso();
        run.completed_at = Some(run.updated_at.clone());
        let saved_failed_run = store.save_agent_run(run)?;
        run_session_finished_hooks(
            store,
            &saved_failed_run,
            json!({"source": "context_compression_frozen"}),
        )
        .await;
        let mut assistant_message = ChatMessage::new(
            conversation.id.clone(),
            "assistant",
            format!(
                "本轮对话已暂停：上一次上下文压缩摘要失败，并且已开启 shortContextAbortOnSummaryFailure。为避免丢失旧对话历史，agent 不会继续运行。\n\n请修复摘要模型后执行 /compact，或在设置中关闭该开关后重试。上一错误：{}",
                short_context
                    .last_summary_error
                    .as_deref()
                    .unwrap_or("unknown summary error")
            ),
            "desktop-agent-error",
        );
        if silent_pet_vision {
            assistant_message.source = "pet-vision".into();
            mark_message_visible_for_pet_vision(&mut assistant_message);
        }
        ensure_turn_user_message_persisted(store, &conversation.id, &user)?;
        let assistant = store.append_message(assistant_message)?;
        emit_agent_run_record(desktop_app, &saved_failed_run, Some(&assistant));
        return Ok(vec![user, assistant]);
    }
    // Preflight compression is an optimisation step — a storage or DB error
    // must not abort the entire agent run. Degrade gracefully instead.
    match preflight_compact_context_for_agent_run(
        store,
        &saved_run.run_id,
        &conversation.id,
        &mut history,
        &mut short_context,
        &chat_config,
    ) {
        Ok(Some(note)) => observations.push(note),
        Ok(None) => {}
        Err(err) => {
            eprintln!("SynthChat: preflight compaction failed (skipping): {err}");
        }
    }
    for iteration in 0..iteration_budget.max_total() {
        if !iteration_budget.consume() {
            append_parent_phase_event(
                store,
                &saved_run.run_id,
                "iteration_budget_exhausted",
                json!({
                    "used": iteration_budget.used(),
                    "maxTotal": iteration_budget.max_total(),
                    "remaining": iteration_budget.remaining(),
                }),
            )?;
            break;
        }
        if check_agent_run_interrupted(
            store,
            &saved_run.run_id,
            run_started_at,
            run_timeout_seconds,
            post_tool_quiet_timeout_seconds,
            desktop_app,
        )? {
            return Ok(vec![user]);
        }
        drain_agent_steers_into_observations(store, &mut run, &mut observations)?;
        workflow_planner.running(store, &saved_run.run_id, iteration + 1)?;
        let prompt_observations = observations_for_prompt(store, &saved_run.run_id, &observations)?;
        let planner_prompt = agent_planner_prompt_for_agent_context_with_store(
            store,
            &prompt_observations,
            &skill_blocks,
            &memory_blocks,
            &short_context,
            &prompt_mcp_tools,
            tool_context,
            &agent,
            Some(&effective_persona),
        );
        let pre_llm_contexts =
            run_pre_llm_call_hooks(store, &saved_run.run_id, &enriched_user_content).await;
        let llm_user_content =
            inject_pre_llm_hook_context(&enriched_user_content, &pre_llm_contexts);
        desktop_stream_state.reset_for_next_llm_call();
        let reply_result = await_agent_run_interruptible(
            store,
            &saved_run.run_id,
            run_started_at,
            run_timeout_seconds,
            post_tool_quiet_timeout_seconds,
            desktop_app,
            complete_chat_with_provider_failover(
                store,
                Some(&saved_run.run_id),
                &providers,
                &effective_persona,
                planner_prompt.clone(),
                {
                    // Cap the in-memory history passed to the LLM to a rolling
                    // window.  The store-side short-context compaction keeps the
                    // persisted record small, but tool messages pushed in the
                    // loop would let the in-memory Vec grow without bound (up to
                    // 90 iterations × N tools per iteration).  Trimming here
                    // keeps each LLM request manageable while preserving the
                    // most recent context that the model actually needs.
                    const HISTORY_WINDOW: usize = 60;
                    if history.len() > HISTORY_WINDOW {
                        let mut cut = history.len() - HISTORY_WINDOW;
                        while cut < history.len() && history[cut].role == "tool" {
                            cut += 1;
                        }
                        history.drain(..cut);
                    }
                    // Strip base64 image payloads from older messages so every
                    // iteration does not redundantly re-send historical image data.
                    // strip_historical_media_payloads is only called on the
                    // compression path; applying it here prevents each turn from
                    // carrying large inline images that the model no longer needs.
                    let history_for_llm: Vec<_> = history.iter().map(|msg| {
                        let stripped = strip_historical_media_payloads(&msg.content);
                        if stripped == msg.content {
                            msg.clone()
                        } else {
                            crate::models::ChatMessage { content: stripped, ..msg.clone() }
                        }
                    }).collect();
                    history_for_llm
                },
                &llm_user_content,
                Some(&visible_tools),
                stream_delta_callback.clone(),
            ),
        )
        .await?;
        let Some(reply_result) = reply_result else {
            return Ok(vec![user]);
        };
        let reply = match reply_result {
            Ok(reply) => reply,
            Err(error) => {
                if check_agent_run_interrupted(
                    store,
                    &saved_run.run_id,
                    run_started_at,
                    run_timeout_seconds,
                    post_tool_quiet_timeout_seconds,
                    desktop_app,
                )? {
                    return Ok(vec![user]);
                }
                if let Some(recovery_note) = recover_llm_failure_for_agent_run(
                    store,
                    &saved_run.run_id,
                    &conversation.id,
                    &mut history,
                    &mut short_context,
                    &error,
                    &mut llm_recoveries_attempted,
                    chat_config.short_context_token_budget,
                )? {
                    observations.push(format!(
                        "Iteration {} LLM recovery: {}",
                        iteration + 1,
                        recovery_note
                    ));
                    continue;
                }
                let mut failed_run = store.agent_run(&saved_run.run_id)?;
                if failed_run.state != "aborted" {
                    workflow_planner.failed(
                        store,
                        &saved_run.run_id,
                        Some(iteration + 1),
                        WorkflowPlannerErrorKind::LlmError,
                        &error.to_string(),
                    )?;
                    failed_run = store.agent_run(&saved_run.run_id)?;
                    failed_run.state = "failed".into();
                    failed_run.error = Some(error.to_string());
                    failed_run.updated_at = now_iso();
                    failed_run.completed_at = Some(failed_run.updated_at.clone());
                    let saved_failed_run = store.save_agent_run(failed_run)?;
                    run_session_finished_hooks(
                        store,
                        &saved_failed_run,
                        json!({"source": "llm_error"}),
                    )
                    .await;
                    emit_agent_run_record(desktop_app, &saved_failed_run, None);
                }
                let mut assistant_message = ChatMessage::new(
                    conversation.id.clone(),
                    "assistant",
                    format!(
                        "本轮对话没有返回：模型请求失败。\n{}",
                        llm_route_summary(&providers, &effective_persona, &error.to_string())
                    ),
                    "desktop-agent-error",
                );
                if silent_pet_vision {
                    assistant_message.source = "pet-vision".into();
                    mark_message_visible_for_pet_vision(&mut assistant_message);
                }
                ensure_turn_user_message_persisted(store, &conversation.id, &user)?;
                let assistant = store.append_message(assistant_message)?;
                if let Ok(saved_failed_run) = store.agent_run(&saved_run.run_id) {
                    emit_agent_run_record(desktop_app, &saved_failed_run, Some(&assistant));
                }
                return Ok(vec![user, assistant]);
            }
        };
        desktop_stream_state.finish_thinking_segment(desktop_app, &persona.id);
        if abort_agent_run_for_turn_aborted_marker(
            store,
            &saved_run.run_id,
            &reply.content,
            desktop_app,
        )? {
            return Ok(vec![user]);
        }
        if check_agent_run_interrupted(
            store,
            &saved_run.run_id,
            run_started_at,
            run_timeout_seconds,
            post_tool_quiet_timeout_seconds,
            desktop_app,
        )? {
            return Ok(vec![user]);
        }
        if reply.finish_reason.as_deref() == Some("incomplete") {
            let recovery_key = "incomplete_response";
            if llm_recoveries_attempted.insert(recovery_key.into()) {
                let note = "Provider returned an incomplete Responses turn (reasoning/commentary without final answer, or unfinished output item). Continue from the current context and return a valid planner JSON object: either {\"action\":\"tool\",...} or {\"action\":\"final\",\"content\":\"...\"}.";
                observations.push(format!(
                    "Iteration {} LLM recovery: {}",
                    iteration + 1,
                    note
                ));
                append_parent_phase_event(
                    store,
                    &saved_run.run_id,
                    "llm_recovery",
                    json!({
                        "kind": recovery_key,
                        "note": note,
                    }),
                )?;
                continue;
            }
        }
        // Only attempt empty-response recovery when the provider did NOT signal an
        // incomplete turn. Incomplete turns are handled above by the
        // "incomplete_response" recovery path; triggering both on the same reply
        // would push conflicting guidance to the LLM and wastefully consume two
        // iteration budget slots without any real progress.
        if reply.content.trim().is_empty()
            && reply.finish_reason.as_deref() != Some("incomplete")
        {
            if let Some(recovery) =
                next_empty_llm_response_recovery(&observations, &mut empty_llm_recovery_attempts)
            {
                observations.push(format!(
                    "Iteration {} LLM recovery: {}",
                    iteration + 1,
                    recovery.note
                ));
                append_parent_phase_event(
                    store,
                    &saved_run.run_id,
                    "llm_recovery",
                    json!({
                        "kind": recovery.kind,
                        "note": recovery.note,
                        "attempt": recovery.attempt,
                        "maxAttempts": recovery.max_attempts,
                        "afterTools": recovery.after_tools,
                        "finishReason": reply.finish_reason.clone(),
                        "providerId": reply.provider_id.clone(),
                        "model": reply.model.clone(),
                    }),
                )?;
                continue;
            } else {
                planner_recovery_exhausted = Some(if observations.is_empty() {
                    "empty_response"
                } else {
                    "empty_response_after_tools"
                });
                append_parent_phase_event(
                    store,
                    &saved_run.run_id,
                    "llm_recovery_exhausted",
                    json!({
                        "kind": if observations.is_empty() { "empty_response" } else { "empty_response_after_tools" },
                        "attempts": empty_llm_recovery_attempts.clone(),
                        "finishReason": reply.finish_reason.clone(),
                        "providerId": reply.provider_id.clone(),
                        "model": reply.model.clone(),
                    }),
                )?;
                // Recovery exhausted and reply is empty — further iterations
                // would parse an empty response and make no progress, burning the
                // full iteration budget before failing.  Break immediately so the
                // post-loop error path can report LlmRecoveryExhausted.
                break;
            }
        }
        let decision = parse_agent_decision(&reply.content);
        append_planner_trace(
            store,
            &saved_run.run_id,
            &conversation.id,
            &persona.id,
            &agent.id,
            iteration + 1,
            &planner_prompt,
            &reply.content,
            &decision,
        )?;
        match workflow_planner.route(
            store,
            &saved_run.run_id,
            iteration + 1,
            &decision,
            reply.content.trim(),
            &available_tools_for_validation,
        )? {
            WorkflowPlannerRoute::ExecuteTools {
                mut requests,
                request_count,
            } => {
                // Cap tool calls per turn so a misbehaving or adversarial LLM
                // response cannot spawn dozens of parallel tool executions at once.
                const MAX_TOOL_CALLS_PER_TURN: usize = 20;
                if requests.len() > MAX_TOOL_CALLS_PER_TURN {
                    eprintln!(
                        "SynthChat: LLM returned {} tool calls; capping at {MAX_TOOL_CALLS_PER_TURN}",
                        requests.len()
                    );
                    requests.truncate(MAX_TOOL_CALLS_PER_TURN);
                }
                desktop_stream_state.emit_provider_thinking_if_idle(
                    desktop_app,
                    &persona.id,
                    &reply.provider_data,
                );
                let refund_iteration_for_execute_code_only =
                    tool_batch_is_execute_code_only(&requests);
                let should_parallelize = should_parallelize_tool_batch(
                    &requests,
                    &mcp_tools,
                    &agent,
                    &chat_config,
                    store,
                    tool_context,
                )?;
                if should_parallelize {
                    for (tool_name, payload) in &requests {
                        let guardrail_payload = payload.clone();
                        if let Some(outcome) =
                            tool_guardrails.before_call(tool_name, &guardrail_payload)
                        {
                            let guardrail_message = outcome.message.clone();
                            observations.push(format!(
                                "Iteration {} tool {} guardrail: {}",
                                iteration + 1,
                                tool_name,
                                guardrail_message
                            ));
                            if outcome.halt {
                                executor_core.record_tool_failed_with_iteration(
                                    store,
                                    desktop_app,
                                    &conversation.id,
                                    &saved_run.run_id,
                                    Some(iteration + 1),
                                    tool_name,
                                    &mcp_tools,
                                    &guardrail_payload,
                                    &AppError::BadRequest(guardrail_message.clone()),
                                )?;
                                assistant_text = guardrail_message;
                                break;
                            }
                        }
                    }
                }
                if !assistant_text.trim().is_empty() {
                    break;
                }
                if should_parallelize && assistant_text.trim().is_empty() {
                    let parallel_tool_names = requests
                        .iter()
                        .map(|(tool_name, _)| tool_name.clone())
                        .collect::<Vec<_>>();
                    executor_core.start_parallel_batch(
                        store,
                        &saved_run.run_id,
                        iteration + 1,
                        request_count,
                        &parallel_tool_names,
                    )?;
                    let parallel_results = await_agent_run_interruptible(
                        store,
                        &saved_run.run_id,
                        run_started_at,
                        run_timeout_seconds,
                        post_tool_quiet_timeout_seconds,
                        desktop_app,
                        execute_parallel_tool_batch(
                            store,
                            &agent,
                            &conversation.id,
                            &saved_run.run_id,
                            &requests,
                            &mcp_tools,
                            tool_context,
                            iteration + 1,
                            app,
                        ),
                    )
                    .await?;
                    let Some(parallel_results) = parallel_results else {
                        return Ok(vec![user]);
                    };
                    let mut parallel_succeeded = 0usize;
                    let mut parallel_failed = 0usize;
                    for (tool_name, payload, result) in parallel_results {
                        let guardrail_payload = payload.clone();
                        match result {
                            Ok((text, mut event)) => {
                                parallel_succeeded += 1;
                                record_file_mutation_result(
                                    &mut failed_file_mutations,
                                    &tool_name,
                                    &payload,
                                    &text,
                                    false,
                                );
                                if check_agent_run_interrupted(
                                    store,
                                    &saved_run.run_id,
                                    run_started_at,
                                    run_timeout_seconds,
                                    post_tool_quiet_timeout_seconds,
                                    desktop_app,
                                )? {
                                    return Ok(vec![user]);
                                }
                                let context_text = persist_large_tool_result_for_context(
                                    store,
                                    &saved_run.run_id,
                                    &tool_name,
                                    &text,
                                    &mut event,
                                )?;
                                let mut tool_message = ChatMessage::new(
                                    conversation.id.clone(),
                                    "tool",
                                    json!({"type": "toolEvent", "event": event.clone()})
                                        .to_string(),
                                    "desktop-agent-tool",
                                );
                                tool_message.provider_data = reply.provider_data.clone();
                                let _tool_message = store.append_message(tool_message)?;
                                history.push(_tool_message);
                                push_tool_event_record(&mut run, &event);
                                if let Some(assistant) = pause_run_for_clarify_tool(
                                    store,
                                    app,
                                    &mut run,
                                    &conversation.id,
                                    WorkflowMode::ChatTurn,
                                    &text,
                                    &event,
                                )? {
                                    return Ok(vec![user, assistant]);
                                }
                                let saved_tool_run = store.save_agent_run(run.clone())?;
                                run = saved_tool_run.clone();
                                emit_agent_run_record(desktop_app, &run, None);
                                let observation_text = append_subdirectory_hints_to_tool_result(
                                    &agent,
                                    &tool_name,
                                    &payload,
                                    &context_text,
                                );
                                observations.push(tool_result_replay_observation(
                                    iteration + 1,
                                    &tool_name,
                                    &tool_name,
                                    &observation_text,
                                ));
                                if let Some(outcome) = tool_guardrails.after_call(
                                    &tool_name,
                                    &guardrail_payload,
                                    &context_text,
                                    false,
                                ) {
                                    observations.push(format!(
                                        "Iteration {} tool {} guardrail: {}",
                                        iteration + 1,
                                        tool_name,
                                        outcome.message
                                    ));
                                    if outcome.halt {
                                        assistant_text = outcome.message.clone();
                                        break;
                                    }
                                }
                            }
                            Err(error) => {
                                parallel_failed += 1;
                                executor_core.record_tool_failed_with_iteration(
                                    store,
                                    desktop_app,
                                    &conversation.id,
                                    &saved_run.run_id,
                                    Some(iteration + 1),
                                    &tool_name,
                                    &mcp_tools,
                                    &payload,
                                    &error,
                                )?;
                                record_file_mutation_result(
                                    &mut failed_file_mutations,
                                    &tool_name,
                                    &payload,
                                    &error.to_string(),
                                    true,
                                );
                                observations.push(format!(
                                    "Iteration {} tool {} error: {}",
                                    iteration + 1,
                                    tool_name,
                                    error
                                ));
                                if let Some(outcome) = tool_guardrails.after_call(
                                    &tool_name,
                                    &guardrail_payload,
                                    &error.to_string(),
                                    true,
                                ) {
                                    observations.push(format!(
                                        "Iteration {} tool {} guardrail: {}",
                                        iteration + 1,
                                        tool_name,
                                        outcome.message
                                    ));
                                    if outcome.halt {
                                        assistant_text = outcome.message.clone();
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    executor_core.complete_parallel_batch(
                        store,
                        &saved_run.run_id,
                        iteration + 1,
                        request_count,
                        &parallel_tool_names,
                        parallel_succeeded,
                        parallel_failed,
                        !assistant_text.trim().is_empty(),
                    )?;
                    if !assistant_text.trim().is_empty() {
                        break;
                    }
                    let executor_route = executor_core.continue_planning(
                        store,
                        &saved_run.run_id,
                        iteration + 1,
                        request_count,
                        Some(true),
                    )?;
                    debug_assert!(matches!(
                        executor_route,
                        WorkflowExecutorRoute::ContinuePlanning { .. }
                    ));
                    if refund_iteration_for_execute_code_only {
                        iteration_budget.refund();
                    }
                    continue;
                }
                for (tool_name, payload) in requests {
                    let guardrail_payload = payload.clone();
                    if let Some(outcome) =
                        tool_guardrails.before_call(&tool_name, &guardrail_payload)
                    {
                        let guardrail_message = outcome.message.clone();
                        observations.push(format!(
                            "Iteration {} tool {} guardrail: {}",
                            iteration + 1,
                            tool_name,
                            guardrail_message
                        ));
                        if outcome.halt {
                            executor_core.record_tool_failed_with_iteration(
                                store,
                                desktop_app,
                                &conversation.id,
                                &saved_run.run_id,
                                Some(iteration + 1),
                                &tool_name,
                                &mcp_tools,
                                &guardrail_payload,
                                &AppError::BadRequest(guardrail_message.clone()),
                            )?;
                            assistant_text = guardrail_message;
                            break;
                        }
                    }
                    let tool_resolution = executor_core.resolve_tool(
                        store,
                        &saved_run.run_id,
                        iteration + 1,
                        &tool_name,
                        &mcp_tools,
                    )?;
                    run = store.agent_run(&saved_run.run_id)?;
                    match tool_resolution {
                        WorkflowExecutorToolResolution::Internal(tool_identity) => {
                            let approval_reason = match executor_core
                                .resolve_approval_policy(
                                    store,
                                    ExecutorApprovalPolicyContext::chat_turn(
                                        &saved_run.run_id,
                                        &providers,
                                        &effective_persona,
                                        tool_context,
                                        subagent_auto_approve,
                                    ),
                                    iteration + 1,
                                    &tool_identity,
                                    &tool_name,
                                    &payload,
                                    is_risky_tool_call(&tool_name, &payload),
                                )
                                .await
                            {
                                Ok(reason) => reason,
                                Err(error) => {
                                    executor_core.record_tool_failed_with_iteration(
                                        store,
                                        desktop_app,
                                        &conversation.id,
                                        &saved_run.run_id,
                                        Some(iteration + 1),
                                        &tool_name,
                                        &mcp_tools,
                                        &guardrail_payload,
                                        &error,
                                    )?;
                                    observations.push(format!(
                                        "Iteration {} tool {} approval error: {}",
                                        iteration + 1,
                                        tool_name,
                                        error
                                    ));
                                    continue;
                                }
                            };
                            if let Some(reason) = approval_reason {
                                let approval_request = executor_core
                                    .request_approval(
                                        store,
                                        ExecutorApprovalRequestContext {
                                            conversation_id: &conversation.id,
                                            persona_id: &persona.id,
                                            agent_id: &agent.id,
                                            run_id: &saved_run.run_id,
                                            tool_context,
                                        },
                                        iteration + 1,
                                        &tool_identity,
                                        payload,
                                        reason,
                                    )
                                    .await?;
                                let approval_route = approval_request.route;
                                let approval = approval_request.approval;
                                debug_assert!(matches!(
                                    approval_route,
                                    WorkflowExecutorRoute::AwaitApproval { .. }
                                ));
                                run = store.agent_run(&saved_run.run_id)?;
                                run.state = "pendingApproval".into();
                                run.updated_at = now_iso();
                                let saved_pending_run = store.save_agent_run(run)?;
                                emit_agent_run_record(desktop_app, &saved_pending_run, None);
                                let mut assistant_message = ChatMessage::new(
                                    conversation.id,
                                    "assistant",
                                    format!(
                                        "工具调用正在等待审批：{} · {}",
                                        approval.server_id, approval.tool_name
                                    ),
                                    "desktop-agent",
                                );
                                if silent_pet_vision {
                                    assistant_message.source = "pet-vision".into();
                                    mark_message_visible_for_pet_vision(&mut assistant_message);
                                }
                                let assistant = store.append_message(assistant_message)?;
                                return Ok(vec![user, assistant]);
                            }
                            run = executor_core.start_tool_execution(
                                store,
                                desktop_app,
                                &saved_run.run_id,
                                &tool_identity,
                                &payload,
                                iteration + 1,
                            )?;
                            emit_agent_run_record(desktop_app, &run, None);
                            let tool_result = await_agent_run_interruptible(
                                store,
                                &saved_run.run_id,
                                run_started_at,
                                run_timeout_seconds,
                                post_tool_quiet_timeout_seconds,
                                desktop_app,
                                executor_core.execute_internal_tool(
                                    store,
                                    ExecutorInternalToolExecutionContext {
                                        agent: &agent,
                                        conversation_id: &conversation.id,
                                        run_id: &saved_run.run_id,
                                        tool_context,
                                        app,
                                        approved_tool_call_replay: false,
                                    },
                                    &tool_name,
                                    payload,
                                ),
                            )
                            .await?;
                            let Some(tool_result) = tool_result else {
                                return Ok(vec![user]);
                            };
                            match tool_result {
                                Ok((text, mut event)) => {
                                    record_file_mutation_result(
                                        &mut failed_file_mutations,
                                        &tool_name,
                                        &guardrail_payload,
                                        &text,
                                        false,
                                    );
                                    if check_agent_run_interrupted(
                                        store,
                                        &saved_run.run_id,
                                        run_started_at,
                                        run_timeout_seconds,
                                        post_tool_quiet_timeout_seconds,
                                        desktop_app,
                                    )? {
                                        return Ok(vec![user]);
                                    }
                                    let context_text = persist_large_tool_result_for_context(
                                        store,
                                        &saved_run.run_id,
                                        &tool_name,
                                        &text,
                                        &mut event,
                                    )?;
                                    let mut tool_message = ChatMessage::new(
                                        conversation.id.clone(),
                                        "tool",
                                        json!({"type": "toolEvent", "event": event.clone()})
                                            .to_string(),
                                        "desktop-agent-tool",
                                    );
                                    tool_message.provider_data = reply.provider_data.clone();
                                    let _tool_message = store.append_message(tool_message)?;
                                    history.push(_tool_message);
                                    push_tool_event_record(&mut run, &event);
                                    if let Some(assistant) = pause_run_for_clarify_tool(
                                        store,
                                        app,
                                        &mut run,
                                        &conversation.id,
                                        WorkflowMode::ChatTurn,
                                        &text,
                                        &event,
                                    )? {
                                        return Ok(vec![user, assistant]);
                                    }
                                    let saved_tool_run = store.save_agent_run(run.clone())?;
                                    run = saved_tool_run.clone();
                                    emit_agent_run_record(desktop_app, &run, None);
                                    let observation_text = append_subdirectory_hints_to_tool_result(
                                        &agent,
                                        &tool_name,
                                        &guardrail_payload,
                                        &context_text,
                                    );
                                    observations.push(tool_result_replay_observation(
                                        iteration + 1,
                                        &tool_name,
                                        &tool_name,
                                        &observation_text,
                                    ));
                                    if let Some(outcome) = tool_guardrails.after_call(
                                        &tool_name,
                                        &guardrail_payload,
                                        &context_text,
                                        false,
                                    ) {
                                        observations.push(format!(
                                            "Iteration {} tool {} guardrail: {}",
                                            iteration + 1,
                                            tool_name,
                                            outcome.message
                                        ));
                                        if outcome.halt {
                                            assistant_text = outcome.message.clone();
                                            break;
                                        }
                                    }
                                }
                                Err(error) => {
                                    executor_core.record_tool_failed_with_iteration(
                                        store,
                                        desktop_app,
                                        &conversation.id,
                                        &saved_run.run_id,
                                        Some(iteration + 1),
                                        &tool_name,
                                        &mcp_tools,
                                        &guardrail_payload,
                                        &error,
                                    )?;
                                    record_file_mutation_result(
                                        &mut failed_file_mutations,
                                        &tool_name,
                                        &guardrail_payload,
                                        &error.to_string(),
                                        true,
                                    );
                                    observations.push(format!(
                                        "Iteration {} tool {} error: {}",
                                        iteration + 1,
                                        tool_name,
                                        error
                                    ));
                                    if let Some(outcome) = tool_guardrails.after_call(
                                        &tool_name,
                                        &guardrail_payload,
                                        &error.to_string(),
                                        true,
                                    ) {
                                        observations.push(format!(
                                            "Iteration {} tool {} guardrail: {}",
                                            iteration + 1,
                                            tool_name,
                                            outcome.message
                                        ));
                                        if outcome.halt {
                                            assistant_text = outcome.message.clone();
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        WorkflowExecutorToolResolution::Mcp {
                            identity: tool_identity,
                            definition,
                        } => {
                            let approval_reason = match executor_core
                                .resolve_approval_policy(
                                    store,
                                    ExecutorApprovalPolicyContext::chat_turn(
                                        &saved_run.run_id,
                                        &providers,
                                        &effective_persona,
                                        tool_context,
                                        subagent_auto_approve,
                                    ),
                                    iteration + 1,
                                    &tool_identity,
                                    &tool_name,
                                    &payload,
                                    definition.requires_approval,
                                )
                                .await
                            {
                                Ok(reason) => reason,
                                Err(error) => {
                                    executor_core.record_tool_failed_with_iteration(
                                        store,
                                        desktop_app,
                                        &conversation.id,
                                        &saved_run.run_id,
                                        Some(iteration + 1),
                                        &tool_name,
                                        &mcp_tools,
                                        &guardrail_payload,
                                        &error,
                                    )?;
                                    observations.push(format!(
                                        "Iteration {} tool {} approval error: {}",
                                        iteration + 1,
                                        tool_name,
                                        error
                                    ));
                                    continue;
                                }
                            };
                            if let Some(reason) = approval_reason {
                                let approval_request = executor_core
                                    .request_approval(
                                        store,
                                        ExecutorApprovalRequestContext {
                                            conversation_id: &conversation.id,
                                            persona_id: &persona.id,
                                            agent_id: &agent.id,
                                            run_id: &saved_run.run_id,
                                            tool_context,
                                        },
                                        iteration + 1,
                                        &tool_identity,
                                        payload,
                                        reason,
                                    )
                                    .await?;
                                let approval_route = approval_request.route;
                                let approval = approval_request.approval;
                                debug_assert!(matches!(
                                    approval_route,
                                    WorkflowExecutorRoute::AwaitApproval { .. }
                                ));
                                run = store.agent_run(&saved_run.run_id)?;
                                run.state = "pendingApproval".into();
                                run.updated_at = now_iso();
                                let saved_pending_run = store.save_agent_run(run)?;
                                emit_agent_run_record(desktop_app, &saved_pending_run, None);
                                let mut assistant_message = ChatMessage::new(
                                    conversation.id,
                                    "assistant",
                                    format!(
                                        "工具调用正在等待审批：{} · {}",
                                        approval.server_id, approval.tool_name
                                    ),
                                    "desktop-agent",
                                );
                                if silent_pet_vision {
                                    assistant_message.source = "pet-vision".into();
                                    mark_message_visible_for_pet_vision(&mut assistant_message);
                                }
                                let assistant = store.append_message(assistant_message)?;
                                return Ok(vec![user, assistant]);
                            }
                            run = executor_core.start_tool_execution(
                                store,
                                desktop_app,
                                &saved_run.run_id,
                                &tool_identity,
                                &payload,
                                iteration + 1,
                            )?;
                            emit_agent_run_record(desktop_app, &run, None);
                            let tool_result = await_agent_run_interruptible(
                                store,
                                &saved_run.run_id,
                                run_started_at,
                                run_timeout_seconds,
                                post_tool_quiet_timeout_seconds,
                                desktop_app,
                                executor_core.execute_mcp_tool(
                                    store,
                                    &saved_run.run_id,
                                    &definition,
                                    payload,
                                    Some(&PythonPluginBridgeContext {
                                        agent: &agent,
                                        conversation_id: &conversation.id,
                                        run_id: &saved_run.run_id,
                                        tool_context,
                                        app,
                                        allow_mutating_tools: true,
                                    }),
                                ),
                            )
                            .await?;
                            let Some(tool_result) = tool_result else {
                                return Ok(vec![user]);
                            };
                            match tool_result {
                                Ok((text, mut event)) => {
                                    record_file_mutation_result(
                                        &mut failed_file_mutations,
                                        &tool_name,
                                        &guardrail_payload,
                                        &text,
                                        false,
                                    );
                                    if check_agent_run_interrupted(
                                        store,
                                        &saved_run.run_id,
                                        run_started_at,
                                        run_timeout_seconds,
                                        post_tool_quiet_timeout_seconds,
                                        desktop_app,
                                    )? {
                                        return Ok(vec![user]);
                                    }
                                    let context_text = persist_large_tool_result_for_context(
                                        store,
                                        &saved_run.run_id,
                                        &tool_name,
                                        &text,
                                        &mut event,
                                    )?;
                                    let mut tool_message = ChatMessage::new(
                                        conversation.id.clone(),
                                        "tool",
                                        json!({"type": "toolEvent", "event": event.clone()})
                                            .to_string(),
                                        "desktop-agent-tool",
                                    );
                                    tool_message.provider_data = reply.provider_data.clone();
                                    let _tool_message = store.append_message(tool_message)?;
                                    history.push(_tool_message);
                                    push_tool_event_record(&mut run, &event);
                                    if let Some(assistant) = pause_run_for_clarify_tool(
                                        store,
                                        app,
                                        &mut run,
                                        &conversation.id,
                                        WorkflowMode::ChatTurn,
                                        &text,
                                        &event,
                                    )? {
                                        return Ok(vec![user, assistant]);
                                    }
                                    let saved_tool_run = store.save_agent_run(run.clone())?;
                                    run = saved_tool_run.clone();
                                    emit_agent_run_record(desktop_app, &run, None);
                                    let tool_source = tool_identity.source_label();
                                    let observation_text = append_subdirectory_hints_to_tool_result(
                                        &agent,
                                        &tool_name,
                                        &guardrail_payload,
                                        &context_text,
                                    );
                                    observations.push(tool_result_replay_observation(
                                        iteration + 1,
                                        &tool_name,
                                        &tool_source,
                                        &observation_text,
                                    ));
                                    if let Some(outcome) = tool_guardrails.after_call(
                                        &tool_name,
                                        &guardrail_payload,
                                        &context_text,
                                        false,
                                    ) {
                                        observations.push(format!(
                                            "Iteration {} tool {} guardrail: {}",
                                            iteration + 1,
                                            tool_name,
                                            outcome.message
                                        ));
                                        if outcome.halt {
                                            assistant_text = outcome.message.clone();
                                            break;
                                        }
                                    }
                                }
                                Err(error) => {
                                    executor_core.record_tool_failed_with_iteration(
                                        store,
                                        desktop_app,
                                        &conversation.id,
                                        &saved_run.run_id,
                                        Some(iteration + 1),
                                        &tool_name,
                                        &mcp_tools,
                                        &guardrail_payload,
                                        &error,
                                    )?;
                                    record_file_mutation_result(
                                        &mut failed_file_mutations,
                                        &tool_name,
                                        &guardrail_payload,
                                        &error.to_string(),
                                        true,
                                    );
                                    observations.push(format!(
                                        "Iteration {} tool {} error: {}",
                                        iteration + 1,
                                        tool_name,
                                        error
                                    ));
                                    if let Some(outcome) = tool_guardrails.after_call(
                                        &tool_name,
                                        &guardrail_payload,
                                        &error.to_string(),
                                        true,
                                    ) {
                                        observations.push(format!(
                                            "Iteration {} tool {} guardrail: {}",
                                            iteration + 1,
                                            tool_name,
                                            outcome.message
                                        ));
                                        if outcome.halt {
                                            assistant_text = outcome.message.clone();
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        WorkflowExecutorToolResolution::Unavailable { requested_name } => {
                            let error = AppError::BadRequest(format!(
                                "tool is not available: {requested_name}"
                            ));
                            executor_core.record_tool_failed_with_iteration(
                                store,
                                desktop_app,
                                &conversation.id,
                                &saved_run.run_id,
                                Some(iteration + 1),
                                &tool_name,
                                &mcp_tools,
                                &guardrail_payload,
                                &error,
                            )?;
                            record_file_mutation_result(
                                &mut failed_file_mutations,
                                &tool_name,
                                &guardrail_payload,
                                &error.to_string(),
                                true,
                            );
                            observations.push(format!(
                                "Iteration {} tool {} error: {}",
                                iteration + 1,
                                tool_name,
                                error
                            ));
                            if let Some(outcome) = tool_guardrails.after_call(
                                &tool_name,
                                &guardrail_payload,
                                &error.to_string(),
                                true,
                            ) {
                                observations.push(format!(
                                    "Iteration {} tool {} guardrail: {}",
                                    iteration + 1,
                                    tool_name,
                                    outcome.message
                                ));
                                if outcome.halt {
                                    assistant_text = outcome.message.clone();
                                    break;
                                }
                            }
                        }
                    }
                    if !assistant_text.trim().is_empty() {
                        break;
                    }
                }
                if !assistant_text.trim().is_empty() {
                    break;
                }
                let executor_route = executor_core.continue_planning(
                    store,
                    &saved_run.run_id,
                    iteration + 1,
                    request_count,
                    Some(false),
                )?;
                debug_assert!(matches!(
                    executor_route,
                    WorkflowExecutorRoute::ContinuePlanning { .. }
                ));
                if refund_iteration_for_execute_code_only {
                    iteration_budget.refund();
                }
                continue;
            }
            WorkflowPlannerRoute::ReviewFinal { content } => {
                assistant_text = content;
                assistant_provider_data = reply.provider_data.clone();
                desktop_stream_state.emit_provider_thinking_if_idle(
                    desktop_app,
                    &persona.id,
                    &assistant_provider_data,
                );
                assistant_model = reply.model.clone();
                assistant_provider_id = reply.provider_id.clone();
                assistant_prompt_tokens = reply.prompt_tokens;
                break;
            }
            WorkflowPlannerRoute::Recover { observation } => {
                observations.push(observation);
                continue;
            }
        }
    }
    if assistant_text.trim().is_empty() {
        if iteration_budget.exhausted() {
            append_parent_phase_event(
                store,
                &saved_run.run_id,
                "iteration_budget_exhausted",
                json!({
                    "used": iteration_budget.used(),
                    "maxTotal": iteration_budget.max_total(),
                    "remaining": iteration_budget.remaining(),
                }),
            )?;
        }
        let (planner_error_kind, planner_error) = if iteration_budget.exhausted() {
            (
                WorkflowPlannerErrorKind::IterationBudgetExhausted,
                format!(
                    "Agent iteration budget exhausted before a final answer ({}/{}).",
                    iteration_budget.used(),
                    iteration_budget.max_total()
                ),
            )
        } else if let Some(kind) = planner_recovery_exhausted {
            (
                WorkflowPlannerErrorKind::LlmRecoveryExhausted,
                format!("LLM recovery exhausted before a final answer: {kind}."),
            )
        } else {
            (
                WorkflowPlannerErrorKind::NoFinalAnswer,
                "Planner loop ended without a final answer.".to_string(),
            )
        };
        workflow_planner.failed(
            store,
            &saved_run.run_id,
            None,
            planner_error_kind,
            &planner_error,
        )?;
        assistant_text = if observations.is_empty() {
            reviewer_skip_reason = Some("no_final_answer");
            recovery_reply(&effective_request_content)
        } else if iteration_budget.exhausted() {
            reviewer_skip_reason = Some("iteration_budget_exhausted");
            format!(
                "已达到本轮 agent 迭代预算（{}/{}），当前没有得到最终回答。\n\n{}",
                iteration_budget.used(),
                iteration_budget.max_total(),
                observations.join("\n\n")
            )
        } else {
            reviewer_skip_reason = Some("no_final_answer");
            format!(
                "已完成可用工具检查，但当前恢复版 agent loop 未得到最终回答。\n\n{}",
                observations.join("\n\n")
            )
        };
    }
    normalize_guardrail_halt_reply(&mut assistant_text, &observations);
    append_file_mutation_footer(&mut assistant_text, &failed_file_mutations);
    assistant_text = sanitize_visible_assistant_reply(&assistant_text);
    assistant_text = run_transform_llm_output_hooks(
        store,
        &saved_run.run_id,
        &effective_request_content,
        &assistant_text,
        assistant_model.as_deref(),
        assistant_provider_id.as_deref(),
    )
    .await;
    run_post_llm_call_hooks(
        store,
        &saved_run.run_id,
        &effective_request_content,
        &assistant_text,
        assistant_model.as_deref(),
        assistant_provider_id.as_deref(),
    )
    .await;
    if check_agent_run_interrupted(
        store,
        &saved_run.run_id,
        run_started_at,
        run_timeout_seconds,
        post_tool_quiet_timeout_seconds,
        desktop_app,
    )? {
        return Ok(vec![user]);
    }
    let mut assistant_message = ChatMessage::new(
        conversation.id.clone(),
        "assistant",
        assistant_text.clone(),
        "desktop-agent",
    );
    if desktop_stream_state
        .emitted_answer_text
        .load(Ordering::SeqCst)
    {
        if let Ok(streaming_message) = desktop_stream_state.message.lock() {
            assistant_message.id = streaming_message.id.clone();
            assistant_message.created_at = streaming_message.created_at.clone();
        }
    }
    assistant_message.provider_data = assistant_provider_data;
    if silent_pet_vision {
        assistant_message.source = "pet-vision".into();
        mark_message_visible_for_pet_vision(&mut assistant_message);
    }
    ensure_turn_user_message_persisted(store, &conversation.id, &user)?;
    let assistant = store.append_message(assistant_message)?;

    let mut final_run = store.agent_run(&saved_run.run_id)?;
    final_run.state = "completed".into();
    final_run.updated_at = now_iso();
    final_run.completed_at = Some(final_run.updated_at.clone());
    let saved_completed_run = store.save_agent_run(final_run)?;
    let reviewer_route = if let Some(reason) = reviewer_skip_reason {
        workflow_reviewer.skipped(
            store,
            &saved_completed_run.run_id,
            &assistant.id,
            reason,
            assistant_model.as_deref(),
            assistant_provider_id.as_deref(),
        )?
    } else {
        workflow_reviewer.completed(
            store,
            &saved_completed_run.run_id,
            &assistant.id,
            assistant_model.as_deref(),
            assistant_provider_id.as_deref(),
        )?
    };
    debug_assert!(matches!(
        reviewer_route,
        WorkflowReviewerRoute::Completed { .. } | WorkflowReviewerRoute::Skipped { .. }
    ));
    if assistant_prompt_tokens > 0 {
        if let Some(note) = maybe_post_turn_compress_with_context_engine(
            store,
            &saved_completed_run.run_id,
            &conversation.id,
            assistant_prompt_tokens,
            chat_config.short_context_token_budget,
            (chat_config.max_context_rounds.max(1) * 2 + 1).clamp(3, 60),
        )? {
            append_parent_phase_event(
                store,
                &saved_completed_run.run_id,
                "context_engine_post_turn_note",
                json!({ "note": note }),
            )?;
        }
    }
    let saved_completed_run = store.agent_run(&saved_completed_run.run_id)?;
    if let Some(request_id) =
        line_postback_cache_set_ready_for_conversation(store, &conversation.id, &assistant_text)?
    {
        append_parent_phase_event(
            store,
            &saved_completed_run.run_id,
            "line_postback_cache_ready",
            json!({
                "requestId": request_id,
                "request_id": request_id,
                "conversationId": conversation.id,
                "conversation_id": conversation.id,
            }),
        )?;
    }
    run_session_finished_hooks(
        store,
        &saved_completed_run,
        json!({
            "source": "chat_turn",
            "postDelivery": {
                "schema": "hermes_post_delivery_callback_desktop_v1",
                "delivered": true,
                "transport": "local_conversation",
                "conversationId": conversation.id,
                "messageId": assistant.id,
                "messageSource": assistant.source,
            }
        }),
    )
    .await;
    let _ = maybe_generate_title_from_auxiliary_assignment(
        store,
        &conversation.id,
        &chat_config,
        &providers,
        &effective_persona,
        &effective_request_content,
        &assistant_text,
    )
    .await;
    on_memory_turn_synced(
        store,
        &saved_completed_run.run_id,
        &conversation.id,
        &persona,
        &effective_request_content,
        &assistant_text,
    )?;
    maybe_enqueue_goal_continuation_after_turn(
        store,
        &conversation,
        &persona,
        &providers,
        &effective_persona,
        &assistant_text,
        app,
    )
    .await?;
    let source_for_queue_drain = request
        .provider_data
        .as_ref()
        .and_then(|data| data.get("source"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if source_for_queue_drain != "proactive-internal" && !silent_pet_vision {
        drain_wechat_queue_this_turn(store, &conversation.id, app).await;
        spawn_user_queue_drain_after_turn(store, &conversation.id, app);
    }
    maybe_run_background_skill_curator(store, &chat_config)?;
    let saved_completed_run = store.agent_run(&saved_completed_run.run_id)?;
    let pet_event_source = request
        .provider_data
        .as_ref()
        .and_then(|data| data.get("source"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("desktop");
    let assistant_desktop_app = if silent_pet_vision { app } else { desktop_app };
    if let Some(app) = assistant_desktop_app {
        let event_assistant = crate::preview_message_for_ui(assistant.clone(), event_preview_chars);
        let _ = app.emit(
            "synthchat-chat-event",
            json!({
                "type": "assistant_message",
                "source": assistant.source,
                "personaId": persona.id,
                "conversationId": conversation.id,
                "message": event_assistant,
                "isLast": true,
            }),
        );
    }
    if let Some(app) = app.filter(|_| pet_event_source != "wechat") {
        if desktop_stream_state
            .emitted_answer_text
            .load(Ordering::SeqCst)
        {
            emit_assistant_stream_event(
                app,
                &conversation.id,
                Some(&persona.id),
                &assistant.source,
                &assistant,
                "",
                true,
                event_preview_chars,
            );
        }
        emit_pet_assistant_event(
            Some(app),
            "assistant_final",
            &assistant.source,
            Some(&persona.id),
            &conversation.id,
            &assistant,
        );
    }
    emit_agent_run_record(desktop_app, &saved_completed_run, Some(&assistant));
    Ok(vec![user, assistant])
}

// Drains any pending WeChat queue items for the conversation while the current
// turn still holds chat_turn_lock. Using WECHAT_DRAIN_ACTIVE flag, the inner
// run_chat_turn call skips re-acquiring the lock, avoiding deadlock.
async fn drain_wechat_queue_this_turn(
    store: &AppStore,
    conversation_id: &str,
    app: Option<&AppHandle>,
) {
    let Ok(has_pending) = pending_user_queue_exists(store, conversation_id) else {
        return;
    };
    if !has_pending {
        return;
    }
    mark_wechat_drain(conversation_id);
    let mut count = 0usize;
    while let Ok(Some(item)) = store.claim_next_wechat_agent_request(conversation_id) {
        emit_agent_queue_event(app, "claimed", Some(&item), Some(conversation_id));
        let request = SendChatRequest {
            conversation_id: Some(item.conversation_id.clone()),
            persona_id: Some(item.persona_id.clone()),
            agent_id: None,
            content: item.content.clone(),
            provider_data: item.request_provider_data(),
            queue_item_id: Some(item.id.clone()),
        };
        // Wrap in the task-local scope so the admission guard bypass in
        // run_chat_turn_with_toolset_policy_and_iteration_limit only activates
        // for this task — concurrent external turns cannot observe it.
        let status = match WECHAT_DRAIN_TASK_ACTIVE
            .scope(true, Box::pin(run_chat_turn_with_app(
                store,
                request,
                ToolExecutionContext::Interactive,
                app,
            )))
            .await
        {
            Ok(messages) => {
                let _ = crate::wechat_settings::finalize_queued_wechat_turn(
                    store,
                    &messages,
                    item.provider_data.as_ref(),
                    item.started_at.as_deref(),
                )
                .await;
                "completed"
            }
            Err(e) => {
                let _ = store.complete_agent_queue_item(&item.id, "failed", Some(e.to_string()));
                break;
            }
        };
        if let Ok(Some(completed)) = store.complete_agent_queue_item(&item.id, status, None) {
            record_agent_queue_workflow_terminal(store, &completed).ok();
            emit_agent_queue_event(app, &completed.status, Some(&completed), Some(conversation_id));
        }
        count += 1;
    }
    unmark_wechat_drain(conversation_id);
    let _ = count;
}

fn spawn_user_queue_drain_after_turn(
    store: &AppStore,
    conversation_id: &str,
    app: Option<&AppHandle>,
) {
    let Some(app) = app.cloned() else {
        return;
    };
    let store = store.clone();
    let conversation_id = conversation_id.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(75)).await;
        let Ok(has_pending_user_queue) = pending_user_queue_exists(&store, &conversation_id) else {
            return;
        };
        if !has_pending_user_queue {
            return;
        }
        let _ = drain_queued_requests_for_conversation(&store, &conversation_id, Some(&app)).await;
    });
}

pub(crate) async fn drain_queued_requests_for_conversation(
    store: &AppStore,
    conversation_id: &str,
    app: Option<&AppHandle>,
) -> AppResult<usize> {
    let mut count = 0usize;
    while let Some(item) = store.claim_next_agent_request(conversation_id)? {
        emit_agent_queue_event(app, "claimed", Some(&item), Some(conversation_id));
        if is_pet_vision_silent_queue_item(&item) {
            let completed = store
                .complete_agent_queue_item(
                    &item.id,
                    "canceled",
                    Some("Skipped stale pet vision capture while draining queue.".into()),
                )?
                .unwrap_or_else(|| {
                    let mut fallback = item.clone();
                    fallback.status = "canceled".into();
                    fallback.error =
                        Some("Skipped stale pet vision capture while draining queue.".into());
                    fallback.updated_at = now_iso();
                    fallback.completed_at = Some(now_iso());
                    fallback
                });
            record_agent_queue_workflow_terminal(store, &completed)?;
            emit_agent_queue_event(
                app,
                &completed.status,
                Some(&completed),
                Some(conversation_id),
            );
            continue;
        }
        let request = SendChatRequest {
            conversation_id: Some(item.conversation_id.clone()),
            persona_id: Some(item.persona_id.clone()),
            agent_id: None,
            content: item.content.clone(),
            provider_data: item.request_provider_data(),
            queue_item_id: Some(item.id.clone()),
        };
        let status = match Box::pin(run_chat_turn_with_app(
            store,
            request,
            ToolExecutionContext::Interactive,
            app,
        ))
        .await
        {
            Ok(messages) => {
                crate::wechat_settings::finalize_queued_wechat_turn(
                    store,
                    &messages,
                    item.provider_data.as_ref(),
                    item.started_at.as_deref(),
                )
                .await?;
                "completed"
            }
            Err(error) => {
                let failed = store
                    .complete_agent_queue_item(&item.id, "failed", Some(error.to_string()))?
                    .unwrap_or_else(|| {
                        let mut fallback = item.clone();
                        fallback.status = "failed".into();
                        fallback.error = Some(error.to_string());
                        fallback.updated_at = now_iso();
                        fallback.completed_at = Some(now_iso());
                        fallback
                    });
                record_agent_queue_workflow_terminal(store, &failed)?;
                emit_agent_queue_event(app, &failed.status, Some(&failed), Some(conversation_id));
                return Err(error);
            }
        };
        let completed = store
            .complete_agent_queue_item(&item.id, status, None)?
            .unwrap_or_else(|| {
                let mut fallback = item;
                fallback.status = status.into();
                fallback.updated_at = now_iso();
                fallback.completed_at = Some(now_iso());
                fallback
            });
        record_agent_queue_workflow_terminal(store, &completed)?;
        emit_agent_queue_event(
            app,
            &completed.status,
            Some(&completed),
            Some(conversation_id),
        );
        count += 1;
    }
    Ok(count)
}

async fn maybe_enqueue_goal_continuation_after_turn(
    store: &AppStore,
    conversation: &Conversation,
    persona: &Persona,
    providers: &[LlmProvider],
    effective_persona: &Persona,
    assistant_text: &str,
    app: Option<&AppHandle>,
) -> AppResult<()> {
    let Some(goal) = goal_state::agent_goal_status(store, &conversation.id)? else {
        return Ok(());
    };
    if goal.status != "active" {
        return Ok(());
    }
    let verdict = match goal_judge::judge_goal_completion(
        store,
        &goal.goal,
        assistant_text,
        &goal.subgoals,
        providers,
        effective_persona,
    )
    .await
    {
        Ok(verdict) => verdict,
        Err(error) => goal_judge::GoalJudgeVerdict {
            done: false,
            reason: format!("judge error: {error}"),
            parse_failed: false,
            model: String::new(),
        },
    };
    let Some(updated) = goal_state::record_agent_goal_verdict(
        store,
        &conversation.id,
        verdict.done,
        &verdict.reason,
        verdict.parse_failed,
    )?
    else {
        return Ok(());
    };
    if updated.status != "active" {
        emit_agent_goal_event(
            app,
            &updated.status,
            &conversation.id,
            Some(&updated),
            updated.last_reason.as_deref(),
        );
        return Ok(());
    }
    let has_pending_user_queue = pending_user_queue_exists(store, &conversation.id)?;
    if has_pending_user_queue {
        let paused = goal_state::pause_agent_goal_for_preempting_queue(store, &conversation.id)?;
        emit_agent_goal_event(
            app,
            "paused",
            &conversation.id,
            paused.as_ref(),
            paused
                .as_ref()
                .and_then(|state| state.paused_reason.as_deref()),
        );
        return Ok(());
    }
    let Some(prompt) = goal_state::agent_goal_continuation_prompt(&updated) else {
        return Ok(());
    };
    let (_, queued) = enqueue_prompt_for_conversation(store, conversation, persona, &prompt)?;
    emit_agent_queue_event(app, "queued", Some(&queued), Some(&conversation.id));
    emit_agent_goal_event(
        app,
        "continuing",
        &conversation.id,
        Some(&updated),
        updated.last_reason.as_deref(),
    );
    spawn_goal_continuation_drain(store, &conversation.id, app);
    Ok(())
}

fn is_goal_continuation_prompt(content: &str) -> bool {
    content.starts_with(GOAL_CONTINUATION_PREFIX)
}

const GOAL_CONTINUATION_PREFIX: &str = "[Continuing toward your standing goal]";

fn pending_user_queue_exists(store: &AppStore, conversation_id: &str) -> AppResult<bool> {
    Ok(store.agent_queue()?.into_iter().any(|item| {
        item.conversation_id == conversation_id
            && item.status == "pending"
            && !is_goal_continuation_prompt(&item.content)
    }))
}

fn pending_goal_continuation_exists(store: &AppStore, conversation_id: &str) -> AppResult<bool> {
    Ok(store.agent_queue()?.into_iter().any(|item| {
        item.conversation_id == conversation_id
            && item.status == "pending"
            && is_goal_continuation_prompt(&item.content)
    }))
}

fn spawn_goal_continuation_drain(store: &AppStore, conversation_id: &str, app: Option<&AppHandle>) {
    let Some(app) = app.cloned() else {
        return;
    };
    let store = store.clone();
    let conversation_id = conversation_id.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ =
            drain_goal_continuations_for_conversation(&store, &conversation_id, Some(&app)).await;
    });
}

async fn drain_goal_continuations_for_conversation(
    store: &AppStore,
    conversation_id: &str,
    app: Option<&AppHandle>,
) -> AppResult<usize> {
    let mut count = 0usize;
    loop {
        if pending_user_queue_exists(store, conversation_id)? {
            let paused = goal_state::pause_agent_goal_for_preempting_queue(store, conversation_id)?;
            emit_agent_goal_event(
                app,
                "paused",
                conversation_id,
                paused.as_ref(),
                paused
                    .as_ref()
                    .and_then(|state| state.paused_reason.as_deref()),
            );
            return Ok(count);
        }
        if !pending_goal_continuation_exists(store, conversation_id)? {
            return Ok(count);
        }
        let Some(item) = store.claim_next_agent_request_with_content_prefix(
            conversation_id,
            GOAL_CONTINUATION_PREFIX,
        )?
        else {
            return Ok(count);
        };
        emit_agent_queue_event(app, "claimed", Some(&item), Some(conversation_id));
        let request = SendChatRequest {
            conversation_id: Some(item.conversation_id.clone()),
            persona_id: Some(item.persona_id.clone()),
            agent_id: None,
            content: item.content.clone(),
            provider_data: item.request_provider_data(),
            queue_item_id: Some(item.id.clone()),
        };
        let status = match Box::pin(run_chat_turn_with_app(
            store,
            request,
            ToolExecutionContext::Interactive,
            app,
        ))
        .await
        {
            Ok(messages) => {
                crate::wechat_settings::finalize_queued_wechat_turn(
                    store,
                    &messages,
                    item.provider_data.as_ref(),
                    item.started_at.as_deref(),
                )
                .await?;
                "completed"
            }
            Err(error) => {
                let failed = store
                    .complete_agent_queue_item(&item.id, "failed", Some(error.to_string()))?
                    .unwrap_or_else(|| {
                        let mut fallback = item.clone();
                        fallback.status = "failed".into();
                        fallback.error = Some(error.to_string());
                        fallback.updated_at = now_iso();
                        fallback.completed_at = Some(now_iso());
                        fallback
                    });
                record_agent_queue_workflow_terminal(store, &failed)?;
                emit_agent_queue_event(app, &failed.status, Some(&failed), Some(conversation_id));
                return Err(error);
            }
        };
        let completed = store
            .complete_agent_queue_item(&item.id, status, None)?
            .unwrap_or_else(|| {
                let mut fallback = item;
                fallback.status = status.into();
                fallback.updated_at = now_iso();
                fallback.completed_at = Some(now_iso());
                fallback
            });
        record_agent_queue_workflow_terminal(store, &completed)?;
        emit_agent_queue_event(
            app,
            &completed.status,
            Some(&completed),
            Some(conversation_id),
        );
        count += 1;
    }
}

fn maybe_run_background_skill_curator(
    store: &AppStore,
    chat_config: &crate::models::ChatConfig,
) -> AppResult<()> {
    if !chat_config.background_skill_curator_enabled {
        return Ok(());
    }
    let _ = skill_library::maybe_curate_skills_report(
        store,
        chat_config.background_skill_curator_interval_hours,
    )?;
    Ok(())
}

#[cfg(test)]
mod goal_loop_tests {
    use super::*;
    use crate::models::new_id;

    #[tokio::test]
    async fn active_goal_queues_continuation_after_continue_verdict() {
        let dir = std::env::temp_dir().join(format!("synthchat-goal-loop-{}", new_id("test")));
        std::fs::create_dir_all(&dir).unwrap();
        let store = AppStore::new(dir.join("state.json")).unwrap();
        let persona = store.persona(None).unwrap();
        let conversation = store
            .create_conversation(Some("Goal".into()), Some(persona.id.clone()))
            .unwrap();
        goal_state::set_agent_goal(&store, &conversation.id, "Finish the parity task", Some(3))
            .unwrap();

        maybe_enqueue_goal_continuation_after_turn(
            &store,
            &conversation,
            &persona,
            &[],
            &persona,
            "I inspected the current code and found more work remains.",
            None,
        )
        .await
        .unwrap();

        let state = goal_state::agent_goal_status(&store, &conversation.id)
            .unwrap()
            .unwrap();
        assert_eq!(state.status, "active");
        assert_eq!(state.turns_used, 1);
        let queue = store.agent_queue().unwrap();
        assert_eq!(queue.len(), 1);
        assert!(queue[0]
            .content
            .starts_with("[Continuing toward your standing goal]"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn completed_goal_does_not_queue_continuation() {
        let dir = std::env::temp_dir().join(format!("synthchat-goal-loop-{}", new_id("test")));
        std::fs::create_dir_all(&dir).unwrap();
        let store = AppStore::new(dir.join("state.json")).unwrap();
        let persona = store.persona(None).unwrap();
        let conversation = store
            .create_conversation(Some("Goal".into()), Some(persona.id.clone()))
            .unwrap();
        goal_state::set_agent_goal(&store, &conversation.id, "Finish the parity task", Some(3))
            .unwrap();

        maybe_enqueue_goal_continuation_after_turn(
            &store,
            &conversation,
            &persona,
            &[],
            &persona,
            "The parity task is completed.",
            None,
        )
        .await
        .unwrap();

        let state = goal_state::agent_goal_status(&store, &conversation.id)
            .unwrap()
            .unwrap();
        assert_eq!(state.status, "done");
        assert_eq!(state.turns_used, 1);
        assert!(store.agent_queue().unwrap().is_empty());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn pending_user_queue_pauses_goal_continuation() {
        let dir = std::env::temp_dir().join(format!("synthchat-goal-loop-{}", new_id("test")));
        std::fs::create_dir_all(&dir).unwrap();
        let store = AppStore::new(dir.join("state.json")).unwrap();
        let persona = store.persona(None).unwrap();
        let conversation = store
            .create_conversation(Some("Goal".into()), Some(persona.id.clone()))
            .unwrap();
        goal_state::set_agent_goal(&store, &conversation.id, "Finish the parity task", Some(3))
            .unwrap();
        enqueue_prompt_for_conversation(&store, &conversation, &persona, "User changed priority")
            .unwrap();

        maybe_enqueue_goal_continuation_after_turn(
            &store,
            &conversation,
            &persona,
            &[],
            &persona,
            "I inspected the current code and found more work remains.",
            None,
        )
        .await
        .unwrap();

        let state = goal_state::agent_goal_status(&store, &conversation.id)
            .unwrap()
            .unwrap();
        assert_eq!(state.status, "paused");
        assert_eq!(
            state.paused_reason.as_deref(),
            Some("pending user queue item preempted goal continuation")
        );
        let queue = store.agent_queue().unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].content, "User changed priority");

        let _ = std::fs::remove_dir_all(dir);
    }
}

const EMPTY_RESPONSE_MAX_RECOVERY_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EmptyLlmResponseRecovery {
    pub kind: &'static str,
    pub note: String,
    pub attempt: u32,
    pub max_attempts: u32,
    pub after_tools: bool,
}

pub(super) fn next_empty_llm_response_recovery(
    observations: &[String],
    attempts: &mut HashMap<String, u32>,
) -> Option<EmptyLlmResponseRecovery> {
    let after_tools = !observations.is_empty();
    let kind = if after_tools {
        "empty_response_after_tools"
    } else {
        "empty_response"
    };
    let prior_attempts = attempts.get(kind).copied().unwrap_or(0);
    if prior_attempts >= EMPTY_RESPONSE_MAX_RECOVERY_ATTEMPTS {
        return None;
    }
    let attempt = prior_attempts + 1;
    attempts.insert(kind.to_string(), attempt);
    let max_attempts = EMPTY_RESPONSE_MAX_RECOVERY_ATTEMPTS;
    let note = if after_tools {
        format!(
            "Model returned an empty response after tool results (attempt {attempt}/{max_attempts}). You just received tool observations above; process them and return {{\"action\":\"final\",\"content\":\"...\"}}. Request another tool only if more evidence is required."
        )
    } else {
        format!(
            "Model returned an empty response (attempt {attempt}/{max_attempts}). Retry with a valid planner JSON object: either {{\"action\":\"tool\",...}} or {{\"action\":\"final\",\"content\":\"...\"}}."
        )
    };
    Some(EmptyLlmResponseRecovery {
        kind,
        note,
        attempt,
        max_attempts,
        after_tools,
    })
}

struct TitleGenerationProviderPlan {
    providers: Vec<LlmProvider>,
    persona: Persona,
}

async fn maybe_generate_title_from_auxiliary_assignment(
    store: &AppStore,
    conversation_id: &str,
    chat_config: &ChatConfig,
    main_providers: &[LlmProvider],
    main_persona: &Persona,
    user_message: &str,
    assistant_response: &str,
) -> AppResult<()> {
    if !chat_config.auto_title_enabled {
        return Ok(());
    }
    let messages = store.messages(conversation_id, None)?;
    let visible_turns = messages
        .iter()
        .filter(|message| matches!(message.role.as_str(), "user" | "assistant"))
        .count();
    if visible_turns > 2 {
        return Ok(());
    }
    let Some(plan) = build_title_generation_provider_plan(store, main_providers, main_persona)?
    else {
        return Ok(());
    };
    let title =
        generate_conversation_title_with_plan(store, &plan, user_message, assistant_response)
            .await?;
    if let Some(title) = clean_generated_title(&title) {
        store.rename_conversation(conversation_id, title)?;
    }
    Ok(())
}

fn build_title_generation_provider_plan(
    store: &AppStore,
    main_providers: &[LlmProvider],
    main_persona: &Persona,
) -> AppResult<Option<TitleGenerationProviderPlan>> {
    let Some(assignment) = list_agent_auxiliary_task_assignments(store)?
        .into_iter()
        .find(|assignment| assignment.key == "title_generation")
    else {
        return Ok(None);
    };
    let provider = assignment.provider.trim();
    let provider_id = if provider.eq_ignore_ascii_case("auto") {
        ""
    } else {
        provider
    };
    let model = assignment.model.trim();
    let base_url = assignment.base_url.trim();
    if provider_id.is_empty() && model.is_empty() && base_url.is_empty() {
        return Ok(None);
    }
    let mut providers = if !base_url.is_empty() {
        vec![LlmProvider {
            id: "auxiliary-title-generation-custom".into(),
            name: "Title generation auxiliary".into(),
            provider_type: "openai_compatible".into(),
            base_url: base_url.into(),
            append_chat_path: true,
            api_key: (!assignment.api_key.trim().is_empty())
                .then(|| assignment.api_key.trim().to_string()),
            model: if model.is_empty() {
                main_providers
                    .first()
                    .map(|provider| provider.model.clone())
                    .unwrap_or_default()
            } else {
                model.to_string()
            },
            enabled: true,
            timeout_seconds: assignment.timeout,
            ..LlmProvider::default()
        }]
    } else if provider_id.is_empty() {
        main_providers.to_vec()
    } else {
        let mut candidates = store.provider_candidates(Some(provider_id))?;
        let credential_prefix = format!("{provider_id}:cred-");
        candidates.retain(|provider| {
            provider.id == provider_id || provider.id.starts_with(&credential_prefix)
        });
        if candidates.is_empty() {
            return Err(AppError::NotFound(format!(
                "title generation llm provider {provider_id}"
            )));
        }
        candidates
    };
    let mut persona = main_persona.clone();
    if !provider_id.is_empty() {
        persona.llm_provider = provider_id.to_string();
    }
    if !model.is_empty() {
        persona.llm_model = model.to_string();
        for provider in &mut providers {
            provider.model = model.to_string();
        }
    }
    Ok(Some(TitleGenerationProviderPlan { providers, persona }))
}

async fn generate_conversation_title_with_plan(
    store: &AppStore,
    plan: &TitleGenerationProviderPlan,
    user_message: &str,
    assistant_response: &str,
) -> AppResult<String> {
    let user_snippet = user_message.chars().take(500).collect::<String>();
    let assistant_snippet = assistant_response.chars().take(500).collect::<String>();
    let system_prompt = "Generate a short, descriptive title (3-7 words) for a conversation. Return ONLY the title text. No quotes, no punctuation at the end, no prefixes.";
    let prompt = format!("User: {user_snippet}\n\nAssistant: {assistant_snippet}");
    let message = ChatMessage::new(
        "__title_generation__".into(),
        "user",
        prompt.clone(),
        "internal",
    );
    let reply = complete_chat_with_provider_failover(
        store,
        None,
        &plan.providers,
        &plan.persona,
        system_prompt.to_string(),
        vec![message],
        &prompt,
        None,
        None,
    )
    .await?;
    Ok(reply.content)
}

fn clean_generated_title(raw: &str) -> Option<String> {
    let mut title = raw
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string();
    if title.to_lowercase().starts_with("title:") {
        title = title[6..].trim().to_string();
    }
    title = title
        .trim_end_matches(['.', '!', '?', '。', '！', '？'])
        .trim()
        .to_string();
    if title.is_empty() {
        return None;
    }
    if title.chars().count() > 80 {
        title = title.chars().take(77).collect::<String>();
        title.push_str("...");
    }
    Some(title)
}

#[cfg(test)]
mod title_generation_tests {
    use super::clean_generated_title;

    #[test]
    fn clean_generated_title_strips_prefix_quotes_and_trailing_punctuation() {
        assert_eq!(
            clean_generated_title("\"Title: Build Hermes Plugins.\"").as_deref(),
            Some("Build Hermes Plugins")
        );
        assert_eq!(clean_generated_title("   ").as_deref(), None);
    }
}

pub(super) fn merge_disabled_toolset_overrides(
    existing: &mut Vec<String>,
    additional: Vec<String>,
) {
    let mut seen = existing
        .iter()
        .map(|name| normalize_toolset_name(name))
        .filter(|name| !name.is_empty())
        .collect::<HashSet<_>>();
    for name in additional {
        let normalized = normalize_toolset_name(&name);
        if normalized.is_empty() || !seen.insert(normalized) {
            continue;
        }
        existing.push(name);
    }
}

pub(super) fn resolve_chat_turn_persona_and_agent(
    store: &AppStore,
    conversation: &Conversation,
    request: &SendChatRequest,
) -> AppResult<(Persona, AgentDefinition)> {
    let agent = store.agent(
        request
            .agent_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
            .or(Some(conversation.agent_id.as_str())),
    )?;
    let persona = resolve_chat_turn_persona(store, conversation, request, &agent)?;
    Ok((persona, agent))
}

fn resolve_chat_turn_persona(
    store: &AppStore,
    conversation: &Conversation,
    request: &SendChatRequest,
    agent: &AgentDefinition,
) -> AppResult<Persona> {
    let requested_persona = request
        .persona_id
        .as_deref()
        .and_then(|id| store.persona(Some(id)).ok());
    if let Some(persona) = requested_persona.as_ref() {
        if persona.agent_id == agent.id {
            return Ok(persona.clone());
        }
    }
    let conversation_persona = store.persona(conversation.persona_id.as_deref()).ok();
    if let Some(persona) = conversation_persona.as_ref() {
        if persona.agent_id == agent.id {
            return Ok(persona.clone());
        }
    }
    store
        .personas()?
        .into_iter()
        .find(|persona| persona.agent_id == agent.id)
        .or(requested_persona)
        .or(conversation_persona)
        .map(Ok)
        .unwrap_or_else(|| store.persona(None))
}

pub(super) fn tool_batch_is_execute_code_only(requests: &[(String, Value)]) -> bool {
    !requests.is_empty()
        && requests
            .iter()
            .all(|(tool_name, _)| tool_name == "execute_code")
}

pub(super) fn apply_subagent_approval_override(
    context: ToolExecutionContext,
    subagent_auto_approve: Option<bool>,
    approval_reason: Option<String>,
    tool_name: &str,
) -> AppResult<Option<String>> {
    let Some(reason) = approval_reason else {
        return Ok(None);
    };
    if !matches!(
        context,
        ToolExecutionContext::SubagentLeaf | ToolExecutionContext::SubagentOrchestrator
    ) {
        return Ok(Some(reason));
    }
    match subagent_auto_approve {
        Some(true) => Ok(None),
        Some(false) => Err(AppError::BadRequest(format!(
            "Subagent auto-denied tool approval for {tool_name}: {reason}. Set delegationSubagentAutoApprove=true to allow unattended approval."
        ))),
        None => Ok(Some(reason)),
    }
}

pub(super) fn handle_busy_conversation_input(
    store: &AppStore,
    conversation: &Conversation,
    persona: &Persona,
    content: &str,
    app: Option<&AppHandle>,
) -> AppResult<Option<Vec<ChatMessage>>> {
    handle_busy_conversation_input_with_origin(
        store,
        conversation,
        persona,
        content,
        None,
        None,
        app,
    )
}

pub(super) fn handle_busy_conversation_input_for_request(
    store: &AppStore,
    conversation: &Conversation,
    persona: &Persona,
    request: &SendChatRequest,
    app: Option<&AppHandle>,
) -> AppResult<Option<Vec<ChatMessage>>> {
    handle_busy_conversation_input_with_origin(
        store,
        conversation,
        persona,
        &request.content,
        request
            .provider_data
            .as_ref()
            .and_then(|data| data.get("source"))
            .and_then(Value::as_str),
        request.provider_data.clone(),
        app,
    )
}

fn handle_busy_conversation_input_with_origin(
    store: &AppStore,
    conversation: &Conversation,
    persona: &Persona,
    content: &str,
    source: Option<&str>,
    provider_data: Option<Value>,
    app: Option<&AppHandle>,
) -> AppResult<Option<Vec<ChatMessage>>> {
    let Some(active) = store.active_agent_run_for_conversation(&conversation.id)? else {
        return Ok(None);
    };
    let pre_persisted_user =
        pre_persisted_user_message(store, &conversation.id, content, provider_data.as_ref())?;
    match normalize_busy_input_mode(&store.config()?.chat.busy_input_mode).as_str() {
        "interrupt" => {
            abort_agent_run(
                store,
                active.run_id,
                Some("Agent run interrupted by a new user request.".into()),
                app,
            )?;
            Ok(None)
        }
        "steer" => {
            let user = if let Some(user) = pre_persisted_user {
                user
            } else {
                let mut user_message = ChatMessage::new(
                    conversation.id.clone(),
                    "user",
                    content.to_string(),
                    "desktop-steer",
                );
                user_message.provider_data = provider_data.clone();
                store.append_message(user_message)?
            };
            store.append_agent_run_steer(&active.run_id, content.to_string())?;
            let assistant = store.append_message(control_message(
                conversation,
                format!(
                    "已将新输入注入当前 agent run：{}。它会在下一轮规划前读取。",
                    active.run_id
                ),
            ))?;
            Ok(Some(vec![user, assistant]))
        }
        _ => {
            let (user, queued) = if let Some(user) = pre_persisted_user {
                let queued = store.enqueue_agent_request(
                    conversation.id.clone(),
                    persona.id.clone(),
                    &user,
                )?;
                (user, queued)
            } else {
                enqueue_prompt_for_conversation_with_origin(
                    store,
                    conversation,
                    persona,
                    content,
                    source,
                    provider_data,
                )?
            };
            emit_agent_queue_event(app, "queued", Some(&queued), Some(&conversation.id));
            let assistant = store.append_message(control_message(
                conversation,
                format!(
                    "当前已有运行中的 agent run：{}。新输入已加入队列：{}。",
                    active.run_id, queued.id
                ),
            ))?;
            Ok(Some(vec![user, assistant]))
        }
    }
}

pub(super) fn clarification_response_context_for_turn(
    store: &AppStore,
    conversation_id: &str,
    response: &str,
) -> AppResult<Option<String>> {
    let response = response.trim();
    if response.is_empty() {
        return Ok(None);
    }
    let Some(mut run) = latest_needs_clarification_run(store, conversation_id)? else {
        return Ok(None);
    };
    let workflow_mode = workflow_mode_for_run(&run);
    let question = run
        .checkpoints
        .iter()
        .rev()
        .find(|checkpoint| checkpoint.state == "needs_clarification")
        .map(|checkpoint| checkpoint.summary.clone())
        .unwrap_or_else(|| "Clarification requested.".into());
    let now = now_iso();
    run.checkpoints.push(AgentCheckpointRecord {
        checkpoint_id: new_id("ckpt"),
        run_id: run.run_id.clone(),
        iteration: run.checkpoints.len() as u32 + 1,
        created_at: now.clone(),
        state: "clarification_response".into(),
        completed_call_ids: Vec::new(),
        event_refs: Vec::new(),
        summary: format!(
            "User clarification response: {}",
            truncate_for_prompt(response, 500)
        ),
    });
    run.state = "completed".into();
    run.error = None;
    run.completed_at = Some(now.clone());
    run.updated_at = now;
    // Optimistic-lock: re-read the run immediately before saving to detect a
    // concurrent clarification response that beat us to this state transition.
    // If the stored state is no longer "needsClarification", another request
    // already consumed it — return None so this caller is silently discarded.
    let current_state = store.agent_run(&run.run_id)
        .map(|r| r.state)
        .unwrap_or_default();
    if current_state != "needsClarification" {
        return Ok(None);
    }
    store.save_agent_run(run.clone())?;
    if let Some(checkpoint) = run.checkpoints.last() {
        let mut human_gate = workflow_human_gate_detail("clarification", "completed", &run.run_id);
        human_gate.insert("question".into(), json!(question.clone()));
        human_gate.insert(
            "checkpointId".into(),
            json!(checkpoint.checkpoint_id.clone()),
        );
        human_gate.insert(
            "checkpoint_id".into(),
            json!(checkpoint.checkpoint_id.clone()),
        );
        human_gate.insert(
            "responsePreview".into(),
            json!(truncate_for_prompt(response, 500)),
        );
        human_gate.insert(
            "response_preview".into(),
            json!(truncate_for_prompt(response, 500)),
        );
        let human_gate = Value::Object(human_gate);
        WorkflowDriver::new(workflow_mode).checkpoint().completed(
            store,
            &run.run_id,
            &checkpoint.state,
            &checkpoint.summary,
            json!({
                "kind": "clarification_response",
                "checkpointId": checkpoint.checkpoint_id.clone(),
                "checkpoint_id": checkpoint.checkpoint_id.clone(),
                "iteration": checkpoint.iteration,
                "humanGate": human_gate.clone(),
                "human_gate": human_gate,
            }),
        )?;
    }
    Ok(Some(format!(
        "Continue the user's original task after a clarification exchange.\n\nOriginal request:\n{}\n\nClarification question:\n{}\n\nUser clarification response:\n{}\n\nUse this response to continue the original task. Do not ask the same clarification again unless the response is still insufficient.",
        run.user_request,
        question,
        response
    )))
}

fn latest_needs_clarification_run(
    store: &AppStore,
    conversation_id: &str,
) -> AppResult<Option<AgentRunRecord>> {
    let mut runs = store
        .agent_runs()?
        .into_iter()
        .filter(|run| {
            run.conversation_id == conversation_id
                && run.parent_run_id.is_none()
                && run.state == "needsClarification"
        })
        .collect::<Vec<_>>();
    runs.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(runs.into_iter().next())
}

pub(super) fn pause_run_for_clarify_tool(
    store: &AppStore,
    app: Option<&AppHandle>,
    run: &mut AgentRunRecord,
    conversation_id: &str,
    workflow_mode: WorkflowMode,
    tool_text: &str,
    event: &crate::models::ToolEvent,
) -> AppResult<Option<ChatMessage>> {
    if event.tool_name != "clarify" {
        return Ok(None);
    }
    let Ok(value) = serde_json::from_str::<Value>(tool_text) else {
        return Ok(None);
    };
    if value
        .get("requiresUserInput")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        != true
    {
        return Ok(None);
    }
    let question = value
        .get("question")
        .and_then(Value::as_str)
        .unwrap_or("Clarification required");
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Clarification required: {question}"));
    run.state = "needsClarification".into();
    run.error = None;
    run.updated_at = now_iso();
    run.checkpoints.push(AgentCheckpointRecord {
        checkpoint_id: new_id("ckpt"),
        run_id: run.run_id.clone(),
        iteration: run.checkpoints.len() as u32 + 1,
        created_at: run.updated_at.clone(),
        state: "needs_clarification".into(),
        completed_call_ids: event.call_id.clone().into_iter().collect(),
        event_refs: event
            .call_id
            .clone()
            .map(|call_id| vec![call_id])
            .unwrap_or_default(),
        summary: truncate_for_prompt(&text.replace('\n', " "), 500),
    });
    let mut saved_run = store.save_agent_run(run.clone())?;
    if let Some(checkpoint) = run.checkpoints.last() {
        let mut human_gate = workflow_human_gate_detail("clarification", "waiting", &run.run_id);
        human_gate.insert("question".into(), json!(question));
        human_gate.insert(
            "reason".into(),
            json!(WORKFLOW_REASON_CLARIFY_REQUIRES_USER_INPUT),
        );
        human_gate.insert("requiresUserInput".into(), json!(true));
        human_gate.insert("requires_user_input".into(), json!(true));
        human_gate.insert(
            "checkpointId".into(),
            json!(checkpoint.checkpoint_id.clone()),
        );
        human_gate.insert(
            "checkpoint_id".into(),
            json!(checkpoint.checkpoint_id.clone()),
        );
        human_gate.insert("toolName".into(), json!(event.tool_name.clone()));
        human_gate.insert("tool_name".into(), json!(event.tool_name.clone()));
        if let Some(call_id) = event.call_id.clone() {
            human_gate.insert("callId".into(), json!(call_id.clone()));
            human_gate.insert("call_id".into(), json!(call_id));
        }
        let human_gate = Value::Object(human_gate);
        WorkflowDriver::new(workflow_mode)
            .checkpoint()
            .waiting_from_executor(
                store,
                &run.run_id,
                WORKFLOW_REASON_CLARIFY_REQUIRES_USER_INPUT,
                json!({
                    "toolName": event.tool_name.clone(),
                    "tool_name": event.tool_name.clone(),
                    "callId": event.call_id.clone(),
                    "call_id": event.call_id.clone(),
                    "checkpointId": checkpoint.checkpoint_id.clone(),
                    "checkpoint_id": checkpoint.checkpoint_id.clone(),
                    "humanGate": human_gate.clone(),
                    "human_gate": human_gate.clone(),
                }),
                &checkpoint.state,
                &checkpoint.summary,
                json!({
                    "kind": "clarify_pause",
                    "checkpointScope": "user_input",
                    "checkpoint_scope": "user_input",
                    "toolName": event.tool_name.clone(),
                    "tool_name": event.tool_name.clone(),
                    "checkpointId": checkpoint.checkpoint_id.clone(),
                    "checkpoint_id": checkpoint.checkpoint_id.clone(),
                    "iteration": checkpoint.iteration,
                    "humanGate": human_gate.clone(),
                    "human_gate": human_gate,
                }),
            )?;
    }
    saved_run = store.agent_run(&run.run_id)?;
    let assistant = store.append_message(ChatMessage::new(
        conversation_id.to_string(),
        "assistant",
        text,
        "desktop-agent-clarify",
    ))?;
    emit_agent_run_record(app, &saved_run, Some(&assistant));
    Ok(Some(assistant))
}

pub(super) fn normalize_busy_input_mode(mode: &str) -> String {
    match mode.trim().to_lowercase().as_str() {
        "steer" | "inject" | "plan" => "steer".into(),
        "interrupt" | "abort" | "replace" => "interrupt".into(),
        _ => "queue".into(),
    }
}

pub(super) fn check_agent_run_interrupted(
    store: &AppStore,
    run_id: &str,
    started_at: Instant,
    timeout_seconds: u64,
    post_tool_quiet_timeout_seconds: u64,
    app: Option<&AppHandle>,
) -> AppResult<bool> {
    let latest = store.agent_run(run_id)?;
    if latest.state == "aborted" {
        emit_agent_run_record(app, &latest, None);
        return Ok(true);
    }
    let effective_timeout_seconds = agent_run_effective_timeout_seconds(
        &latest,
        timeout_seconds,
        post_tool_quiet_timeout_seconds,
    );
    if effective_timeout_seconds > 0
        && agent_run_idle_for_timeout(&latest, started_at, effective_timeout_seconds)
    {
        let reason = agent_run_timeout_reason(&latest, effective_timeout_seconds);
        let workflow_mode = workflow_mode_for_run(&latest);
        WorkflowDriver::new(workflow_mode).timeout(
            store,
            run_id,
            &reason,
            effective_timeout_seconds,
        )?;
        let aborted = store.abort_agent_run(run_id, Some(reason.clone()))?;
        store.mark_hermes_session_resume_pending(
            &aborted.conversation_id,
            "agent_run_timeout",
            "agent-loop-timeout",
        )?;
        spawn_session_finished_hooks(
            store,
            aborted.clone(),
            json!({
                "source": "agent_run_timeout",
                "reason": reason,
            }),
        );
        let assistant = store.append_message(ChatMessage::new(
            aborted.conversation_id.clone(),
            "assistant",
            format!("本轮 agent 已自动结束：{}", reason),
            "desktop-agent-error",
        ))?;
        emit_agent_run_record(app, &aborted, Some(&assistant));
        return Ok(true);
    }
    Ok(false)
}

pub(super) async fn await_agent_run_interruptible<F, T>(
    store: &AppStore,
    run_id: &str,
    started_at: Instant,
    timeout_seconds: u64,
    post_tool_quiet_timeout_seconds: u64,
    app: Option<&AppHandle>,
    future: F,
) -> AppResult<Option<T>>
where
    F: Future<Output = T>,
{
    tokio::pin!(future);
    loop {
        tokio::select! {
            output = &mut future => return Ok(Some(output)),
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                if check_agent_run_interrupted(
                    store,
                    run_id,
                    started_at,
                    timeout_seconds,
                    post_tool_quiet_timeout_seconds,
                    app,
                )? {
                    return Ok(None);
                }
            }
        }
    }
}

pub(super) fn apply_acp_session_mcp_scope(
    store: &AppStore,
    conversation: &Conversation,
    agent: &mut AgentDefinition,
) -> AppResult<()> {
    let has_session_mcp = conversation
        .metadata
        .pointer("/acpRuntimeConfig/mcpServers")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty());
    if !has_session_mcp {
        return Ok(());
    }
    let prefix = format!(
        "acp_{}_",
        conversation
            .id
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            })
            .collect::<String>()
    );
    let session_server_ids = store
        .static_list("mcpServers")?
        .into_iter()
        .filter_map(|server| {
            let id = server.get("id").and_then(Value::as_str)?.to_string();
            id.starts_with(&prefix).then_some(id)
        })
        .collect::<Vec<_>>();
    if !session_server_ids.is_empty() {
        agent.enabled_mcp_servers = session_server_ids;
    }
    Ok(())
}

pub(super) fn abort_agent_run_for_turn_aborted_marker(
    store: &AppStore,
    run_id: &str,
    text: &str,
    app: Option<&AppHandle>,
) -> AppResult<bool> {
    if !has_turn_aborted_marker(text) {
        return Ok(false);
    }
    let reason = "Provider reported turn_aborted before completing the turn.".to_string();
    let current_run = store.agent_run(run_id)?;
    let workflow_mode = workflow_mode_for_run(&current_run);
    WorkflowDriver::new(workflow_mode).planner().failed(
        store,
        run_id,
        None,
        WorkflowPlannerErrorKind::ProviderTurnAborted,
        &reason,
    )?;
    let aborted = store.abort_agent_run(run_id, Some(reason.clone()))?;
    spawn_session_finished_hooks(
        store,
        aborted.clone(),
        json!({
            "source": "turn_aborted_marker",
            "reason": reason,
        }),
    );
    let assistant = store.append_message(ChatMessage::new(
        aborted.conversation_id.clone(),
        "assistant",
        format!("本轮 agent 已中止：{reason}"),
        "desktop-agent-error",
    ))?;
    emit_agent_run_record(app, &aborted, Some(&assistant));
    Ok(true)
}

pub(super) fn has_turn_aborted_marker(text: &str) -> bool {
    const TURN_ABORTED_MARKERS: [&str; 2] = ["<turn_aborted>", "<turn_aborted/>"];
    TURN_ABORTED_MARKERS
        .iter()
        .any(|marker| text.contains(marker))
}

fn agent_run_effective_timeout_seconds(
    run: &AgentRunRecord,
    timeout_seconds: u64,
    post_tool_quiet_timeout_seconds: u64,
) -> u64 {
    if post_tool_quiet_timeout_seconds > 0 && agent_run_last_activity_is_tool_result(run) {
        if timeout_seconds > 0 {
            post_tool_quiet_timeout_seconds.min(timeout_seconds)
        } else {
            post_tool_quiet_timeout_seconds
        }
    } else {
        timeout_seconds
    }
}

fn agent_run_last_activity_is_tool_result(run: &AgentRunRecord) -> bool {
    run.last_activity_desc
        .as_deref()
        .map(str::trim)
        .is_some_and(|activity| {
            activity.starts_with("tool completed:")
                || activity.starts_with("tool failed:")
                || activity.starts_with("tool error:")
        })
}

fn agent_run_idle_for_timeout(
    run: &AgentRunRecord,
    started_at: Instant,
    timeout_seconds: u64,
) -> bool {
    if let Some(activity_at) = run
        .last_activity_at
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
    {
        let elapsed = Utc::now().signed_duration_since(activity_at).num_seconds();
        // Guard against NTP clock step-backs (VM migrations, snapshot restores):
        // a negative elapsed means the wall clock was adjusted backward — fall back
        // to the monotonic `started_at` path so the timeout cannot be evaded.
        if elapsed < 0 {
            return started_at.elapsed() >= Duration::from_secs(timeout_seconds);
        }
        return elapsed >= timeout_seconds as i64;
    }
    started_at.elapsed() >= Duration::from_secs(timeout_seconds)
}

fn agent_run_timeout_reason(run: &AgentRunRecord, timeout_seconds: u64) -> String {
    if let Some(activity) = run
        .last_activity_desc
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        return format!(
            "Agent run timed out after {timeout_seconds}s of inactivity; last activity: {activity}."
        );
    }
    format!("Agent run timed out after {timeout_seconds}s.")
}

pub(super) fn drain_agent_steers_into_observations(
    store: &AppStore,
    run: &mut AgentRunRecord,
    observations: &mut Vec<String>,
) -> AppResult<()> {
    let pending = store.drain_agent_run_steers(&run.run_id)?;
    if pending.is_empty() {
        return Ok(());
    }
    run.pending_steers.clear();
    for steer in &pending {
        observations.push(format!(
            "User steer injected before this planner step: {}",
            truncate_for_prompt(steer, 4000)
        ));
    }
    run.phase_events.push(AgentRunPhaseRecord {
        phase: "steer_injected".into(),
        detail: json!({
            "count": pending.len(),
            "previews": pending
                .iter()
                .map(|steer| truncate_for_prompt(steer, 180))
                .collect::<Vec<_>>()
        }),
        updated_at: now_iso(),
    });
    run.updated_at = now_iso();
    store.save_agent_run(run.clone())?;
    Ok(())
}

pub(super) fn recovery_reply(user_content: &str) -> String {
    let trimmed = user_content.trim();
    if trimmed.is_empty() {
        "Agent runtime recovery baseline is active. The previous full agent module must be restored before advanced tool orchestration is available.".into()
    } else {
        format!(
            "Agent runtime recovery baseline is active. I received: {trimmed}\n\nAdvanced Hermes-style tool orchestration is temporarily unavailable until the full agent module is restored."
        )
    }
}

fn llm_route_summary(
    providers: &[crate::models::LlmProvider],
    persona: &Persona,
    error: &str,
) -> String {
    let provider = providers.first();
    let provider_id = provider.map(|provider| provider.id.as_str()).unwrap_or("-");
    let provider_type = provider
        .map(|provider| provider.provider_type.as_str())
        .unwrap_or("-");
    let model = if !persona.llm_model.trim().is_empty() {
        persona.llm_model.trim()
    } else {
        provider
            .map(|provider| provider.model.trim())
            .filter(|model| !model.is_empty())
            .unwrap_or("-")
    };
    let base_url = provider
        .map(|provider| provider.base_url.trim())
        .filter(|base_url| !base_url.is_empty())
        .unwrap_or("-");
    format!(
        "路由：provider={provider_id} ({provider_type})，model={model}，baseUrl={base_url}。\n错误：{}",
        truncate_for_prompt(error, 500)
    )
}
