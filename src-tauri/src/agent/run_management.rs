use std::{env, ffi::OsString, fs, path::PathBuf, time::Duration};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tokio::{process::Command, time::timeout};

use crate::{
    error::{AppError, AppResult},
    models::{
        AgentCheckpointRecord, AgentQueuedRequest, AgentRunPhaseRecord, AgentRunRecord,
        ChatMessage, ScheduledAgentJob, SendChatRequest,
    },
    process_utils::CommandWindowExt,
    store::{scan_scheduled_job_assembled_prompt, AppStore},
};

use super::{
    communication::send_message_external_targets,
    workflow_graph::{
        workflow_mode_for_run, workflow_node_role_label, WorkflowDriver,
        WORKFLOW_DETAIL_ALIAS_PAIRS, WORKFLOW_STATUS_ORDER,
    },
    *,
};

const HERMES_MAX_PLATFORM_DELIVERY_OUTPUT_CHARS: usize = 4000;
const HERMES_TRUNCATED_PLATFORM_DELIVERY_VISIBLE_CHARS: usize = 3800;

#[derive(Debug, Clone)]
pub(super) struct ScheduledScriptRun {
    pub success: bool,
    pub output: String,
}

pub(super) fn append_parent_phase_event(
    store: &AppStore,
    run_id: &str,
    phase: &str,
    detail: Value,
) -> AppResult<()> {
    let mut run = store.agent_run(run_id)?;
    run.phase_events.push(AgentRunPhaseRecord {
        phase: phase.to_string(),
        detail,
        updated_at: now_iso(),
    });
    run.touch_activity(format!("phase: {phase}"));
    store.save_agent_run(run)?;
    Ok(())
}

pub fn spawn_background_chat_turn_for_job(
    app: AppHandle,
    conversation_id: String,
    persona_id: String,
    prompt: String,
    job: Option<crate::models::ScheduledAgentJob>,
) {
    tokio::spawn(async move {
        let store = app.state::<AppStore>();
        if let Some(job) = job.as_ref().filter(|job| job.no_agent) {
            let result = run_scheduled_no_agent_job(&store, job).await;
            match result {
                Ok(output) => {
                    let saved_output = if output.trim().is_empty() {
                        None
                    } else {
                        Some(output)
                    };
                    let delivery_output = saved_output.clone();
                    let _ = store.record_scheduled_agent_job_result(
                        &job.id,
                        "completed",
                        saved_output,
                        None,
                    );
                    if let Some(output) = delivery_output.as_deref() {
                        if !scheduled_job_output_is_silent(output) {
                            let delivery_error =
                                deliver_scheduled_job_result(&store, job, output, true).await;
                            let _ = store
                                .record_scheduled_agent_job_delivery_error(&job.id, delivery_error);
                        }
                    }
                }
                Err(error) => {
                    let error_text = error.to_string();
                    let _ = store.record_scheduled_agent_job_result(
                        &job.id,
                        "failed",
                        None,
                        Some(error_text.clone()),
                    );
                    let delivery_error =
                        deliver_scheduled_job_result(&store, job, &error_text, false).await;
                    let _ =
                        store.record_scheduled_agent_job_delivery_error(&job.id, delivery_error);
                }
            }
            return;
        }
        let prerun_script = match job.as_ref().filter(|job| job.script.is_some()) {
            Some(job) => match run_scheduled_job_script(store.inner(), job).await {
                Ok(script) if script.success && script.output.trim().is_empty() => {
                    let _ =
                        store.record_scheduled_agent_job_result(&job.id, "completed", None, None);
                    return;
                }
                Ok(script) if script.success && script_output_wakes_agent(&script.output) => {
                    Some(script)
                }
                Ok(script) if script.success => {
                    let _ = store.record_scheduled_agent_job_result(
                        &job.id,
                        "completed",
                        Some("Script gate returned wakeAgent=false; agent skipped.".into()),
                        None,
                    );
                    return;
                }
                Ok(script) => Some(script),
                Err(error) => Some(ScheduledScriptRun {
                    success: false,
                    output: error.to_string(),
                }),
            },
            None => None,
        };
        let effective_prompt = match job.as_ref() {
            Some(job) if prerun_script.is_some() => {
                match build_scheduled_job_prompt_with_script(&store, job, prerun_script.as_ref()) {
                    Ok(prompt) => prompt,
                    Err(error) => {
                        let error_text = error.to_string();
                        let _ = store.record_scheduled_agent_job_result(
                            &job.id,
                            "failed",
                            None,
                            Some(error_text.clone()),
                        );
                        let delivery_error =
                            deliver_scheduled_job_result(&store, job, &error_text, false).await;
                        let _ = store
                            .record_scheduled_agent_job_delivery_error(&job.id, delivery_error);
                        return;
                    }
                }
            }
            Some(job) => match build_scheduled_job_prompt(&store, job) {
                Ok(prompt) => prompt,
                Err(error) => {
                    let error_text = error.to_string();
                    let _ = store.record_scheduled_agent_job_result(
                        &job.id,
                        "failed",
                        None,
                        Some(error_text.clone()),
                    );
                    let delivery_error =
                        deliver_scheduled_job_result(&store, job, &error_text, false).await;
                    let _ =
                        store.record_scheduled_agent_job_delivery_error(&job.id, delivery_error);
                    return;
                }
            },
            None => prompt.clone(),
        };
        let request = SendChatRequest {
            conversation_id: Some(conversation_id),
            persona_id: Some(persona_id),
            agent_id: job.as_ref().and_then(|job| job.agent_id.clone()),
            content: effective_prompt,
            provider_data: None,
            queue_item_id: None,
        };
        let job_policy = job.as_ref().map(|job| {
            (
                job.id.clone(),
                job.enabled_toolsets.clone(),
                scheduled_job_disabled_toolsets(job),
                job.skills.clone(),
                job.provider.clone(),
                job.model.clone(),
                job.base_url.clone(),
                job.timeout_seconds,
                job.workdir.clone(),
            )
        });
        let result = run_chat_turn_with_toolset_policy_and_iteration_limit(
            &store,
            request,
            ToolExecutionContext::ScheduledJob,
            job_policy
                .as_ref()
                .map(|(_, enabled, _, _, _, _, _, _, _)| enabled.clone()),
            job_policy
                .as_ref()
                .map(|(_, _, disabled, _, _, _, _, _, _)| disabled.clone()),
            None,
            job_policy
                .as_ref()
                .and_then(|(_, _, _, _, provider, _, _, _, _)| provider.clone()),
            job_policy
                .as_ref()
                .and_then(|(_, _, _, _, _, model, _, _, _)| model.clone()),
            job_policy
                .as_ref()
                .and_then(|(_, _, _, _, _, _, base_url, _, _)| base_url.clone()),
            job_policy
                .as_ref()
                .and_then(|(_, _, _, _, _, _, _, timeout_seconds, _)| *timeout_seconds),
            None,
            job_policy
                .as_ref()
                .and_then(|(_, _, _, _, _, _, _, _, workdir)| workdir.clone()),
            job_policy
                .as_ref()
                .and_then(|(_, _, _, skills, _, _, _, _, _)| {
                    if skills.is_empty() {
                        None
                    } else {
                        Some(skills.clone())
                    }
                }),
            None,
            Some(&app),
        );
        let cron_env = job
            .as_ref()
            .and_then(|job| scheduled_cron_auto_delivery_env(&store, job));
        let _cron_env_guard = cron_env
            .as_ref()
            .map(|env| CronAutoDeliveryEnvGuard::set(env));
        let result = result.await;

        if let Some((job_id, _, _, _, _, _, _, _, _)) = job_policy {
            match result {
                Ok(messages) => {
                    let output = messages
                        .iter()
                        .rev()
                        .find(|message| message.role == "assistant")
                        .map(|message| message.content.clone())
                        .or_else(|| messages.last().map(|message| message.content.clone()));
                    let _ =
                        store.record_scheduled_agent_job_result(&job_id, "completed", output, None);
                    if let Some(job) = job.as_ref() {
                        if let Some(output) = messages
                            .iter()
                            .rev()
                            .find(|message| message.role == "assistant")
                            .map(|message| message.content.as_str())
                            .or_else(|| messages.last().map(|message| message.content.as_str()))
                            .filter(|output| !scheduled_job_output_is_silent(output))
                        {
                            let delivery_error =
                                deliver_scheduled_job_result(&store, job, output, true).await;
                            let _ = store
                                .record_scheduled_agent_job_delivery_error(&job_id, delivery_error);
                        }
                    }
                }
                Err(error) => {
                    let error_text = error.to_string();
                    let _ = store.record_scheduled_agent_job_result(
                        &job_id,
                        "failed",
                        None,
                        Some(error_text.clone()),
                    );
                    if let Some(job) = job.as_ref() {
                        let delivery_error =
                            deliver_scheduled_job_result(&store, job, &error_text, false).await;
                        let _ = store
                            .record_scheduled_agent_job_delivery_error(&job_id, delivery_error);
                    }
                }
            }
        }
    });
}

pub(super) fn scheduled_job_disabled_toolsets(job: &ScheduledAgentJob) -> Vec<String> {
    let mut disabled = vec!["cronjob".into(), "messaging".into(), "clarify".into()];
    merge_disabled_toolset_overrides(&mut disabled, job.disabled_toolsets.clone());
    disabled
}

pub(super) async fn run_scheduled_no_agent_job(
    store: &AppStore,
    job: &ScheduledAgentJob,
) -> AppResult<String> {
    let script = run_scheduled_job_script(store, job).await?;
    if !script.success {
        return Err(AppError::BadRequest(script.output));
    }
    Ok(script.output)
}

pub(super) async fn run_scheduled_job_script(
    store: &AppStore,
    job: &ScheduledAgentJob,
) -> AppResult<ScheduledScriptRun> {
    let script = job
        .script
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("noAgent scheduled job requires script".into()))?;
    let script_path = resolve_scheduled_script_path(store, script)?;
    let mut command = scheduled_script_command(&script_path);
    let current_dir = job
        .workdir
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| script_path.parent().map(PathBuf::from))
        .map(|path| normalize_command_path(&path))
        .unwrap_or_else(|| PathBuf::from("."));
    command.current_dir(current_dir);
    let timeout_seconds = job
        .script_timeout_seconds
        .or_else(|| {
            store
                .config()
                .ok()
                .map(|config| config.chat.agent_run_timeout_seconds)
        })
        .unwrap_or(600);
    let output = if timeout_seconds == 0 {
        command.output().await?
    } else {
        timeout(Duration::from_secs(timeout_seconds), command.output())
            .await
            .map_err(|_| {
                AppError::BadRequest(format!(
                    "scheduled script timed out after {timeout_seconds}s: {}",
                    script_path.display()
                ))
            })??
    };
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let output_text = if stderr.is_empty() {
        stdout
    } else if stdout.is_empty() {
        stderr
    } else {
        format!("{stdout}\n\n[stderr]\n{stderr}")
    };
    if output.status.success() {
        Ok(ScheduledScriptRun {
            success: true,
            output: output_text,
        })
    } else {
        Ok(ScheduledScriptRun {
            success: false,
            output: format!(
                "scheduled script exited with status {}: {}",
                output.status, output_text
            ),
        })
    }
}

fn resolve_scheduled_script_path(store: &AppStore, script: &str) -> AppResult<PathBuf> {
    let scripts_dir = store.data_dir().join("scripts");
    fs::create_dir_all(&scripts_dir)?;
    let scripts_dir = scripts_dir.canonicalize()?;
    let raw = PathBuf::from(script.trim());
    let candidate = if raw.is_absolute() {
        raw
    } else {
        scripts_dir.join(raw)
    };
    let resolved = candidate.canonicalize().map_err(|_| {
        AppError::NotFound(format!(
            "scheduled script not found under {}: {script}",
            scripts_dir.display()
        ))
    })?;
    if !resolved.starts_with(&scripts_dir) {
        return Err(AppError::BadRequest(format!(
            "scheduled script must stay under {}",
            scripts_dir.display()
        )));
    }
    Ok(resolved)
}

fn scheduled_script_command(script_path: &std::path::Path) -> Command {
    let command_path = normalize_command_path(script_path);
    let extension = script_path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_lowercase();
    match extension.as_str() {
        "ps1" => {
            let mut command = Command::new("powershell");
            command.hide_window();
            command
                .arg("-NoProfile")
                .arg("-ExecutionPolicy")
                .arg("Bypass")
                .arg("-File")
                .arg(&command_path);
            command
        }
        "cmd" | "bat" => {
            let mut command = Command::new("cmd");
            command.hide_window();
            command.arg("/C").arg("call").arg(&command_path);
            command
        }
        "py" => {
            let mut command = Command::new("python");
            command.hide_window();
            command.arg(&command_path);
            command
        }
        _ => {
            let mut command = Command::new(command_path);
            command.hide_window();
            command
        }
    }
}

fn normalize_command_path(path: &std::path::Path) -> PathBuf {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if let Some(stripped) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(stripped);
        }
    }
    path.to_path_buf()
}

pub(super) fn build_scheduled_job_prompt(
    store: &AppStore,
    job: &ScheduledAgentJob,
) -> AppResult<String> {
    build_scheduled_job_prompt_with_script(store, job, None)
}

pub(super) fn build_scheduled_job_prompt_with_script(
    store: &AppStore,
    job: &ScheduledAgentJob,
    prerun_script: Option<&ScheduledScriptRun>,
) -> AppResult<String> {
    let mut prompt = job.prompt.clone();
    if let Some(script) = prerun_script {
        if script.success {
            if script.output.trim().is_empty() {
                return Ok(String::new());
            }
            prompt = format!(
                "## Script Output\n\
The following data was collected by a pre-run script. Use it as context for your analysis.\n\n\
```\n{}\n```\n\n{prompt}",
                script.output.trim()
            );
        } else {
            prompt = format!(
                "## Script Error\n\
The data-collection script failed. Report this to the user.\n\n\
```\n{}\n```\n\n{prompt}",
                script.output.trim()
            );
        }
    }
    for source in &job.context_from {
        let Some((source_id, source_label)) = resolve_scheduled_context_source(store, source)?
        else {
            continue;
        };
        let Some(output) = latest_scheduled_output_context(store, &source_id)? else {
            continue;
        };
        prompt = format!(
            "## Output from scheduled job '{source_label}'\n\
The following is the most recent output from a preceding scheduled job. Use it as context for your analysis.\n\n\
```\n{output}\n```\n\n{prompt}"
        );
    }
    let prompt = format!(
        "[IMPORTANT: You are running as a scheduled cron job. Your final response will be recorded as the job output and automatically delivered when this job has a delivery target. Do not call send_message or try to deliver the output yourself. If there is genuinely nothing new to report, respond with exactly \"[SILENT]\" and nothing else.]\n\n{prompt}"
    );
    if let Some(reason) = scan_scheduled_job_assembled_prompt(&prompt, !job.skills.is_empty()) {
        return Err(AppError::BadRequest(reason));
    }
    Ok(prompt)
}

pub(super) fn script_output_wakes_agent(output: &str) -> bool {
    let Some(last) = output.lines().rev().find(|line| !line.trim().is_empty()) else {
        return true;
    };
    let Ok(value) = serde_json::from_str::<Value>(last.trim()) else {
        return true;
    };
    value
        .get("wakeAgent")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

pub(super) async fn deliver_scheduled_job_result(
    store: &AppStore,
    job: &ScheduledAgentJob,
    content: &str,
    success: bool,
) -> Option<String> {
    let targets = match resolve_scheduled_delivery_targets(store, job) {
        Ok(targets) => targets,
        Err(error) => return Some(error.to_string()),
    };
    if targets.is_empty() {
        return None;
    }
    let content = scheduled_delivery_content(store, job, content, success);
    if content.trim().is_empty() {
        return None;
    }
    let platform_content = if targets
        .iter()
        .any(scheduled_delivery_payload_targets_platform)
    {
        match scheduled_platform_delivery_content(store, job, &content) {
            Ok(content) => content,
            Err(error) => return Some(error.to_string()),
        }
    } else {
        content.clone()
    };
    let mut errors = Vec::new();
    for payload in targets {
        let mut payload = payload;
        let payload_content = if scheduled_delivery_payload_targets_platform(&payload) {
            &platform_content
        } else {
            &content
        };
        payload["action"] = json!("send");
        payload["message"] = json!(payload_content);
        payload["source"] = json!("scheduled-agent-job");
        match super::send_message_tool_async(
            store,
            job.conversation_id.as_deref().unwrap_or_default(),
            &payload,
        )
        .await
        {
            Ok(_) => {}
            Err(error) => errors.push(error.to_string()),
        }
    }
    if errors.is_empty() {
        None
    } else {
        Some(errors.join("; "))
    }
}

pub(super) fn scheduled_platform_delivery_content(
    store: &AppStore,
    job: &ScheduledAgentJob,
    content: &str,
) -> AppResult<String> {
    if content.chars().count() <= HERMES_MAX_PLATFORM_DELIVERY_OUTPUT_CHARS {
        return Ok(content.to_string());
    }
    let saved_path = store.save_scheduled_agent_job_delivery_output(&job.id, content)?;
    let visible = content
        .chars()
        .take(HERMES_TRUNCATED_PLATFORM_DELIVERY_VISIBLE_CHARS)
        .collect::<String>();
    Ok(format!(
        "{}\n\n... [truncated, full output saved to {}]",
        visible.trim_end(),
        saved_path.to_string_lossy()
    ))
}

pub(super) fn scheduled_delivery_payload_targets_platform(payload: &Value) -> bool {
    let Some(target) = payload
        .get("target")
        .or_else(|| payload.get("platform"))
        .or_else(|| payload.get("channel"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    let platform = target
        .split(':')
        .next()
        .unwrap_or(target)
        .to_ascii_lowercase();
    !matches!(
        platform.as_str(),
        "local" | "synthchat" | "desktop" | "current"
    )
}

pub(super) fn resolve_scheduled_delivery_targets(
    store: &AppStore,
    job: &ScheduledAgentJob,
) -> AppResult<Vec<Value>> {
    let deliver = job
        .deliver
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            if job.origin.is_some() {
                "origin"
            } else {
                "local"
            }
        });
    if deliver.eq_ignore_ascii_case("local") {
        return Ok(vec![]);
    }
    let mut targets = Vec::new();
    for part in deliver
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        if part.eq_ignore_ascii_case("local") {
            continue;
        } else if part.eq_ignore_ascii_case("origin") {
            if let Some(target) = scheduled_origin_delivery_target(job) {
                targets.push(target);
            } else if let Some(target) = scheduled_first_home_delivery_target(store)? {
                targets.push(target);
            }
        } else if part.eq_ignore_ascii_case("all") {
            targets.extend(scheduled_home_delivery_targets(store)?);
        } else if part.contains(':') {
            targets.push(json!({ "target": part }));
        } else if let Some(target) = scheduled_home_delivery_target(store, part)? {
            targets.push(target);
        } else {
            targets.push(json!({ "target": part }));
        }
    }
    Ok(dedupe_scheduled_delivery_targets(targets))
}

fn scheduled_home_delivery_targets(store: &AppStore) -> AppResult<Vec<Value>> {
    let mut targets = Vec::new();
    for entry in send_message_external_targets(store)? {
        if let Some(target) = entry
            .get("homeTarget")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            targets.push(json!({ "target": target }));
        }
    }
    Ok(targets)
}

fn scheduled_first_home_delivery_target(store: &AppStore) -> AppResult<Option<Value>> {
    Ok(scheduled_home_delivery_targets(store)?.into_iter().next())
}

fn scheduled_home_delivery_target(store: &AppStore, platform: &str) -> AppResult<Option<Value>> {
    let platform = platform.trim().to_ascii_lowercase();
    if platform.is_empty() {
        return Ok(None);
    }
    for entry in send_message_external_targets(store)? {
        let entry_platform = entry
            .get("platform")
            .and_then(Value::as_str)
            .map(|value| value.trim().to_ascii_lowercase());
        if entry_platform.as_deref() != Some(platform.as_str()) {
            continue;
        }
        if let Some(target) = entry
            .get("homeTarget")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Ok(Some(json!({ "target": target })));
        }
    }
    Ok(None)
}

fn dedupe_scheduled_delivery_targets(targets: Vec<Value>) -> Vec<Value> {
    let mut deduped = Vec::new();
    for target in targets {
        let key = target
            .get("target")
            .and_then(Value::as_str)
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| target.to_string());
        if deduped.iter().any(|existing: &Value| {
            existing
                .get("target")
                .and_then(Value::as_str)
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| existing.to_string())
                == key
        }) {
            continue;
        }
        deduped.push(target);
    }
    deduped
}

#[derive(Debug, Clone)]
struct CronAutoDeliveryEnv {
    platform: String,
    chat_id: String,
    thread_id: Option<String>,
}

fn scheduled_cron_auto_delivery_env(
    store: &AppStore,
    job: &ScheduledAgentJob,
) -> Option<CronAutoDeliveryEnv> {
    let target = resolve_scheduled_delivery_targets(store, job)
        .ok()?
        .into_iter()
        .next()?;
    let target = target.get("target")?.as_str()?.trim();
    parse_scheduled_delivery_target_for_env(target)
}

fn parse_scheduled_delivery_target_for_env(target: &str) -> Option<CronAutoDeliveryEnv> {
    let mut parts = target.split(':');
    let platform = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_ascii_lowercase();
    if matches!(platform.as_str(), "current" | "synthchat" | "local") {
        return None;
    }
    let chat_id = parts.next()?.trim();
    if chat_id.is_empty() {
        return None;
    }
    let thread_id = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    Some(CronAutoDeliveryEnv {
        platform,
        chat_id: chat_id.to_string(),
        thread_id,
    })
}

struct CronAutoDeliveryEnvGuard {
    previous: Vec<(&'static str, Option<OsString>)>,
}

impl CronAutoDeliveryEnvGuard {
    fn set(values: &CronAutoDeliveryEnv) -> Self {
        let keys = [
            "HERMES_CRON_AUTO_DELIVER_PLATFORM",
            "HERMES_CRON_AUTO_DELIVER_CHAT_ID",
            "HERMES_CRON_AUTO_DELIVER_THREAD_ID",
            "SYNTHCHAT_CRON_AUTO_DELIVER_PLATFORM",
            "SYNTHCHAT_CRON_AUTO_DELIVER_CHAT_ID",
            "SYNTHCHAT_CRON_AUTO_DELIVER_THREAD_ID",
        ];
        let previous = keys
            .into_iter()
            .map(|key| (key, env::var_os(key)))
            .collect::<Vec<_>>();
        env::set_var("HERMES_CRON_AUTO_DELIVER_PLATFORM", &values.platform);
        env::set_var("HERMES_CRON_AUTO_DELIVER_CHAT_ID", &values.chat_id);
        if let Some(thread_id) = values.thread_id.as_deref() {
            env::set_var("HERMES_CRON_AUTO_DELIVER_THREAD_ID", thread_id);
        } else {
            env::remove_var("HERMES_CRON_AUTO_DELIVER_THREAD_ID");
        }
        env::set_var("SYNTHCHAT_CRON_AUTO_DELIVER_PLATFORM", &values.platform);
        env::set_var("SYNTHCHAT_CRON_AUTO_DELIVER_CHAT_ID", &values.chat_id);
        if let Some(thread_id) = values.thread_id.as_deref() {
            env::set_var("SYNTHCHAT_CRON_AUTO_DELIVER_THREAD_ID", thread_id);
        } else {
            env::remove_var("SYNTHCHAT_CRON_AUTO_DELIVER_THREAD_ID");
        }
        Self { previous }
    }
}

impl Drop for CronAutoDeliveryEnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..) {
            if let Some(value) = value {
                env::set_var(key, value);
            } else {
                env::remove_var(key);
            }
        }
    }
}

fn scheduled_origin_delivery_target(job: &ScheduledAgentJob) -> Option<Value> {
    let origin = job.origin.as_ref()?;
    let platform = origin
        .get("platform")
        .and_then(Value::as_str)
        .unwrap_or("synthchat")
        .trim()
        .to_ascii_lowercase();
    if matches!(platform.as_str(), "synthchat" | "local" | "desktop") {
        let conversation_id = origin
            .get("conversationId")
            .or_else(|| origin.get("conversation_id"))
            .and_then(Value::as_str)
            .or(job.conversation_id.as_deref())?;
        return Some(json!({ "target": conversation_id }));
    }
    let chat_id = origin
        .get("chatId")
        .or_else(|| origin.get("chat_id"))
        .or_else(|| origin.get("channelId"))
        .or_else(|| origin.get("channel_id"))
        .or_else(|| origin.get("roomId"))
        .or_else(|| origin.get("room_id"))
        .or_else(|| origin.get("receiveId"))
        .or_else(|| origin.get("receive_id"))
        .or_else(|| origin.get("recipient"))
        .or_else(|| origin.get("to"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let thread_id = origin
        .get("threadId")
        .or_else(|| origin.get("thread_id"))
        .or_else(|| origin.get("messageThreadId"))
        .or_else(|| origin.get("message_thread_id"))
        .or_else(|| origin.get("rootId"))
        .or_else(|| origin.get("root_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let target = if let Some(thread_id) = thread_id {
        format!("{platform}:{chat_id}:{thread_id}")
    } else {
        format!("{platform}:{chat_id}")
    };
    Some(json!({ "target": target }))
}

pub(super) fn scheduled_delivery_content(
    store: &AppStore,
    job: &ScheduledAgentJob,
    content: &str,
    success: bool,
) -> String {
    let body = if success {
        content.trim().to_string()
    } else {
        format!(
            "Cron job '{}' failed:\n{}",
            scheduled_job_context_label(job),
            content.trim()
        )
    };
    if !store
        .config()
        .map(|config| config.chat.runtime_footer_enabled)
        .unwrap_or(false)
    {
        return body;
    }
    append_scheduled_runtime_footer(job, &body, success)
}

fn append_scheduled_runtime_footer(job: &ScheduledAgentJob, body: &str, success: bool) -> String {
    let status = if success { "success" } else { "failed" };
    let model = job
        .model
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("default");
    let provider = job
        .provider
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("default");
    let workdir = job
        .workdir
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("-");
    format!(
        "{}\n\n---\nruntime: job={} · status={} · provider={} · model={} · cwd={}",
        body.trim(),
        scheduled_job_context_label(job),
        status,
        provider,
        model,
        workdir
    )
}

pub(super) fn scheduled_job_output_is_silent(output: &str) -> bool {
    output.trim().eq_ignore_ascii_case("[SILENT]")
}

fn resolve_scheduled_context_source(
    store: &AppStore,
    selector: &str,
) -> AppResult<Option<(String, String)>> {
    let selector = selector.trim();
    if selector.is_empty() {
        return Ok(None);
    }
    let jobs = store.scheduled_agent_jobs()?;
    if let Some(job) = jobs.iter().find(|job| job.id == selector) {
        return Ok(Some((job.id.clone(), scheduled_job_context_label(job))));
    }
    let matches = jobs
        .iter()
        .filter(|job| job.id.starts_with(selector) || job.name.eq_ignore_ascii_case(selector))
        .collect::<Vec<_>>();
    Ok(match matches.as_slice() {
        [job] => Some((job.id.clone(), scheduled_job_context_label(job))),
        _ => None,
    })
}

fn scheduled_job_context_label(job: &ScheduledAgentJob) -> String {
    if job.name.trim().is_empty() {
        job.id.clone()
    } else {
        format!("{} ({})", job.name.trim(), job.id)
    }
}

fn latest_scheduled_output_context(store: &AppStore, job_id: &str) -> AppResult<Option<String>> {
    let Some(record) = store.scheduled_job_outputs(job_id)?.into_iter().next() else {
        return Ok(None);
    };
    let content = fs::read_to_string(record.path)?;
    let content = content.trim();
    if content.is_empty() {
        return Ok(None);
    }
    Ok(Some(truncate_scheduled_context(content, 8000)))
}

fn truncate_scheduled_context(content: &str, max_chars: usize) -> String {
    let mut output = String::new();
    for ch in content.chars().take(max_chars) {
        output.push(ch);
    }
    if content.chars().count() > max_chars {
        output.push_str("\n\n[... output truncated ...]");
    }
    output
}

pub fn export_agent_run_bundle(store: &AppStore, run_id: String) -> AppResult<String> {
    let run = store.agent_run(&run_id)?;
    let trajectory = export_agent_run_trajectory(store, &run)?;
    let child_runs = store
        .agent_runs()?
        .into_iter()
        .filter(|item| item.parent_run_id.as_deref() == Some(&run_id))
        .collect::<Vec<_>>();
    let planner_traces = store
        .planner_traces()?
        .into_iter()
        .filter(|trace| trace.run_id == run_id)
        .collect::<Vec<_>>();
    let tool_traces = store
        .tool_traces()?
        .into_iter()
        .filter(|trace| trace.event.run_id.as_deref() == Some(&run_id))
        .collect::<Vec<_>>();
    let approvals = store
        .tool_approvals()?
        .into_iter()
        .filter(|approval| approval.run_id.as_deref() == Some(&run_id))
        .collect::<Vec<_>>();
    let artifacts = store.tool_artifacts_for_run(&run_id)?;
    let todos = store.agent_todos_for_run(&run_id)?;
    Ok(serde_json::to_string_pretty(&json!({
        "run": run,
        "childRuns": child_runs,
        "artifacts": artifacts,
        "todos": todos,
        "plannerTraces": planner_traces,
        "toolTraces": tool_traces,
        "approvals": approvals,
        "trajectory": trajectory,
        "recoveryBaseline": true
    }))?)
}

pub(super) fn export_agent_run_trajectory(
    store: &AppStore,
    run: &AgentRunRecord,
) -> AppResult<Value> {
    let mut conversations = store
        .messages(&run.conversation_id, None)?
        .into_iter()
        .filter(|message| message.created_at >= run.started_at)
        .filter_map(|message| trajectory_message_from_chat_message(&message))
        .collect::<Vec<_>>();
    let has_user_request = conversations.iter().any(|message| {
        message
            .get("from")
            .and_then(Value::as_str)
            .is_some_and(|role| role == "human")
    });
    if !has_user_request && !run.user_request.trim().is_empty() {
        conversations.insert(
            0,
            json!({
                "from": "human",
                "value": normalize_trajectory_content(&run.user_request),
            }),
        );
    }
    Ok(json!({
        "conversations": conversations,
        "timestamp": now_iso(),
        "model": run.agent_id,
        "completed": run.state == "completed",
        "source": "synthchat-agent-run-bundle"
    }))
}

fn trajectory_message_from_chat_message(message: &ChatMessage) -> Option<Value> {
    let role = match message.role.as_str() {
        "system" => "system",
        "user" => "human",
        "assistant" => "gpt",
        "tool" => "tool",
        _ => return None,
    };
    let value = match message.role.as_str() {
        "assistant" => trajectory_assistant_content(message),
        "tool" => format!(
            "<tool_response>\n{}\n</tool_response>",
            normalize_trajectory_content(&message.content)
        ),
        _ => normalize_trajectory_content(&message.content),
    };
    if value.trim().is_empty() {
        return None;
    }
    Some(json!({
        "from": role,
        "value": value,
    }))
}

fn trajectory_assistant_content(message: &ChatMessage) -> String {
    let mut content = String::new();
    if let Some(reasoning) = provider_reasoning_text(message.provider_data.as_ref()) {
        content.push_str("<think>\n");
        content.push_str(reasoning.trim());
        content.push_str("\n</think>\n");
    }
    content.push_str(&convert_scratchpad_to_think(&normalize_trajectory_content(
        &message.content,
    )));
    if !content.contains("<think>") {
        content = format!("<think>\n</think>\n{}", content);
    }
    content.trim().to_string()
}

fn provider_reasoning_text(provider_data: Option<&Value>) -> Option<&str> {
    let data = provider_data?;
    data.get("openai")
        .and_then(|openai| openai.get("reasoning_content"))
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
}

fn convert_scratchpad_to_think(content: &str) -> String {
    content
        .replace("<REASONING_SCRATCHPAD>", "<think>")
        .replace("</REASONING_SCRATCHPAD>", "</think>")
}

fn normalize_trajectory_content(content: &str) -> String {
    if content.contains("data:image/") || content.contains("\"type\":\"image") {
        "[screenshot]".into()
    } else {
        content.to_string()
    }
}

pub fn list_agent_run_artifacts(store: &AppStore, run_id: String) -> AppResult<Vec<Value>> {
    store.tool_artifacts_for_run(&run_id)
}

pub fn list_agent_artifact_index(
    store: &AppStore,
    conversation_id: Option<&str>,
    limit: usize,
) -> AppResult<Vec<Value>> {
    let scan_limit = limit.max(1).saturating_mul(10).max(100);
    let mut artifacts = store.tool_artifact_index(scan_limit)?;
    if let Some(conversation_id) = conversation_id {
        let run_ids = store
            .agent_runs()?
            .into_iter()
            .filter(|run| run.conversation_id == conversation_id)
            .map(|run| run.run_id)
            .collect::<std::collections::HashSet<_>>();
        artifacts.retain(|artifact| {
            artifact
                .get("runId")
                .and_then(Value::as_str)
                .is_some_and(|run_id| run_ids.contains(run_id))
        });
    }
    if artifacts.len() > limit {
        artifacts.truncate(limit);
    }
    Ok(artifacts)
}

pub async fn drain_all_agent_queues(
    store: &AppStore,
    app: Option<&AppHandle>,
) -> AppResult<Vec<crate::models::AgentQueuedRequest>> {
    let mut drained = Vec::new();
    while let Some(item) = store.claim_next_agent_request("")? {
        emit_agent_queue_event(app, "claimed", Some(&item), Some(&item.conversation_id));
        let request = SendChatRequest {
            conversation_id: Some(item.conversation_id.clone()),
            persona_id: Some(item.persona_id.clone()),
            agent_id: None,
            content: item.content.clone(),
            provider_data: item.request_provider_data(),
            queue_item_id: Some(item.id.clone()),
        };
        let (status, error) = match run_chat_turn(store, request, app).await {
            Ok(messages) => {
                crate::wechat_settings::finalize_queued_wechat_turn(
                    store,
                    &messages,
                    item.provider_data.as_ref(),
                    item.started_at.as_deref(),
                )
                .await?;
                ("completed", None)
            }
            Err(error) => ("failed", Some(error.to_string())),
        };
        let mut completed = store
            .complete_agent_queue_item(&item.id, status, error.clone())?
            .unwrap_or_else(|| {
                let mut fallback = item;
                fallback.status = status.into();
                fallback.error = error;
                fallback.updated_at = now_iso();
                fallback.completed_at = Some(now_iso());
                fallback
            });
        if completed.status == "canceled" {
            completed
                .error
                .get_or_insert_with(|| "Canceled by user.".into());
        }
        record_agent_queue_workflow_terminal(store, &completed)?;
        emit_agent_queue_event(
            app,
            &completed.status,
            Some(&completed),
            Some(&completed.conversation_id),
        );
        drained.push(completed);
    }
    Ok(drained)
}

pub(crate) fn record_agent_queue_workflow_terminal(
    store: &AppStore,
    item: &AgentQueuedRequest,
) -> AppResult<()> {
    let status = item.status.trim().to_ascii_lowercase();
    if !matches!(
        status.as_str(),
        "completed" | "failed" | "canceled" | "cancelled"
    ) {
        return Ok(());
    }
    let Some(run) = store
        .agent_runs()?
        .into_iter()
        .filter(|run| run.queue_item_id.as_deref() == Some(item.id.as_str()))
        .max_by(|left, right| left.updated_at.cmp(&right.updated_at))
    else {
        return Ok(());
    };
    WorkflowDriver::new(workflow_mode_for_run(&run))
        .queue()
        .terminal(store, &run.run_id, &item.id, &status, item.error.as_deref())
}

pub async fn resume_agent_run(
    store: &AppStore,
    run_id: String,
    checkpoint_id: Option<String>,
    app: Option<&AppHandle>,
) -> AppResult<AgentRunRecord> {
    let mut run = store.agent_run(&run_id)?;
    validate_run_resume_allowed(store, &run, checkpoint_id.as_deref())?;
    let observations = resume_observations(&run, checkpoint_id.as_deref())?;
    let original_request = if run.user_request.trim().is_empty() {
        store
            .messages(&run.conversation_id, None)?
            .into_iter()
            .rev()
            .find(|message| message.role == "user")
            .map(|message| message.content)
            .ok_or_else(|| AppError::BadRequest("cannot resume without original request".into()))?
    } else {
        run.user_request.clone()
    };
    record_resume_checkpoint_waiting(store, &run, checkpoint_id.as_deref())?;
    run = store.agent_run(&run_id)?;
    let resume_prompt = format!(
        "Resume the prior agent run and continue the user's task.\n\nOriginal request:\n{}\n\nResume observations:\n{}\n\nContinue from the saved state. If more tool work is impossible, explain the current evidence and remaining blocker.",
        original_request,
        observations.join("\n\n")
    );
    run.state = "running".into();
    run.error = None;
    run.completed_at = None;
    run.updated_at = now_iso();
    store.save_agent_run(run.clone())?;

    let result = Box::pin(run_chat_turn_with_app(
        store,
        SendChatRequest {
            conversation_id: Some(run.conversation_id.clone()),
            persona_id: Some(run.persona_id.clone()),
            agent_id: None,
            content: resume_prompt,
            provider_data: None,
            queue_item_id: None,
        },
        ToolExecutionContext::Interactive,
        app,
    ))
    .await;

    let mut run = store.agent_run(&run_id)?;
    let now = now_iso();
    let resume_checkpoint = match result {
        Ok(messages) => {
            let summary = messages
                .iter()
                .rev()
                .find(|message| message.role == "assistant")
                .map(|message| truncate_for_prompt(&message.content.replace('\n', " "), 240))
                .unwrap_or_else(|| "Resume turn completed.".into());
            let checkpoint = AgentCheckpointRecord {
                checkpoint_id: new_id("ckpt"),
                run_id: run.run_id.clone(),
                iteration: run.checkpoints.len() as u32 + 1,
                created_at: now.clone(),
                state: "resumed".into(),
                completed_call_ids: Vec::new(),
                event_refs: Vec::new(),
                summary,
            };
            run.checkpoints.push(checkpoint.clone());
            run.state = "completed".into();
            run.error = None;
            run.completed_at = Some(now.clone());
            checkpoint
        }
        Err(error) => {
            let checkpoint = AgentCheckpointRecord {
                checkpoint_id: new_id("ckpt"),
                run_id: run.run_id.clone(),
                iteration: run.checkpoints.len() as u32 + 1,
                created_at: now.clone(),
                state: "resume_failed".into(),
                completed_call_ids: Vec::new(),
                event_refs: Vec::new(),
                summary: error.to_string(),
            };
            run.checkpoints.push(checkpoint.clone());
            run.state = "failed".into();
            run.error = Some(error.to_string());
            run.completed_at = Some(now.clone());
            checkpoint
        }
    };
    run.updated_at = now;
    let saved = store.save_agent_run(run)?;
    record_resume_checkpoint_result(store, &saved, &resume_checkpoint)?;
    store.agent_run(&saved.run_id)
}

pub(super) fn record_resume_checkpoint_waiting(
    store: &AppStore,
    run: &AgentRunRecord,
    checkpoint_id: Option<&str>,
) -> AppResult<()> {
    let selected_checkpoint = resume_checkpoint_for_detail(run, checkpoint_id);
    let summary = selected_checkpoint
        .map(|checkpoint| {
            format!(
                "Resume requested from checkpoint {} ({})",
                checkpoint.checkpoint_id, checkpoint.state
            )
        })
        .unwrap_or_else(|| "Resume requested from latest run state".into());
    let mut detail = json!({
        "kind": "resume_checkpoint",
        "checkpointScope": "resume",
        "previousState": run.state.as_str(),
    });
    if let Some(checkpoint) = selected_checkpoint {
        detail["checkpointId"] = json!(checkpoint.checkpoint_id.as_str());
        detail["checkpointState"] = json!(checkpoint.state.as_str());
        detail["checkpointSummary"] = json!(checkpoint.summary.as_str());
        detail["checkpointIteration"] = json!(checkpoint.iteration);
    }
    let checkpoint_node = WorkflowDriver::new(workflow_mode_for_run(run)).checkpoint();
    let mut transition_detail = detail.clone();
    transition_detail["state"] = json!("resume_started");
    transition_detail["summary"] = json!(summary.as_str());
    checkpoint_node.resume_requested_from_current(store, run, transition_detail)?;
    checkpoint_node.waiting(store, &run.run_id, "resume_started", &summary, detail)
}

pub(super) fn record_resume_checkpoint_result(
    store: &AppStore,
    run: &AgentRunRecord,
    checkpoint: &AgentCheckpointRecord,
) -> AppResult<()> {
    let mut detail = json!({
        "kind": "resume_checkpoint",
        "checkpointScope": "resume",
        "checkpointId": checkpoint.checkpoint_id.as_str(),
        "checkpointIteration": checkpoint.iteration,
        "runState": run.state.as_str(),
        "state": checkpoint.state.as_str(),
        "summary": checkpoint.summary.as_str(),
    });
    let checkpoint_node = WorkflowDriver::new(workflow_mode_for_run(run)).checkpoint();
    if checkpoint.state == "resume_failed" {
        checkpoint_node.failed(
            store,
            &run.run_id,
            &checkpoint.state,
            &checkpoint.summary,
            detail,
        )
    } else {
        checkpoint_node.resume_continued_to_planner(store, &run.run_id, detail.clone())?;
        detail["preserveCurrent"] = json!(true);
        checkpoint_node.completed(
            store,
            &run.run_id,
            &checkpoint.state,
            &checkpoint.summary,
            detail,
        )
    }
}

fn resume_checkpoint_for_detail<'a>(
    run: &'a AgentRunRecord,
    checkpoint_id: Option<&str>,
) -> Option<&'a AgentCheckpointRecord> {
    checkpoint_id
        .and_then(|id| {
            run.checkpoints.iter().find(|checkpoint| {
                checkpoint.checkpoint_id == id || checkpoint.checkpoint_id.starts_with(id)
            })
        })
        .or_else(|| run.checkpoints.last())
}

pub(super) fn validate_run_resume_allowed(
    store: &AppStore,
    run: &AgentRunRecord,
    checkpoint_id: Option<&str>,
) -> AppResult<()> {
    match run.state.as_str() {
        "completed" => {
            return Err(AppError::BadRequest(
                "completed agent run cannot be resumed; start a new request instead".into(),
            ));
        }
        "running" | "started" => {
            return Err(AppError::BadRequest(format!(
                "agent run is already active: {}",
                run.state
            )));
        }
        "aborted" => {
            return Err(AppError::BadRequest(
                "aborted agent run cannot be resumed safely".into(),
            ));
        }
        "pendingApproval" => {
            let has_pending_approval = store.tool_approvals()?.iter().any(|approval| {
                approval.run_id.as_deref() == Some(run.run_id.as_str())
                    && approval.status == "pending"
            });
            if has_pending_approval {
                return Err(AppError::BadRequest(
                    "agent run is waiting for tool approval; approve or deny the pending tool call first"
                        .into(),
                ));
            }
        }
        _ => {}
    }
    if let Some(id) = checkpoint_id {
        let checkpoint = run
            .checkpoints
            .iter()
            .find(|checkpoint| {
                checkpoint.checkpoint_id == id || checkpoint.checkpoint_id.starts_with(id)
            })
            .ok_or_else(|| AppError::NotFound(format!("checkpoint {id}")))?;
        if checkpoint.state == "completed" {
            return Err(AppError::BadRequest(
                "completed checkpoint is not a resumable interruption point".into(),
            ));
        }
    }
    Ok(())
}

pub(super) fn resume_observations(
    run: &AgentRunRecord,
    checkpoint_id: Option<&str>,
) -> AppResult<Vec<String>> {
    let checkpoint = if let Some(id) = checkpoint_id {
        Some(
            run.checkpoints
                .iter()
                .find(|checkpoint| {
                    checkpoint.checkpoint_id == id || checkpoint.checkpoint_id.starts_with(id)
                })
                .ok_or_else(|| AppError::NotFound(format!("checkpoint {id}")))?,
        )
    } else {
        run.checkpoints.last()
    };
    let checkpoint_text = checkpoint
        .map(|checkpoint| {
            format!(
                "{} [{}]: {}",
                checkpoint.checkpoint_id, checkpoint.state, checkpoint.summary
            )
        })
        .unwrap_or_else(|| "none".into());
    Ok(vec![format!(
        "Resuming agent run {}; previousState={}; checkpoint={}; runError={}",
        run.run_id,
        run.state,
        checkpoint_text,
        run.error.as_deref().unwrap_or("")
    )])
}

pub async fn rerun_agent_run(
    store: &AppStore,
    run_id: String,
    app: Option<&AppHandle>,
) -> AppResult<Vec<ChatMessage>> {
    let run = store.agent_run(&run_id)?;
    run_chat_turn(
        store,
        SendChatRequest {
            conversation_id: Some(run.conversation_id),
            persona_id: Some(run.persona_id),
            agent_id: None,
            content: run.user_request,
            provider_data: None,
            queue_item_id: None,
        },
        app,
    )
    .await
}

pub async fn diagnose_agent_run(
    store: &AppStore,
    run_id: String,
    _app: Option<&AppHandle>,
) -> AppResult<ChatMessage> {
    let run = store.agent_run(&run_id)?;
    let content = build_agent_run_diagnosis_report(store, &run)?;
    Ok(store.append_message(ChatMessage::new(
        run.conversation_id,
        "assistant",
        content,
        "desktop-diagnosis",
    ))?)
}

pub(super) fn build_agent_run_diagnosis_report(
    store: &AppStore,
    run: &AgentRunRecord,
) -> AppResult<String> {
    let planner_traces = store
        .planner_traces()?
        .into_iter()
        .filter(|trace| trace.run_id == run.run_id)
        .collect::<Vec<_>>();
    let tool_traces = store
        .tool_traces()?
        .into_iter()
        .filter(|trace| trace.event.run_id.as_deref() == Some(run.run_id.as_str()))
        .collect::<Vec<_>>();
    let approvals = store
        .tool_approvals()?
        .into_iter()
        .filter(|approval| approval.run_id.as_deref() == Some(run.run_id.as_str()))
        .collect::<Vec<_>>();
    let todos = store.agent_todos_for_run(&run.run_id)?;
    let scheduled_jobs = store
        .scheduled_agent_jobs()?
        .into_iter()
        .filter(|job| job.conversation_id.as_deref() == Some(run.conversation_id.as_str()))
        .collect::<Vec<_>>();

    let failed_tools = tool_traces
        .iter()
        .filter(|trace| !trace.ok || trace.error.is_some() || trace.event.error.is_some())
        .collect::<Vec<_>>();
    let pending_approvals = approvals
        .iter()
        .filter(|approval| approval.status == "pending")
        .collect::<Vec<_>>();
    let blocked_todos = todos
        .iter()
        .filter(|todo| todo.status == "blocked")
        .collect::<Vec<_>>();
    let incomplete_todos = todos
        .iter()
        .filter(|todo| todo.status != "completed")
        .count();

    let conclusion =
        if run.state == "completed" && failed_tools.is_empty() && pending_approvals.is_empty() {
            "该 run 已完成，当前证据中没有未处理的失败工具或待审批。"
        } else if !pending_approvals.is_empty() {
            "该 run 卡在工具审批或审批后的继续执行路径，需要先处理待审批项。"
        } else if !failed_tools.is_empty() {
            "该 run 存在失败工具调用，失败工具很可能是任务未成功完成的直接原因。"
        } else if run.state == "failed" {
            "该 run 标记为 failed，但当前工具证据不足，需要结合 planner/checkpoint 继续排查。"
        } else if run.state == "aborted" {
            "该 run 已被中止，不能按自然完成路径判断任务成功。"
        } else {
            "该 run 未被证据证明已完整完成，需要继续检查 planner、checkpoint 和后续输出。"
        };

    let latest_checkpoint = run
        .checkpoints
        .last()
        .map(|checkpoint| {
            format!(
                "{} [{}] {}",
                checkpoint.checkpoint_id,
                checkpoint.state,
                truncate_for_prompt(&checkpoint.summary.replace('\n', " "), 220)
            )
        })
        .unwrap_or_else(|| "无 checkpoint".into());
    let recent_planner = planner_traces
        .iter()
        .rev()
        .take(3)
        .map(|trace| {
            format!(
                "- #{} {} parsed={} error={}",
                trace.iteration,
                trace.created_at,
                trace.parsed_step,
                trace.error.as_deref().unwrap_or("")
            )
        })
        .collect::<Vec<_>>();
    let recent_tools = tool_traces
        .iter()
        .rev()
        .take(5)
        .map(|trace| {
            format!(
                "- {}.{} ok={} timedOut={} kind={} summary={} error={}",
                trace.server_id,
                trace.tool_name,
                trace.ok,
                trace.timed_out,
                trace.event.kind,
                truncate_for_prompt(&trace.event.summary.replace('\n', " "), 140),
                trace
                    .error
                    .as_deref()
                    .or(trace.event.error.as_deref())
                    .unwrap_or("")
            )
        })
        .collect::<Vec<_>>();
    let approval_rows = approvals
        .iter()
        .take(8)
        .map(|approval| {
            format!(
                "- {} {}.{} status={} reason={}",
                approval.id,
                approval.server_id,
                approval.tool_name,
                approval.status,
                truncate_for_prompt(&approval.reason.replace('\n', " "), 120)
            )
        })
        .collect::<Vec<_>>();
    let scheduled_rows = scheduled_jobs
        .iter()
        .take(5)
        .map(|job| {
            format!(
                "- {} [{}] enabled={} lastStatus={} error={}",
                job.id,
                job.name,
                job.enabled,
                job.last_run_status.as_deref().unwrap_or("-"),
                job.last_error.as_deref().unwrap_or("")
            )
        })
        .collect::<Vec<_>>();
    let workflow_diagnosis = workflow_graph_diagnosis(run.workflow_graph.as_ref());

    let mut root_causes = Vec::new();
    if let Some(error) = run
        .error
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        root_causes.push(format!("run.error: {error}"));
    }
    if !pending_approvals.is_empty() {
        root_causes.push(format!(
            "仍有 {} 个待审批工具调用。",
            pending_approvals.len()
        ));
    }
    if !failed_tools.is_empty() {
        root_causes.push(format!("失败工具调用数量：{}。", failed_tools.len()));
    }
    if !blocked_todos.is_empty() {
        root_causes.push(format!("blocked todo 数量：{}。", blocked_todos.len()));
    }
    if !workflow_diagnosis.failed_nodes.is_empty() {
        root_causes.push(format!(
            "workflow failed 节点：{}。",
            workflow_diagnosis.failed_nodes.join(", ")
        ));
    }
    if !workflow_diagnosis.waiting_nodes.is_empty() {
        root_causes.push(format!(
            "workflow waiting 节点：{}。",
            workflow_diagnosis.waiting_nodes.join(", ")
        ));
    }
    if !workflow_diagnosis.canceled_nodes.is_empty() {
        root_causes.push(format!(
            "workflow canceled 节点：{}。",
            workflow_diagnosis.canceled_nodes.join(", ")
        ));
    }
    if root_causes.is_empty() {
        root_causes.push(
            "当前证据没有单一明确根因；优先检查 workflow 当前节点、最后一个 checkpoint 和最近 planner 决策。".into(),
        );
    }

    let mut next_steps = Vec::new();
    if !pending_approvals.is_empty() {
        next_steps.push("先用 /approve、/always、/trust-server 或 /deny 处理待审批项。");
    }
    if run.state == "failed" || !failed_tools.is_empty() {
        next_steps.push("针对失败工具的 payload、权限、网络和配置重跑最小复现。");
    }
    if !workflow_diagnosis.failed_nodes.is_empty() {
        next_steps.push(
            "从 workflow failed 节点的 detail 反查 planner/executor/reviewer 的直接失败原因。",
        );
    }
    if !workflow_diagnosis.waiting_nodes.is_empty() {
        next_steps.push("先处理 workflow waiting 节点对应的审批、checkpoint 或澄清阻塞。");
    }
    if !workflow_diagnosis.canceled_nodes.is_empty() {
        next_steps.push("检查 workflow canceled 节点的 detail.reason，确认是用户中断、审批拒绝还是上游会话取消。");
    }
    if !run.checkpoints.is_empty() && !matches!(run.state.as_str(), "completed" | "aborted") {
        next_steps.push("可用 /resume <runId前缀> [checkpointId前缀] 从最近可恢复点继续。");
    }
    if incomplete_todos > 0 {
        next_steps.push("检查未完成 todo，确认是否需要继续工具调用或拆分子任务。");
    }
    if next_steps.is_empty() {
        next_steps.push("如用户仍认为任务失败，导出 run bundle 对比最终回复和原始需求。");
    }

    Ok(format!(
        "1) 结论\n{}\n\n2) 关键证据\n- run: {} state={} started={} updated={} completed={}\n- request: {}\n- latestCheckpoint: {}\n- checkpoints: {}\n- workflowGraph: {} nodes={} transitions={}\n- plannerTraces: {}\n- toolTraces: {} (failed {})\n- approvals: {} (pending {})\n- todos: {} (incomplete {})\n- scheduledJobsForConversation: {}\n\nWorkflow graph：\n{}\n\n最近 workflow transitions：\n{}\n\n最近 planner：\n{}\n\n最近工具：\n{}\n\n审批：\n{}\n\n计划任务：\n{}\n\n3) 根因\n{}\n\n4) 下一步修复建议\n{}",
        conclusion,
        run.run_id,
        run.state,
        run.started_at,
        run.updated_at,
        run.completed_at.as_deref().unwrap_or("-"),
        truncate_for_prompt(&run.user_request.replace('\n', " "), 220),
        latest_checkpoint,
        run.checkpoints.len(),
        workflow_diagnosis.summary,
        workflow_diagnosis.node_count,
        workflow_diagnosis.transition_count,
        planner_traces.len(),
        tool_traces.len(),
        failed_tools.len(),
        approvals.len(),
        pending_approvals.len(),
        todos.len(),
        incomplete_todos,
        scheduled_jobs.len(),
        format_list_or_dash(workflow_diagnosis.node_rows),
        format_list_or_dash(workflow_diagnosis.transition_rows),
        format_list_or_dash(recent_planner),
        format_list_or_dash(recent_tools),
        format_list_or_dash(approval_rows),
        format_list_or_dash(scheduled_rows),
        root_causes
            .into_iter()
            .map(|item| format!("- {item}"))
            .collect::<Vec<_>>()
            .join("\n"),
        next_steps
            .into_iter()
            .map(|item| format!("- {item}"))
            .collect::<Vec<_>>()
            .join("\n")
    ))
}

struct WorkflowGraphDiagnosis {
    summary: String,
    node_rows: Vec<String>,
    transition_rows: Vec<String>,
    failed_nodes: Vec<String>,
    waiting_nodes: Vec<String>,
    canceled_nodes: Vec<String>,
    node_count: usize,
    transition_count: usize,
}

fn workflow_graph_diagnosis(graph: Option<&Value>) -> WorkflowGraphDiagnosis {
    let Some(graph) = graph.and_then(Value::as_object) else {
        return WorkflowGraphDiagnosis {
            summary: "未捕获 workflowGraph".into(),
            node_rows: Vec::new(),
            transition_rows: Vec::new(),
            failed_nodes: Vec::new(),
            waiting_nodes: Vec::new(),
            canceled_nodes: Vec::new(),
            node_count: 0,
            transition_count: 0,
        };
    };
    let nodes = graph.get("nodes").and_then(Value::as_array);
    let transitions = graph.get("transitions").and_then(Value::as_array);
    let current_node = graph
        .get("currentNode")
        .or_else(|| graph.get("current_node"))
        .and_then(Value::as_str)
        .unwrap_or("-");
    let current_status = graph
        .get("currentStatus")
        .or_else(|| graph.get("current_status"))
        .and_then(Value::as_str)
        .or_else(|| {
            nodes
                .and_then(|nodes| {
                    nodes
                        .iter()
                        .find(|node| node.get("node").and_then(Value::as_str) == Some(current_node))
                })
                .and_then(|node| node.get("status"))
                .and_then(Value::as_str)
        })
        .unwrap_or("-");
    let status_summary = workflow_status_summary(nodes.map(Vec::as_slice));
    let sequence = graph
        .get("lastEventSequence")
        .or_else(|| graph.get("last_event_sequence"))
        .and_then(Value::as_u64)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".into());
    let schema = graph
        .get("schema")
        .and_then(Value::as_str)
        .unwrap_or("workflow");
    let mode = graph.get("mode").and_then(Value::as_str).unwrap_or("-");
    let node_rows = nodes
        .into_iter()
        .flatten()
        .map(|node| {
            let name = node.get("node").and_then(Value::as_str).unwrap_or("-");
            let status = node.get("status").and_then(Value::as_str).unwrap_or("-");
            let updated = node
                .get("updatedAt")
                .or_else(|| node.get("updated_at"))
                .and_then(Value::as_str)
                .unwrap_or("-");
            let detail = workflow_compact_detail(node.get("detail"), 160);
            format!(
                "- {} status={} role={} updated={} detail={}",
                workflow_node_label(name),
                status,
                workflow_node_role_label(name),
                updated,
                if detail.is_empty() {
                    "-"
                } else {
                    detail.as_str()
                }
            )
        })
        .collect::<Vec<_>>();
    let failed_nodes = workflow_nodes_with_status(nodes.map(Vec::as_slice), "failed");
    let waiting_nodes = workflow_nodes_with_status(nodes.map(Vec::as_slice), "waiting");
    let canceled_nodes = workflow_nodes_with_status(nodes.map(Vec::as_slice), "canceled");
    let mut transition_values = transitions.into_iter().flatten().collect::<Vec<_>>();
    transition_values.sort_by_key(|transition| {
        transition
            .get("eventSequence")
            .or_else(|| transition.get("event_sequence"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    });
    let transition_rows = transition_values
        .into_iter()
        .rev()
        .take(5)
        .map(|transition| {
            let from = transition
                .get("from")
                .and_then(Value::as_str)
                .unwrap_or("-");
            let to = transition.get("to").and_then(Value::as_str).unwrap_or("-");
            let reason = transition
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("transition");
            let sequence = transition
                .get("eventSequence")
                .or_else(|| transition.get("event_sequence"))
                .and_then(Value::as_u64)
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".into());
            let detail = workflow_compact_detail(transition.get("detail"), 140);
            format!(
                "- #{} {} -> {} reason={} detail={}",
                sequence,
                workflow_node_label(from),
                workflow_node_label(to),
                reason,
                if detail.is_empty() {
                    "-"
                } else {
                    detail.as_str()
                }
            )
        })
        .collect::<Vec<_>>();

    WorkflowGraphDiagnosis {
        summary: format!(
            "{} mode={} current={} status={} seq={}{}",
            schema,
            mode,
            workflow_node_label(current_node),
            current_status,
            sequence,
            if status_summary.is_empty() {
                String::new()
            } else {
                format!(" statuses=[{}]", status_summary)
            }
        ),
        node_count: nodes.map(|nodes| nodes.len()).unwrap_or(0),
        transition_count: transitions
            .map(|transitions| transitions.len())
            .unwrap_or(0),
        node_rows,
        transition_rows,
        failed_nodes,
        waiting_nodes,
        canceled_nodes,
    }
}

fn workflow_status_summary(nodes: Option<&[Value]>) -> String {
    WORKFLOW_STATUS_ORDER
        .iter()
        .copied()
        .filter_map(|status| {
            let count = nodes
                .map(|nodes| {
                    nodes
                        .iter()
                        .filter(|node| node.get("status").and_then(Value::as_str) == Some(status))
                        .count()
                })
                .unwrap_or(0);
            (count > 0).then(|| format!("{status} {count}"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn workflow_nodes_with_status(nodes: Option<&[Value]>, status: &str) -> Vec<String> {
    nodes
        .into_iter()
        .flatten()
        .filter(|node| node.get("status").and_then(Value::as_str) == Some(status))
        .filter_map(|node| node.get("node").and_then(Value::as_str))
        .map(workflow_node_label)
        .collect()
}

fn workflow_node_label(node: &str) -> String {
    node.replace('_', " ")
}

fn workflow_compact_detail(value: Option<&Value>, max_chars: usize) -> String {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return String::new();
    };
    if let Some(object) = value.as_object() {
        if let Some(summary) = workflow_detail_summary(object) {
            return truncate_for_prompt(&summary, max_chars);
        }
    }
    let text = value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
        .replace('\n', " ");
    truncate_for_prompt(&text, max_chars)
}

fn workflow_detail_summary(object: &serde_json::Map<String, Value>) -> Option<String> {
    let tools = workflow_detail_string_array(object.get("tools"));
    let origins = workflow_detail_string_array(
        object
            .get("toolOrigins")
            .or_else(|| object.get("tool_origins")),
    )
    .into_iter()
    .map(|origin| workflow_tool_origin_label(&origin))
    .collect::<Vec<_>>();
    let call_ids = workflow_detail_string_array(
        object
            .get("toolCallIds")
            .or_else(|| object.get("tool_call_ids")),
    );
    let tool_calls = workflow_detail_tool_call_summaries(
        object.get("toolCalls").or_else(|| object.get("tool_calls")),
    );
    let protocol = workflow_detail_scalar(object, &["toolProtocol", "tool_protocol"]);

    let mut parts = Vec::new();
    for (label, value) in [
        (
            "queueLifecycle",
            workflow_detail_scalar_alias(object, "queueLifecycle"),
        ),
        (
            "queueStatus",
            workflow_detail_scalar_alias(object, "queueStatus"),
        ),
        ("admission", workflow_detail_scalar(object, &["admission"])),
        (
            "requestSource",
            workflow_detail_scalar_alias(object, "requestSource"),
        ),
        (
            "toolContext",
            workflow_detail_scalar_alias(object, "toolContext"),
        ),
        (
            "queueItemId",
            workflow_detail_scalar_alias(object, "queueItemId"),
        ),
        (
            "approvalId",
            workflow_detail_scalar_alias(object, "approvalId"),
        ),
        ("status", workflow_detail_scalar(object, &["status"])),
        ("serverId", workflow_detail_scalar_alias(object, "serverId")),
        ("toolName", workflow_detail_scalar_alias(object, "toolName")),
        (
            "requestedName",
            workflow_detail_scalar_alias(object, "requestedName"),
        ),
        ("toolKind", workflow_detail_scalar_alias(object, "toolKind")),
        (
            "sourceLabel",
            workflow_detail_scalar_alias(object, "sourceLabel"),
        ),
        (
            "definitionName",
            workflow_detail_scalar_alias(object, "definitionName"),
        ),
        (
            "requiresApproval",
            workflow_detail_scalar_alias(object, "requiresApproval"),
        ),
        (
            "directBridge",
            workflow_detail_scalar_alias(object, "directBridge"),
        ),
        (
            "approvedToolCallReplay",
            workflow_detail_scalar_alias(object, "approvedToolCallReplay"),
        ),
        (
            "bridgeStatus",
            workflow_detail_scalar_alias(object, "bridgeStatus"),
        ),
        (
            "bridgeRejectionReason",
            workflow_detail_scalar_alias(object, "bridgeRejectionReason"),
        ),
        (
            "bridgeStage",
            workflow_detail_scalar_alias(object, "bridgeStage"),
        ),
        (
            "lastBridgeTarget",
            workflow_detail_nested_summary_alias(object, "lastBridgeTarget"),
        ),
        (
            "checkpointId",
            workflow_detail_scalar_alias(object, "checkpointId"),
        ),
        (
            "checkpointScope",
            workflow_detail_scalar_alias(object, "checkpointScope"),
        ),
        (
            "checkpointState",
            workflow_detail_scalar_alias(object, "checkpointState"),
        ),
        (
            "checkpointIteration",
            workflow_detail_scalar_alias(object, "checkpointIteration"),
        ),
        ("kind", workflow_detail_scalar(object, &["kind"])),
        ("state", workflow_detail_scalar(object, &["state"])),
        (
            "previousState",
            workflow_detail_scalar_alias(object, "previousState"),
        ),
        ("runState", workflow_detail_scalar_alias(object, "runState")),
        (
            "preserveCurrent",
            workflow_detail_scalar_alias(object, "preserveCurrent"),
        ),
        (
            "mutationKind",
            workflow_detail_scalar_alias(object, "mutationKind"),
        ),
        (
            "targetSummary",
            workflow_detail_scalar_alias(object, "targetSummary"),
        ),
        (
            "checkpointSummary",
            workflow_detail_scalar_alias(object, "checkpointSummary"),
        ),
        ("source", workflow_detail_scalar(object, &["source"])),
        (
            "conversationKind",
            workflow_detail_scalar_alias(object, "conversationKind"),
        ),
        ("roomId", workflow_detail_scalar_alias(object, "roomId")),
        (
            "channelId",
            workflow_detail_scalar_alias(object, "channelId"),
        ),
        ("chatId", workflow_detail_scalar_alias(object, "chatId")),
        ("threadId", workflow_detail_scalar_alias(object, "threadId")),
        ("groupId", workflow_detail_scalar_alias(object, "groupId")),
        ("phase", workflow_detail_scalar(object, &["phase"])),
        ("strategy", workflow_detail_scalar(object, &["strategy"])),
        ("batch", workflow_detail_scalar(object, &["batch"])),
        (
            "requestedChildren",
            workflow_detail_count_alias(object, "requestedChildren"),
        ),
        (
            "existingChildren",
            workflow_detail_count_alias(object, "existingChildren"),
        ),
        (
            "completedChildren",
            workflow_detail_count_alias(object, "completedChildren"),
        ),
        (
            "failedChildren",
            workflow_detail_count_alias(object, "failedChildren"),
        ),
        (
            "abortedChildren",
            workflow_detail_count_alias(object, "abortedChildren"),
        ),
        (
            "unknownChildren",
            workflow_detail_count_alias(object, "unknownChildren"),
        ),
        ("children", workflow_detail_count(object.get("children"))),
        ("results", workflow_detail_count(object.get("results"))),
        (
            "parentDepth",
            workflow_detail_scalar_alias(object, "parentDepth"),
        ),
        (
            "childDepth",
            workflow_detail_scalar_alias(object, "childDepth"),
        ),
        (
            "maxSubagents",
            workflow_detail_scalar_alias(object, "maxSubagents"),
        ),
        (
            "maxSubagentDepth",
            workflow_detail_scalar_alias(object, "maxSubagentDepth"),
        ),
        (
            "maxConcurrentChildren",
            workflow_detail_scalar_alias(object, "maxConcurrentChildren"),
        ),
        ("ok", workflow_detail_scalar(object, &["ok"])),
        (
            "orchestratorEnabled",
            workflow_detail_scalar_alias(object, "orchestratorEnabled"),
        ),
        (
            "subagentAutoApprove",
            workflow_detail_scalar_alias(object, "subagentAutoApprove"),
        ),
        (
            "inheritMcpToolsets",
            workflow_detail_scalar_alias(object, "inheritMcpToolsets"),
        ),
        ("action", workflow_detail_scalar(object, &["action"])),
        (
            "toolCount",
            workflow_detail_count_alias(object, "toolCount"),
        ),
        ("tools", (!tools.is_empty()).then(|| tools.join(", "))),
        ("origins", (!origins.is_empty()).then(|| origins.join(", "))),
        (
            "callIds",
            (!call_ids.is_empty()).then(|| call_ids.join(", ")),
        ),
        (
            "toolCalls",
            (!tool_calls.is_empty()).then(|| tool_calls.join(", ")),
        ),
        ("protocol", protocol),
        ("stage", workflow_detail_scalar(object, &["stage"])),
        (
            "resolution",
            workflow_detail_scalar(object, &["resolution"]),
        ),
        (
            "messageId",
            workflow_detail_scalar_alias(object, "messageId"),
        ),
        (
            "providerId",
            workflow_detail_scalar_alias(object, "providerId"),
        ),
        ("summary", workflow_detail_scalar(object, &["summary"])),
        (
            "errorKind",
            workflow_detail_scalar_alias(object, "errorKind"),
        ),
        ("reason", workflow_detail_scalar(object, &["reason"])),
        (
            "timeoutSeconds",
            workflow_detail_scalar_alias(object, "timeoutSeconds"),
        ),
        ("error", workflow_detail_scalar(object, &["error"])),
    ] {
        if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
            parts.push(format!("{label}={value}"));
        }
    }

    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn workflow_detail_snake_alias(camel_key: &str) -> Option<&'static str> {
    WORKFLOW_DETAIL_ALIAS_PAIRS
        .iter()
        .find_map(|(camel, snake)| (*camel == camel_key).then_some(*snake))
}

fn workflow_detail_scalar_alias(
    object: &serde_json::Map<String, Value>,
    camel_key: &str,
) -> Option<String> {
    if let Some(snake_key) = workflow_detail_snake_alias(camel_key) {
        workflow_detail_scalar(object, &[camel_key, snake_key])
    } else {
        workflow_detail_scalar(object, &[camel_key])
    }
}

fn workflow_detail_count_alias(
    object: &serde_json::Map<String, Value>,
    camel_key: &str,
) -> Option<String> {
    workflow_detail_count(object.get(camel_key)).or_else(|| {
        workflow_detail_snake_alias(camel_key)
            .and_then(|snake_key| workflow_detail_count(object.get(snake_key)))
    })
}

fn workflow_detail_nested_summary_alias(
    object: &serde_json::Map<String, Value>,
    camel_key: &str,
) -> Option<String> {
    if let Some(snake_key) = workflow_detail_snake_alias(camel_key) {
        workflow_detail_nested_summary(object, &[camel_key, snake_key])
    } else {
        workflow_detail_nested_summary(object, &[camel_key])
    }
}

fn workflow_detail_scalar(
    object: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<String> {
    keys.iter()
        .find_map(|key| object.get(*key))
        .and_then(workflow_detail_value_scalar)
}

fn workflow_detail_nested_summary(
    object: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<String> {
    keys.iter()
        .find_map(|key| object.get(*key))
        .and_then(Value::as_object)
        .and_then(workflow_detail_summary)
}

fn workflow_detail_value_scalar(value: &Value) -> Option<String> {
    if let Some(text) = value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(text.to_string());
    }
    if let Some(number) = value.as_i64() {
        return Some(number.to_string());
    }
    if let Some(number) = value.as_u64() {
        return Some(number.to_string());
    }
    if let Some(number) = value.as_f64().filter(|number| number.is_finite()) {
        return Some(number.to_string());
    }
    value.as_bool().map(|value| value.to_string())
}

fn workflow_detail_count(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::Array(items)) => Some(items.len().to_string()),
        Some(Value::Number(number)) => Some(number.to_string()),
        _ => None,
    }
}

fn workflow_detail_string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn workflow_detail_tool_call_summaries(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .filter_map(|call| {
            let name = call
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let origin = call
                .get("origin")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(workflow_tool_origin_label);
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let summary = [name.map(str::to_string), origin, id.map(str::to_string)]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(":");
            (!summary.is_empty()).then_some(summary)
        })
        .collect()
}

fn workflow_tool_origin_label(origin: &str) -> String {
    match origin {
        "provider_native" => "provider native".into(),
        "planner_json" => "planner JSON".into(),
        "hermes_markup" => "Hermes markup".into(),
        other => other.replace('_', " "),
    }
}

fn format_list_or_dash(items: Vec<String>) -> String {
    if items.is_empty() {
        "-".into()
    } else {
        items.join("\n")
    }
}

pub fn abort_agent_run(
    store: &AppStore,
    run_id: String,
    reason: Option<String>,
    app: Option<&AppHandle>,
) -> AppResult<AgentRunRecord> {
    let run = store.abort_agent_run(&run_id, reason.clone())?;
    spawn_session_finished_hooks(
        store,
        run.clone(),
        json!({
            "source": "abort_agent_run",
            "reason": reason,
        }),
    );
    emit_agent_run_record(app, &run, None);
    Ok(run)
}
