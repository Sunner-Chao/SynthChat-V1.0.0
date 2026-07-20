use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    future::Future,
    net::{Ipv4Addr, Ipv6Addr},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration as StdDuration,
};

use regex::Regex;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map as JsonMap, Value as JsonValue, json};
use serde_yaml_ng::{Mapping as YamlMapping, Value as YamlValue};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::{Instant, MissedTickBehavior},
};
use url::{Host, Url};
use uuid::Uuid;

#[path = "mcp_remote.rs"]
mod remote;

use crate::{
    profiles::{ConfigDocumentMutation, ProfileError, ProfileService, Versioned},
    providers::ProviderToolDefinition,
    tools::{ToolExecutionControl, ToolExecutionControlError},
};

const MAX_SERVERS: usize = 64;
const MAX_NAME_BYTES: usize = 64;
const MAX_COMMAND_BYTES: usize = 1_024;
const MAX_ARGS: usize = 64;
const MAX_ARG_BYTES: usize = 2_048;
const MAX_ARGS_BYTES: usize = 16 * 1_024;
const MAX_URL_BYTES: usize = 2_048;
const MAX_SECRET_NAMES: usize = 32;
const MIN_TIMEOUT_SECONDS: u64 = 1;
const MAX_TIMEOUT_SECONDS: u64 = 600;
const MAX_IDEMPOTENCY_RECORDS: usize = 4_096;
const IDEMPOTENCY_RETENTION_HOURS: i64 = 24;
const MCP_SERVERS_KEY: &str = "mcp_servers";
const MCP_METADATA_KEY: &str = "_synthchat_mcp";
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const MAX_RPC_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_MCP_TOOLS: usize = 128;
const MAX_MCP_TOOL_NAME_BYTES: usize = 128;
const MAX_MCP_DESCRIPTION_BYTES: usize = 8 * 1024;
const MAX_MCP_SCHEMA_BYTES: usize = 64 * 1024;
const MAX_MCP_SCHEMA_DEPTH: usize = 16;
const MAX_MCP_SCHEMA_NODES: usize = 512;
const MAX_MCP_LIST_PAGES: usize = 8;
const MAX_MCP_RESULT_BYTES: usize = 64 * 1024;
const MAX_PROVIDER_TOOL_NAME_BYTES: usize = 64;
const MCP_PROCESS_SHUTDOWN_TIMEOUT: StdDuration = StdDuration::from_secs(2);

#[derive(Clone)]
pub struct McpService {
    profiles: Arc<ProfileService>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum McpTransport {
    Stdio,
    StreamableHttp,
    Sse,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    pub id: String,
    pub name: String,
    pub transport: McpTransport,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub enabled: bool,
    pub timeout_seconds: u64,
    pub env_secret_names: Vec<String>,
    pub bearer_token_secret_name: Option<String>,
    pub missing_secret_names: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(
    tag = "transport",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum CreateMcpServer {
    Stdio {
        name: String,
        command: String,
        args: Vec<String>,
        enabled: bool,
        timeout_seconds: u64,
        env_secret_names: Vec<String>,
    },
    StreamableHttp {
        name: String,
        url: String,
        enabled: bool,
        timeout_seconds: u64,
        #[serde(default)]
        bearer_token_secret_name: Option<String>,
    },
    Sse {
        name: String,
        url: String,
        enabled: bool,
        timeout_seconds: u64,
        #[serde(default)]
        bearer_token_secret_name: Option<String>,
    },
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServerPatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub transport: Option<McpTransport>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub env_secret_names: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_nullable_patch_field")]
    pub bearer_token_secret_name: Option<Option<String>>,
}

#[derive(Debug, Error)]
pub enum McpError {
    #[error("the MCP request does not match the contract")]
    InvalidRequest,
    #[error("invalid MCP server id")]
    InvalidServerId,
    #[error("MCP server not found")]
    ServerNotFound,
    #[error("an MCP server with this name already exists")]
    NameConflict,
    #[error("the MCP configuration capacity is exhausted")]
    CapacityExceeded,
    #[error("idempotency key was reused with a different MCP request")]
    IdempotencyConflict,
    #[error("the original idempotent MCP resource no longer exists")]
    IdempotencyResourceGone,
    #[error("the stored MCP configuration is malformed")]
    StoredConfigInvalid,
    #[error(transparent)]
    Profile(#[from] ProfileError),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct McpToolBinding {
    provider_name: String,
    server: StoredServer,
    upstream_name: String,
    description: String,
    input_schema: JsonValue,
    output_schema: Option<JsonValue>,
}

impl McpToolBinding {
    pub(crate) fn provider_name(&self) -> &str {
        &self.provider_name
    }

    pub(crate) fn provider_definition(&self) -> ProviderToolDefinition {
        ProviderToolDefinition {
            name: self.provider_name.clone(),
            description: self.description.clone(),
            parameters: self.input_schema.clone(),
            // External schemas are validated locally. They are not rewritten into
            // the narrower provider-specific strict-schema dialect.
            strict: Some(false),
        }
    }

    pub(crate) fn validate_arguments(&self, raw: &str) -> Result<JsonValue, McpRuntimeError> {
        if raw.len() > MAX_MCP_SCHEMA_BYTES {
            return Err(McpRuntimeError::InvalidArguments);
        }
        let value: JsonValue =
            serde_json::from_str(raw).map_err(|_| McpRuntimeError::InvalidArguments)?;
        if !value.is_object() || !schema_accepts(&self.input_schema, &value, 0)? {
            return Err(McpRuntimeError::InvalidArguments);
        }
        Ok(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct McpCallOutput {
    pub(crate) raw_result_json: String,
    pub(crate) provider_content: String,
    pub(crate) input_summary: String,
    pub(crate) result_summary: String,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub(crate) enum McpRuntimeError {
    #[error("MCP configuration is unavailable")]
    Configuration,
    #[error("MCP transport is unavailable")]
    Transport,
    #[error("MCP peer returned an invalid protocol message")]
    InvalidProtocol,
    #[error("MCP tool arguments are invalid")]
    InvalidArguments,
    #[error("MCP tool result is invalid")]
    InvalidResult,
    #[error("MCP operation timed out")]
    Timeout,
    #[error("MCP operation was cancelled")]
    Cancelled,
}

#[derive(Clone, Debug, PartialEq)]
struct DiscoveredTool {
    name: String,
    description: String,
    input_schema: JsonValue,
    output_schema: Option<JsonValue>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StoredServer {
    id: String,
    name: String,
    transport: McpTransport,
    command: Option<String>,
    args: Vec<String>,
    url: Option<String>,
    enabled: bool,
    timeout_seconds: u64,
    env_secret_names: Vec<String>,
    bearer_token_secret_name: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IdempotencyState {
    Present,
    Deleted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IdempotencyRecord {
    fingerprint: String,
    server_id: String,
    state: IdempotencyState,
    created_at: String,
}

#[derive(Clone, Debug)]
enum CreateDisposition {
    New,
    Replay(StoredServer),
}

impl McpService {
    pub fn new(profiles: Arc<ProfileService>) -> Self {
        Self { profiles }
    }

    pub fn list_servers(&self, profile_id: &str) -> Result<Versioned<Vec<McpServer>>, McpError> {
        let result = self
            .profiles
            .transact_config_document(profile_id, None, |document| {
                Ok::<_, McpError>(ConfigDocumentMutation::unchanged(parse_servers(
                    profile_id, document,
                )?))
            })?;
        Ok(Versioned {
            value: self.project_servers(profile_id, result.value)?,
            etag: result.etag,
        })
    }

    pub fn create_server(
        &self,
        profile_id: &str,
        request: &CreateMcpServer,
        idempotency_key: &str,
    ) -> Result<Versioned<McpServer>, McpError> {
        validate_idempotency_key(idempotency_key)?;
        let candidate = request.to_stored(new_server_id())?;
        let fingerprint = fingerprint(request)?;
        let key_hash = idempotency_key_hash(profile_id, idempotency_key);
        let now = OffsetDateTime::now_utc();
        let created_at = now
            .format(&Rfc3339)
            .map_err(|_| McpError::StoredConfigInvalid)?;
        let mut last_conflict = None;
        for _ in 0..4 {
            let preflight =
                self.profiles
                    .transact_config_document(profile_id, None, |document| {
                        Ok::<_, McpError>(ConfigDocumentMutation::unchanged(create_disposition(
                            profile_id,
                            document,
                            &candidate,
                            &key_hash,
                            &fingerprint,
                            now,
                        )?))
                    })?;
            let readiness_target = match &preflight.value {
                CreateDisposition::New => &candidate,
                CreateDisposition::Replay(server) => server,
            };
            let missing = self.missing_for_server(profile_id, readiness_target)?;
            let result = self.profiles.transact_config_document(
                profile_id,
                Some(&preflight.etag),
                |document| {
                    apply_create(
                        profile_id,
                        document,
                        &candidate,
                        &key_hash,
                        &fingerprint,
                        &created_at,
                        now,
                    )
                },
            );
            match result {
                Ok(result) => {
                    return Ok(Versioned {
                        value: result.value.project(&missing),
                        etag: result.etag,
                    });
                }
                Err(error @ McpError::Profile(ProfileError::RevisionConflict { .. })) => {
                    last_conflict = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_conflict.expect("a bounded create retry always records its revision conflict"))
    }

    pub fn update_server(
        &self,
        profile_id: &str,
        server_id: &str,
        expected_etag: &str,
        patch: &McpServerPatch,
    ) -> Result<Versioned<McpServer>, McpError> {
        validate_server_id(server_id)?;
        let preflight = self.profiles.transact_config_document(
            profile_id,
            Some(expected_etag),
            |document| {
                Ok::<_, McpError>(ConfigDocumentMutation::unchanged(parse_servers(
                    profile_id, document,
                )?))
            },
        )?;
        let current = preflight
            .value
            .iter()
            .find(|server| server.id == server_id)
            .cloned()
            .ok_or(McpError::ServerNotFound)?;
        let candidate = apply_patch(current.clone(), patch)?;
        if candidate.name != current.name
            && preflight
                .value
                .iter()
                .any(|server| server.name == candidate.name)
        {
            return Err(McpError::NameConflict);
        }
        let missing = self.missing_for_server(profile_id, &candidate)?;
        let result = self.profiles.transact_config_document(
            profile_id,
            Some(expected_etag),
            |document| {
                let servers = parse_servers(profile_id, document)?;
                let current = servers
                    .iter()
                    .find(|server| server.id == server_id)
                    .cloned()
                    .ok_or(McpError::ServerNotFound)?;
                let next = apply_patch(current.clone(), patch)?;
                if next.name != current.name
                    && servers.iter().any(|server| server.name == next.name)
                {
                    return Err(McpError::NameConflict);
                }
                if next == current {
                    return Ok(ConfigDocumentMutation::unchanged(current));
                }

                let mut servers_mapping = mcp_servers_mapping(document)?.clone();
                let existing = servers_mapping
                    .remove(yaml_key(&current.name))
                    .and_then(|value| value.as_mapping().cloned())
                    .ok_or(McpError::StoredConfigInvalid)?;
                servers_mapping.insert(
                    yaml_key(&next.name),
                    YamlValue::Mapping(write_server_mapping(&next, Some(existing))?),
                );
                set_document_value(
                    document,
                    MCP_SERVERS_KEY,
                    YamlValue::Mapping(servers_mapping),
                )?;
                Ok(ConfigDocumentMutation::changed(next))
            },
        )?;
        Ok(Versioned {
            value: result.value.project(&missing),
            etag: result.etag,
        })
    }

    pub fn delete_server(
        &self,
        profile_id: &str,
        server_id: &str,
        expected_etag: &str,
    ) -> Result<Versioned<()>, McpError> {
        validate_server_id(server_id)?;
        self.profiles
            .transact_config_document(profile_id, Some(expected_etag), |document| {
                let servers = parse_servers(profile_id, document)?;
                let target = servers.iter().find(|server| server.id == server_id);
                let mut changed = false;
                if let Some(target) = target {
                    let mut servers_mapping = mcp_servers_mapping(document)?.clone();
                    if servers_mapping.remove(yaml_key(&target.name)).is_none() {
                        return Err(McpError::StoredConfigInvalid);
                    }
                    set_document_value(
                        document,
                        MCP_SERVERS_KEY,
                        YamlValue::Mapping(servers_mapping),
                    )?;
                    changed = true;
                }

                let mut records = parse_idempotency_records(document)?;
                for record in records.values_mut() {
                    if record.server_id == server_id && record.state != IdempotencyState::Deleted {
                        record.state = IdempotencyState::Deleted;
                        changed = true;
                    }
                }
                if changed {
                    write_idempotency_records(document, &records)?;
                    Ok(ConfigDocumentMutation::changed(()))
                } else {
                    Ok(ConfigDocumentMutation::unchanged(()))
                }
            })
    }

    pub(crate) fn runtime_available(&self) -> bool {
        true
    }

    pub(crate) fn streamable_http_available(&self) -> bool {
        true
    }

    pub(crate) fn sse_available(&self) -> bool {
        true
    }

    pub(crate) async fn discover_tools(
        &self,
        profile_id: &str,
    ) -> Result<Vec<McpToolBinding>, McpRuntimeError> {
        let servers = self.runtime_servers(profile_id)?;
        let mut bindings = Vec::new();
        let mut provider_names = BTreeSet::new();
        for server in servers {
            if !server.enabled
                || !self
                    .missing_for_server(profile_id, &server)
                    .map_err(|_| McpRuntimeError::Configuration)?
                    .is_empty()
            {
                continue;
            }
            let tools = match server.transport {
                McpTransport::Stdio => self.discover_stdio(profile_id, &server).await,
                McpTransport::StreamableHttp | McpTransport::Sse => {
                    match self.bearer_token(profile_id, &server) {
                        Ok(bearer) => remote::discover(&server, bearer).await,
                        Err(error) => Err(error),
                    }
                }
            };
            let tools = match tools {
                Ok(tools) => tools,
                Err(error) => {
                    tracing::warn!(
                        server_id = %server.id,
                        transport = ?server.transport,
                        ?error,
                        "MCP server discovery failed"
                    );
                    continue;
                }
            };
            for tool in tools {
                let provider_name = projected_tool_name(&server.name, &tool.name);
                if !provider_names.insert(provider_name.clone()) {
                    return Err(McpRuntimeError::InvalidProtocol);
                }
                bindings.push(McpToolBinding {
                    provider_name,
                    server: server.clone(),
                    upstream_name: tool.name,
                    description: tool.description,
                    input_schema: tool.input_schema,
                    output_schema: tool.output_schema,
                });
            }
        }
        bindings.sort_by(|left, right| left.provider_name.cmp(&right.provider_name));
        Ok(bindings)
    }

    pub(crate) async fn call_tool(
        &self,
        profile_id: &str,
        binding: &McpToolBinding,
        raw_arguments: &str,
        control: &ToolExecutionControl,
    ) -> Result<McpCallOutput, McpRuntimeError> {
        let arguments = binding.validate_arguments(raw_arguments)?;
        let mut result = match binding.server.transport {
            McpTransport::Stdio => {
                self.call_stdio(profile_id, binding, arguments, control)
                    .await?
            }
            McpTransport::StreamableHttp | McpTransport::Sse => {
                let bearer = self.bearer_token(profile_id, &binding.server)?;
                remote::call(
                    &binding.server,
                    bearer,
                    &binding.upstream_name,
                    arguments,
                    control,
                )
                .await?
            }
        };
        if let Some(schema) = binding.output_schema.as_ref()
            && let Some(structured) = result.get("structuredContent")
            && !schema_accepts(schema, structured, 0)?
        {
            return Err(McpRuntimeError::InvalidResult);
        }
        let reported_error = result
            .get("isError")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        let secrets = self
            .profiles
            .secret_redaction_snapshots(profile_id)
            .map_err(|_| McpRuntimeError::Configuration)?;
        redact_json(&mut result, &secrets);
        let raw_result_json =
            serde_json::to_string(&result).map_err(|_| McpRuntimeError::InvalidResult)?;
        if raw_result_json.len() > MAX_MCP_RESULT_BYTES {
            return Err(McpRuntimeError::InvalidResult);
        }
        Ok(McpCallOutput {
            provider_content: raw_result_json.clone(),
            raw_result_json,
            input_summary: format!("MCP tool {}", binding.provider_name),
            result_summary: if reported_error {
                "MCP tool reported an error".to_owned()
            } else {
                "MCP tool completed".to_owned()
            },
        })
    }

    fn runtime_servers(&self, profile_id: &str) -> Result<Vec<StoredServer>, McpRuntimeError> {
        self.profiles
            .transact_config_document(profile_id, None, |document| {
                Ok::<_, McpError>(ConfigDocumentMutation::unchanged(parse_servers(
                    profile_id, document,
                )?))
            })
            .map(|result| result.value)
            .map_err(|_| McpRuntimeError::Configuration)
    }

    async fn discover_stdio(
        &self,
        profile_id: &str,
        server: &StoredServer,
    ) -> Result<Vec<DiscoveredTool>, McpRuntimeError> {
        let environment = self.secret_environment(profile_id, server)?;
        let mut client = StdioClient::spawn(server, &environment).await?;
        let timeout = StdDuration::from_secs(server.timeout_seconds);
        let result = async {
            client.initialize(timeout, None).await?;
            client.list_tools(timeout, None).await
        }
        .await;
        client.close().await;
        result
    }

    async fn call_stdio(
        &self,
        profile_id: &str,
        binding: &McpToolBinding,
        arguments: JsonValue,
        control: &ToolExecutionControl,
    ) -> Result<JsonValue, McpRuntimeError> {
        let environment = self.secret_environment(profile_id, &binding.server)?;
        let mut client = StdioClient::spawn(&binding.server, &environment).await?;
        let timeout = StdDuration::from_secs(binding.server.timeout_seconds);
        let result = async {
            client.initialize(timeout, Some(control)).await?;
            client
                .call_tool(&binding.upstream_name, arguments, timeout, control)
                .await
        }
        .await;
        client.close().await;
        result
    }

    fn secret_environment(
        &self,
        profile_id: &str,
        server: &StoredServer,
    ) -> Result<Vec<(String, SecretString)>, McpRuntimeError> {
        server
            .env_secret_names
            .iter()
            .map(|name| {
                self.profiles
                    .first_secret_snapshot(profile_id, std::slice::from_ref(name), true)
                    .map_err(|_| McpRuntimeError::Configuration)?
                    .map(|(_, value)| (name.clone(), value))
                    .ok_or(McpRuntimeError::Configuration)
            })
            .collect()
    }

    fn bearer_token(
        &self,
        profile_id: &str,
        server: &StoredServer,
    ) -> Result<Option<SecretString>, McpRuntimeError> {
        let Some(name) = server.bearer_token_secret_name.as_ref() else {
            return Ok(None);
        };
        self.profiles
            .first_secret_snapshot(profile_id, std::slice::from_ref(name), true)
            .map_err(|_| McpRuntimeError::Configuration)?
            .map(|(_, value)| value)
            .ok_or(McpRuntimeError::Configuration)
            .map(Some)
    }

    fn project_servers(
        &self,
        profile_id: &str,
        servers: Vec<StoredServer>,
    ) -> Result<Vec<McpServer>, McpError> {
        let names = servers
            .iter()
            .flat_map(StoredServer::secret_names)
            .collect::<BTreeSet<_>>();
        let missing = self.profiles.missing_secret_names(profile_id, &names)?;
        Ok(servers
            .into_iter()
            .map(|server| server.project(&missing))
            .collect())
    }

    fn missing_for_server(
        &self,
        profile_id: &str,
        server: &StoredServer,
    ) -> Result<BTreeSet<String>, McpError> {
        let names = server.secret_names().collect::<BTreeSet<_>>();
        self.profiles
            .missing_secret_names(profile_id, &names)
            .map_err(McpError::from)
    }
}

impl CreateMcpServer {
    fn to_stored(&self, id: String) -> Result<StoredServer, McpError> {
        let mut server = match self {
            Self::Stdio {
                name,
                command,
                args,
                enabled,
                timeout_seconds,
                env_secret_names,
            } => StoredServer {
                id,
                name: name.clone(),
                transport: McpTransport::Stdio,
                command: Some(command.clone()),
                args: args.clone(),
                url: None,
                enabled: *enabled,
                timeout_seconds: *timeout_seconds,
                env_secret_names: env_secret_names.clone(),
                bearer_token_secret_name: None,
            },
            Self::StreamableHttp {
                name,
                url,
                enabled,
                timeout_seconds,
                bearer_token_secret_name,
            } => StoredServer {
                id,
                name: name.clone(),
                transport: McpTransport::StreamableHttp,
                command: None,
                args: Vec::new(),
                url: Some(url.clone()),
                enabled: *enabled,
                timeout_seconds: *timeout_seconds,
                env_secret_names: Vec::new(),
                bearer_token_secret_name: bearer_token_secret_name.clone(),
            },
            Self::Sse {
                name,
                url,
                enabled,
                timeout_seconds,
                bearer_token_secret_name,
            } => StoredServer {
                id,
                name: name.clone(),
                transport: McpTransport::Sse,
                command: None,
                args: Vec::new(),
                url: Some(url.clone()),
                enabled: *enabled,
                timeout_seconds: *timeout_seconds,
                env_secret_names: Vec::new(),
                bearer_token_secret_name: bearer_token_secret_name.clone(),
            },
        };
        server.env_secret_names.sort();
        validate_server(&server)?;
        Ok(server)
    }
}

impl StoredServer {
    fn secret_names(&self) -> impl Iterator<Item = String> + '_ {
        self.env_secret_names
            .iter()
            .cloned()
            .chain(self.bearer_token_secret_name.iter().cloned())
    }

    fn project(self, missing: &BTreeSet<String>) -> McpServer {
        let mut missing_secret_names = self
            .secret_names()
            .filter(|name| missing.contains(name))
            .collect::<Vec<_>>();
        missing_secret_names.sort();
        McpServer {
            id: self.id,
            name: self.name,
            transport: self.transport,
            command: self.command,
            args: self.args,
            url: self.url,
            enabled: self.enabled,
            timeout_seconds: self.timeout_seconds,
            env_secret_names: self.env_secret_names,
            bearer_token_secret_name: self.bearer_token_secret_name,
            missing_secret_names,
        }
    }
}

fn deserialize_nullable_patch_field<'de, D, T>(
    deserializer: D,
) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

fn apply_patch(mut server: StoredServer, patch: &McpServerPatch) -> Result<StoredServer, McpError> {
    if let Some(transport) = patch.transport
        && transport != server.transport
    {
        return Err(McpError::InvalidRequest);
    }
    if let Some(name) = &patch.name {
        server.name = name.clone();
    }
    if let Some(enabled) = patch.enabled {
        server.enabled = enabled;
    }
    if let Some(timeout_seconds) = patch.timeout_seconds {
        server.timeout_seconds = timeout_seconds;
    }
    match server.transport {
        McpTransport::Stdio => {
            if patch.url.is_some() || patch.bearer_token_secret_name.is_some() {
                return Err(McpError::InvalidRequest);
            }
            if let Some(command) = &patch.command {
                server.command = Some(command.clone());
            }
            if let Some(args) = &patch.args {
                server.args = args.clone();
            }
            if let Some(names) = &patch.env_secret_names {
                server.env_secret_names = names.clone();
            }
        }
        McpTransport::StreamableHttp | McpTransport::Sse => {
            if patch.command.is_some() || patch.args.is_some() || patch.env_secret_names.is_some() {
                return Err(McpError::InvalidRequest);
            }
            if let Some(url) = &patch.url {
                server.url = Some(url.clone());
            }
            if let Some(name) = &patch.bearer_token_secret_name {
                server.bearer_token_secret_name = name.clone();
            }
        }
    }
    server.env_secret_names.sort();
    validate_server(&server)?;
    Ok(server)
}

fn validate_server(server: &StoredServer) -> Result<(), McpError> {
    validate_name(&server.name)?;
    validate_server_id(&server.id)?;
    if !(MIN_TIMEOUT_SECONDS..=MAX_TIMEOUT_SECONDS).contains(&server.timeout_seconds) {
        return Err(McpError::InvalidRequest);
    }
    match server.transport {
        McpTransport::Stdio => {
            let command = server.command.as_deref().ok_or(McpError::InvalidRequest)?;
            validate_command(command)?;
            if server.url.is_some() || server.bearer_token_secret_name.is_some() {
                return Err(McpError::InvalidRequest);
            }
            validate_args(&server.args)?;
            validate_secret_names(&server.env_secret_names)?;
        }
        McpTransport::StreamableHttp | McpTransport::Sse => {
            if server.command.is_some()
                || !server.args.is_empty()
                || !server.env_secret_names.is_empty()
            {
                return Err(McpError::InvalidRequest);
            }
            validate_remote_url(server.url.as_deref().ok_or(McpError::InvalidRequest)?)?;
            if let Some(name) = server.bearer_token_secret_name.as_deref() {
                validate_secret_name(name)?;
            }
        }
    }
    Ok(())
}

fn validate_name(value: &str) -> Result<(), McpError> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > MAX_NAME_BYTES
        || !bytes[0].is_ascii_alphanumeric()
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_' || *byte == b'-')
    {
        Err(McpError::InvalidRequest)
    } else {
        Ok(())
    }
}

fn validate_server_id(value: &str) -> Result<(), McpError> {
    let Some(suffix) = value.strip_prefix("mcp_") else {
        return Err(McpError::InvalidServerId);
    };
    if suffix.len() != 32
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Err(McpError::InvalidServerId)
    } else {
        Ok(())
    }
}

fn validate_idempotency_key(value: &str) -> Result<(), McpError> {
    if !(8..=128).contains(&value.len()) || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        Err(McpError::InvalidRequest)
    } else {
        Ok(())
    }
}

fn validate_command(value: &str) -> Result<(), McpError> {
    if value.trim() != value
        || value.is_empty()
        || value.len() > MAX_COMMAND_BYTES
        || value.starts_with('-')
        || value.chars().any(|ch| ch.is_control())
        || value.chars().any(|ch| "&|;<>`$\"'".contains(ch))
        || (value.chars().any(char::is_whitespace) && !value.contains(['/', '\\']))
    {
        return Err(McpError::InvalidRequest);
    }
    let executable = value
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(value)
        .to_ascii_lowercase();
    let executable = executable.strip_suffix(".exe").unwrap_or(&executable);
    if matches!(
        executable,
        "sh" | "bash" | "zsh" | "fish" | "cmd" | "powershell" | "pwsh" | "wscript" | "cscript"
    ) {
        return Err(McpError::InvalidRequest);
    }
    Ok(())
}

fn validate_args(args: &[String]) -> Result<(), McpError> {
    if args.len() > MAX_ARGS {
        return Err(McpError::InvalidRequest);
    }
    let mut total = 0usize;
    for arg in args {
        if arg.len() > MAX_ARG_BYTES
            || arg.chars().any(|ch| ch == '\0' || ch == '\r' || ch == '\n')
            || sensitive_argument(arg)
        {
            return Err(McpError::InvalidRequest);
        }
        total = total
            .checked_add(arg.len())
            .ok_or(McpError::InvalidRequest)?;
    }
    if total > MAX_ARGS_BYTES {
        Err(McpError::InvalidRequest)
    } else {
        Ok(())
    }
}

fn sensitive_argument(value: &str) -> bool {
    let trimmed = value.trim();
    let option = trimmed.trim_start_matches('-');
    let (name, has_assignment) = option
        .split_once('=')
        .map_or((option, false), |(name, _)| (name, true));
    let normalized = name
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    let sensitive = [
        "token",
        "apikey",
        "secret",
        "password",
        "passwd",
        "credential",
        "authorization",
        "auth",
        "privatekey",
        "accesskey",
    ]
    .iter()
    .any(|marker| normalized.contains(marker));
    sensitive && (trimmed.starts_with('-') || has_assignment)
}

fn validate_secret_names(names: &[String]) -> Result<(), McpError> {
    if names.len() > MAX_SECRET_NAMES {
        return Err(McpError::InvalidRequest);
    }
    let unique = names.iter().collect::<BTreeSet<_>>();
    if unique.len() != names.len() {
        return Err(McpError::InvalidRequest);
    }
    for name in names {
        validate_secret_name(name)?;
    }
    Ok(())
}

fn validate_secret_name(value: &str) -> Result<(), McpError> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 128
        || !bytes[0].is_ascii_uppercase()
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'_')
    {
        Err(McpError::InvalidRequest)
    } else {
        Ok(())
    }
}

fn validate_remote_url(value: &str) -> Result<(), McpError> {
    if value.len() > MAX_URL_BYTES {
        return Err(McpError::InvalidRequest);
    }
    let url = Url::parse(value).map_err(|_| McpError::InvalidRequest)?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(McpError::InvalidRequest);
    }
    let host = url.host().ok_or(McpError::InvalidRequest)?;
    let loopback = match host {
        Host::Domain(domain) => {
            domain.eq_ignore_ascii_case("localhost")
                || domain.to_ascii_lowercase().ends_with(".localhost")
        }
        Host::Ipv4(address) => address.is_loopback(),
        Host::Ipv6(address) => address.is_loopback(),
    };
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(McpError::InvalidRequest);
    }
    match host {
        Host::Ipv4(address) if !loopback && restricted_ipv4(address) => {
            Err(McpError::InvalidRequest)
        }
        Host::Ipv6(address) if !loopback && restricted_ipv6(address) => {
            Err(McpError::InvalidRequest)
        }
        _ => Ok(()),
    }
}

fn restricted_ipv4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    address.is_unspecified()
        || address.is_private()
        || address.is_link_local()
        || address.is_multicast()
        || address.is_broadcast()
        || a == 0
        || a >= 240
        || (a == 100 && (64..=127).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
}

fn restricted_ipv6(address: Ipv6Addr) -> bool {
    address.is_unspecified()
        || address.is_multicast()
        || address.is_unique_local()
        || address.is_unicast_link_local()
        || address.to_ipv4_mapped().is_some_and(restricted_ipv4)
}

fn create_disposition(
    profile_id: &str,
    document: &YamlValue,
    candidate: &StoredServer,
    key_hash: &str,
    fingerprint: &str,
    now: OffsetDateTime,
) -> Result<CreateDisposition, McpError> {
    let servers = parse_servers(profile_id, document)?;
    let mut records = parse_idempotency_records(document)?;
    let _ = cleanup_idempotency_records(&mut records, now)?;
    if let Some(record) = records.get(key_hash) {
        if record.fingerprint != fingerprint {
            return Err(McpError::IdempotencyConflict);
        }
        if record.state == IdempotencyState::Deleted {
            return Err(McpError::IdempotencyResourceGone);
        }
        return servers
            .into_iter()
            .find(|server| server.id == record.server_id)
            .map(CreateDisposition::Replay)
            .ok_or(McpError::IdempotencyResourceGone);
    }
    if servers.len() >= MAX_SERVERS || records.len() >= MAX_IDEMPOTENCY_RECORDS {
        return Err(McpError::CapacityExceeded);
    }
    if servers.iter().any(|server| server.name == candidate.name) {
        return Err(McpError::NameConflict);
    }
    Ok(CreateDisposition::New)
}

fn apply_create(
    profile_id: &str,
    document: &mut YamlValue,
    candidate: &StoredServer,
    key_hash: &str,
    fingerprint: &str,
    created_at: &str,
    now: OffsetDateTime,
) -> Result<ConfigDocumentMutation<StoredServer>, McpError> {
    let servers = parse_servers(profile_id, document)?;
    let mut records = parse_idempotency_records(document)?;
    let records_changed = cleanup_idempotency_records(&mut records, now)?;
    if let Some(record) = records.get(key_hash) {
        if record.fingerprint != fingerprint {
            return Err(McpError::IdempotencyConflict);
        }
        if record.state == IdempotencyState::Deleted {
            return Err(McpError::IdempotencyResourceGone);
        }
        let server = servers
            .into_iter()
            .find(|server| server.id == record.server_id)
            .ok_or(McpError::IdempotencyResourceGone)?;
        if records_changed {
            write_idempotency_records(document, &records)?;
            return Ok(ConfigDocumentMutation::changed(server));
        }
        return Ok(ConfigDocumentMutation::unchanged(server));
    }

    if servers.len() >= MAX_SERVERS || records.len() >= MAX_IDEMPOTENCY_RECORDS {
        return Err(McpError::CapacityExceeded);
    }
    if servers.iter().any(|server| server.name == candidate.name) {
        return Err(McpError::NameConflict);
    }
    let mut servers_mapping = mcp_servers_mapping(document)?.clone();
    servers_mapping.insert(
        yaml_key(&candidate.name),
        YamlValue::Mapping(write_server_mapping(candidate, None)?),
    );
    set_document_value(
        document,
        MCP_SERVERS_KEY,
        YamlValue::Mapping(servers_mapping),
    )?;
    records.insert(
        key_hash.to_owned(),
        IdempotencyRecord {
            fingerprint: fingerprint.to_owned(),
            server_id: candidate.id.clone(),
            state: IdempotencyState::Present,
            created_at: created_at.to_owned(),
        },
    );
    write_idempotency_records(document, &records)?;
    Ok(ConfigDocumentMutation::changed(candidate.clone()))
}

fn parse_servers(profile_id: &str, document: &YamlValue) -> Result<Vec<StoredServer>, McpError> {
    let mapping = mcp_servers_mapping(document)?;
    if mapping.len() > MAX_SERVERS {
        return Err(McpError::StoredConfigInvalid);
    }
    let mut servers = Vec::with_capacity(mapping.len());
    let mut ids = BTreeSet::new();
    for (name, value) in mapping {
        let name = name.as_str().ok_or(McpError::StoredConfigInvalid)?;
        validate_name(name).map_err(|_| McpError::StoredConfigInvalid)?;
        let entry = value.as_mapping().ok_or(McpError::StoredConfigInvalid)?;
        let server = parse_server(profile_id, name, entry)?;
        if !ids.insert(server.id.clone()) {
            return Err(McpError::StoredConfigInvalid);
        }
        servers.push(server);
    }
    servers.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(servers)
}

fn parse_server(
    profile_id: &str,
    name: &str,
    entry: &YamlMapping,
) -> Result<StoredServer, McpError> {
    if entry.contains_key(yaml_key("auth")) {
        return Err(McpError::StoredConfigInvalid);
    }
    let command = optional_yaml_string(entry, "command")?;
    let url = optional_yaml_string(entry, "url")?;
    let transport_value = optional_yaml_string(entry, "transport")?;
    let type_value = optional_yaml_string(entry, "type")?;
    let transport = stored_transport(
        command.is_some(),
        url.is_some(),
        transport_value.as_deref(),
        type_value.as_deref(),
    )?;
    let id = parse_server_id(profile_id, name, entry)?;
    let args = yaml_string_sequence(entry, "args")?;
    let enabled = optional_yaml_bool(entry, "enabled")?.unwrap_or(true);
    let timeout_seconds = optional_yaml_u64(entry, "connect_timeout")?.unwrap_or(30);
    let env_secret_names = parse_env_secret_names(entry)?;
    let bearer_token_secret_name = parse_bearer_secret_name(entry)?;
    let server = StoredServer {
        id,
        name: name.to_owned(),
        transport,
        command,
        args,
        url,
        enabled,
        timeout_seconds,
        env_secret_names,
        bearer_token_secret_name,
    };
    validate_server(&server).map_err(|_| McpError::StoredConfigInvalid)?;
    Ok(server)
}

fn stored_transport(
    has_command: bool,
    has_url: bool,
    transport: Option<&str>,
    kind: Option<&str>,
) -> Result<McpTransport, McpError> {
    if has_command && !has_url {
        if matches!(transport, None | Some("stdio")) && matches!(kind, None | Some("stdio")) {
            return Ok(McpTransport::Stdio);
        }
        return Err(McpError::StoredConfigInvalid);
    }
    if !has_command && has_url {
        if transport == Some("sse") || kind == Some("sse") {
            if matches!(transport, None | Some("sse") | Some("http"))
                && matches!(kind, None | Some("sse") | Some("http"))
            {
                return Ok(McpTransport::Sse);
            }
            return Err(McpError::StoredConfigInvalid);
        }
        if matches!(
            transport,
            None | Some("http") | Some("streamable_http") | Some("streamableHttp")
        ) && matches!(
            kind,
            None | Some("http") | Some("streamable_http") | Some("streamableHttp")
        ) {
            return Ok(McpTransport::StreamableHttp);
        }
    }
    Err(McpError::StoredConfigInvalid)
}

fn parse_server_id(profile_id: &str, name: &str, entry: &YamlMapping) -> Result<String, McpError> {
    let Some(internal) = entry.get(yaml_key("_synthchat")) else {
        return Ok(legacy_server_id(profile_id, name));
    };
    let internal = internal.as_mapping().ok_or(McpError::StoredConfigInvalid)?;
    let id = internal
        .get(yaml_key("id"))
        .and_then(YamlValue::as_str)
        .ok_or(McpError::StoredConfigInvalid)?
        .to_owned();
    validate_server_id(&id).map_err(|_| McpError::StoredConfigInvalid)?;
    Ok(id)
}

fn parse_env_secret_names(entry: &YamlMapping) -> Result<Vec<String>, McpError> {
    let Some(value) = entry.get(yaml_key("env")) else {
        return Ok(Vec::new());
    };
    let mapping = value.as_mapping().ok_or(McpError::StoredConfigInvalid)?;
    let mut names = Vec::with_capacity(mapping.len());
    for (key, value) in mapping {
        let key = key.as_str().ok_or(McpError::StoredConfigInvalid)?;
        validate_secret_name(key).map_err(|_| McpError::StoredConfigInvalid)?;
        let expected = format!("${{{key}}}");
        if value.as_str() != Some(expected.as_str()) {
            return Err(McpError::StoredConfigInvalid);
        }
        names.push(key.to_owned());
    }
    names.sort();
    validate_secret_names(&names).map_err(|_| McpError::StoredConfigInvalid)?;
    Ok(names)
}

fn parse_bearer_secret_name(entry: &YamlMapping) -> Result<Option<String>, McpError> {
    let Some(value) = entry.get(yaml_key("headers")) else {
        return Ok(None);
    };
    let mapping = value.as_mapping().ok_or(McpError::StoredConfigInvalid)?;
    if mapping.is_empty() {
        return Ok(None);
    }
    if mapping.len() != 1 {
        return Err(McpError::StoredConfigInvalid);
    }
    let value = mapping
        .get(yaml_key("Authorization"))
        .and_then(YamlValue::as_str)
        .ok_or(McpError::StoredConfigInvalid)?;
    let name = value
        .strip_prefix("Bearer ${")
        .and_then(|value| value.strip_suffix('}'))
        .ok_or(McpError::StoredConfigInvalid)?;
    validate_secret_name(name).map_err(|_| McpError::StoredConfigInvalid)?;
    Ok(Some(name.to_owned()))
}

fn write_server_mapping(
    server: &StoredServer,
    existing: Option<YamlMapping>,
) -> Result<YamlMapping, McpError> {
    let mut mapping = existing.unwrap_or_default();
    for key in [
        "command",
        "args",
        "url",
        "transport",
        "enabled",
        "connect_timeout",
        "env",
        "headers",
        "auth",
    ] {
        mapping.remove(yaml_key(key));
    }
    mapping.insert(yaml_key("enabled"), YamlValue::Bool(server.enabled));
    mapping.insert(
        yaml_key("connect_timeout"),
        serde_yaml_ng::to_value(server.timeout_seconds)
            .map_err(|_| McpError::StoredConfigInvalid)?,
    );
    match server.transport {
        McpTransport::Stdio => {
            mapping.insert(
                yaml_key("command"),
                YamlValue::String(server.command.clone().ok_or(McpError::InvalidRequest)?),
            );
            mapping.insert(
                yaml_key("args"),
                YamlValue::Sequence(server.args.iter().cloned().map(YamlValue::String).collect()),
            );
            if !server.env_secret_names.is_empty() {
                let env = server
                    .env_secret_names
                    .iter()
                    .map(|name| (yaml_key(name), YamlValue::String(format!("${{{name}}}"))))
                    .collect::<YamlMapping>();
                mapping.insert(yaml_key("env"), YamlValue::Mapping(env));
            }
        }
        McpTransport::StreamableHttp | McpTransport::Sse => {
            mapping.insert(
                yaml_key("url"),
                YamlValue::String(server.url.clone().ok_or(McpError::InvalidRequest)?),
            );
            if server.transport == McpTransport::Sse {
                mapping.insert(yaml_key("transport"), YamlValue::String("sse".to_owned()));
            }
            if let Some(name) = server.bearer_token_secret_name.as_deref() {
                let mut headers = YamlMapping::new();
                headers.insert(
                    yaml_key("Authorization"),
                    YamlValue::String(format!("Bearer ${{{name}}}")),
                );
                mapping.insert(yaml_key("headers"), YamlValue::Mapping(headers));
            }
        }
    }
    let internal = mapping
        .entry(yaml_key("_synthchat"))
        .or_insert_with(|| YamlValue::Mapping(YamlMapping::new()));
    let internal = internal
        .as_mapping_mut()
        .ok_or(McpError::StoredConfigInvalid)?;
    internal.insert(yaml_key("id"), YamlValue::String(server.id.clone()));
    Ok(mapping)
}

fn mcp_servers_mapping(document: &YamlValue) -> Result<&YamlMapping, McpError> {
    let root = document.as_mapping().ok_or(McpError::StoredConfigInvalid)?;
    match root.get(yaml_key(MCP_SERVERS_KEY)) {
        None | Some(YamlValue::Null) => Ok(empty_mapping()),
        Some(value) => value.as_mapping().ok_or(McpError::StoredConfigInvalid),
    }
}

fn empty_mapping() -> &'static YamlMapping {
    static EMPTY: std::sync::OnceLock<YamlMapping> = std::sync::OnceLock::new();
    EMPTY.get_or_init(YamlMapping::new)
}

fn set_document_value(
    document: &mut YamlValue,
    key: &str,
    value: YamlValue,
) -> Result<(), McpError> {
    document
        .as_mapping_mut()
        .ok_or(McpError::StoredConfigInvalid)?
        .insert(yaml_key(key), value);
    Ok(())
}

fn optional_yaml_string(mapping: &YamlMapping, key: &str) -> Result<Option<String>, McpError> {
    mapping
        .get(yaml_key(key))
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or(McpError::StoredConfigInvalid)
        })
        .transpose()
}

fn optional_yaml_bool(mapping: &YamlMapping, key: &str) -> Result<Option<bool>, McpError> {
    mapping
        .get(yaml_key(key))
        .map(|value| value.as_bool().ok_or(McpError::StoredConfigInvalid))
        .transpose()
}

fn optional_yaml_u64(mapping: &YamlMapping, key: &str) -> Result<Option<u64>, McpError> {
    mapping
        .get(yaml_key(key))
        .map(|value| value.as_u64().ok_or(McpError::StoredConfigInvalid))
        .transpose()
}

fn yaml_string_sequence(mapping: &YamlMapping, key: &str) -> Result<Vec<String>, McpError> {
    let Some(value) = mapping.get(yaml_key(key)) else {
        return Ok(Vec::new());
    };
    value
        .as_sequence()
        .ok_or(McpError::StoredConfigInvalid)?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or(McpError::StoredConfigInvalid)
        })
        .collect()
}

fn parse_idempotency_records(
    document: &YamlValue,
) -> Result<BTreeMap<String, IdempotencyRecord>, McpError> {
    let root = document.as_mapping().ok_or(McpError::StoredConfigInvalid)?;
    let Some(metadata) = root.get(yaml_key(MCP_METADATA_KEY)) else {
        return Ok(BTreeMap::new());
    };
    let metadata = metadata.as_mapping().ok_or(McpError::StoredConfigInvalid)?;
    if let Some(version) = metadata.get(yaml_key("schema_version"))
        && version.as_u64() != Some(1)
    {
        return Err(McpError::StoredConfigInvalid);
    }
    let Some(records) = metadata.get(yaml_key("idempotency")) else {
        return Ok(BTreeMap::new());
    };
    let records = records.as_mapping().ok_or(McpError::StoredConfigInvalid)?;
    if records.len() > MAX_IDEMPOTENCY_RECORDS {
        return Err(McpError::StoredConfigInvalid);
    }
    let mut parsed = BTreeMap::new();
    for (key, value) in records {
        let key = key.as_str().ok_or(McpError::StoredConfigInvalid)?;
        if key.len() != 64 || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(McpError::StoredConfigInvalid);
        }
        let value = value.as_mapping().ok_or(McpError::StoredConfigInvalid)?;
        let fingerprint = required_yaml_string(value, "fingerprint")?;
        if fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(McpError::StoredConfigInvalid);
        }
        let server_id = required_yaml_string(value, "server_id")?;
        validate_server_id(&server_id).map_err(|_| McpError::StoredConfigInvalid)?;
        let state = match required_yaml_string(value, "state")?.as_str() {
            "present" => IdempotencyState::Present,
            "deleted" => IdempotencyState::Deleted,
            _ => return Err(McpError::StoredConfigInvalid),
        };
        let created_at = required_yaml_string(value, "created_at")?;
        OffsetDateTime::parse(&created_at, &Rfc3339).map_err(|_| McpError::StoredConfigInvalid)?;
        parsed.insert(
            key.to_ascii_lowercase(),
            IdempotencyRecord {
                fingerprint: fingerprint.to_ascii_lowercase(),
                server_id,
                state,
                created_at,
            },
        );
    }
    Ok(parsed)
}

fn write_idempotency_records(
    document: &mut YamlValue,
    records: &BTreeMap<String, IdempotencyRecord>,
) -> Result<(), McpError> {
    let root = document
        .as_mapping_mut()
        .ok_or(McpError::StoredConfigInvalid)?;
    let metadata = root
        .entry(yaml_key(MCP_METADATA_KEY))
        .or_insert_with(|| YamlValue::Mapping(YamlMapping::new()));
    let metadata = metadata
        .as_mapping_mut()
        .ok_or(McpError::StoredConfigInvalid)?;
    metadata.insert(
        yaml_key("schema_version"),
        serde_yaml_ng::to_value(1_u64).map_err(|_| McpError::StoredConfigInvalid)?,
    );
    let mut values = YamlMapping::new();
    for (key, record) in records {
        let mut value = YamlMapping::new();
        value.insert(
            yaml_key("fingerprint"),
            YamlValue::String(record.fingerprint.clone()),
        );
        value.insert(
            yaml_key("server_id"),
            YamlValue::String(record.server_id.clone()),
        );
        value.insert(
            yaml_key("state"),
            YamlValue::String(
                match record.state {
                    IdempotencyState::Present => "present",
                    IdempotencyState::Deleted => "deleted",
                }
                .to_owned(),
            ),
        );
        value.insert(
            yaml_key("created_at"),
            YamlValue::String(record.created_at.clone()),
        );
        values.insert(yaml_key(key), YamlValue::Mapping(value));
    }
    metadata.insert(yaml_key("idempotency"), YamlValue::Mapping(values));
    Ok(())
}

fn cleanup_idempotency_records(
    records: &mut BTreeMap<String, IdempotencyRecord>,
    now: OffsetDateTime,
) -> Result<bool, McpError> {
    let before = records.len();
    let cutoff = now - Duration::hours(IDEMPOTENCY_RETENTION_HOURS);
    records.retain(|_, record| {
        OffsetDateTime::parse(&record.created_at, &Rfc3339)
            .map(|created_at| created_at >= cutoff)
            .unwrap_or(false)
    });
    Ok(records.len() != before)
}

fn required_yaml_string(mapping: &YamlMapping, key: &str) -> Result<String, McpError> {
    mapping
        .get(yaml_key(key))
        .and_then(YamlValue::as_str)
        .map(ToOwned::to_owned)
        .ok_or(McpError::StoredConfigInvalid)
}

fn fingerprint(request: &CreateMcpServer) -> Result<String, McpError> {
    let mut canonical = request.clone();
    if let CreateMcpServer::Stdio {
        env_secret_names, ..
    } = &mut canonical
    {
        env_secret_names.sort();
    }
    serde_json::to_vec(&canonical)
        .map(|bytes| sha256_hex(&bytes))
        .map_err(|_| McpError::InvalidRequest)
}

fn idempotency_key_hash(profile_id: &str, key: &str) -> String {
    sha256_hex(format!("POST\n/api/v1/profiles/{profile_id}/mcp/servers\n{key}").as_bytes())
}

fn new_server_id() -> String {
    format!("mcp_{}", Uuid::new_v4().simple())
}

fn legacy_server_id(profile_id: &str, name: &str) -> String {
    format!(
        "mcp_{}",
        &sha256_hex(format!("legacy-mcp\n{profile_id}\n{name}").as_bytes())[..32]
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

struct StdioClient {
    child: Child,
    peer: JsonRpcPeer<ChildStdout, ChildStdin>,
}

impl StdioClient {
    async fn spawn(
        server: &StoredServer,
        environment: &[(String, SecretString)],
    ) -> Result<Self, McpRuntimeError> {
        let command = server
            .command
            .as_deref()
            .ok_or(McpRuntimeError::Configuration)?;
        let executable = resolve_executable(command)?;
        let mut process = Command::new(executable);
        process
            .args(&server.args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for (name, value) in environment {
            process.env(name, value.expose_secret());
        }
        let mut child = process.spawn().map_err(|_| McpRuntimeError::Transport)?;
        let stdout = child.stdout.take().ok_or(McpRuntimeError::Transport)?;
        let stdin = child.stdin.take().ok_or(McpRuntimeError::Transport)?;
        Ok(Self {
            child,
            peer: JsonRpcPeer::new(stdout, stdin),
        })
    }

    async fn initialize(
        &mut self,
        timeout: StdDuration,
        control: Option<&ToolExecutionControl>,
    ) -> Result<(), McpRuntimeError> {
        let result = self
            .peer
            .request(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {
                        "name": "synthchat",
                        "version": env!("CARGO_PKG_VERSION"),
                    }
                }),
                timeout,
                control,
            )
            .await?;
        let object = result.as_object().ok_or(McpRuntimeError::InvalidProtocol)?;
        if object.get("protocolVersion").and_then(JsonValue::as_str) != Some(MCP_PROTOCOL_VERSION)
            || !object.get("capabilities").is_some_and(JsonValue::is_object)
        {
            return Err(McpRuntimeError::InvalidProtocol);
        }
        self.peer
            .notify("notifications/initialized", json!({}), timeout, control)
            .await
    }

    async fn list_tools(
        &mut self,
        timeout: StdDuration,
        control: Option<&ToolExecutionControl>,
    ) -> Result<Vec<DiscoveredTool>, McpRuntimeError> {
        let mut cursor: Option<String> = None;
        let mut tools = Vec::new();
        let mut names = BTreeSet::new();
        for _ in 0..MAX_MCP_LIST_PAGES {
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |cursor| json!({ "cursor": cursor }));
            let result = self
                .peer
                .request("tools/list", params, timeout, control)
                .await?;
            let object = result.as_object().ok_or(McpRuntimeError::InvalidProtocol)?;
            let page = object
                .get("tools")
                .and_then(JsonValue::as_array)
                .ok_or(McpRuntimeError::InvalidProtocol)?;
            for value in page {
                let tool = parse_discovered_tool(value)?;
                if !names.insert(tool.name.clone()) || tools.len() >= MAX_MCP_TOOLS {
                    return Err(McpRuntimeError::InvalidProtocol);
                }
                tools.push(tool);
            }
            cursor = match object.get("nextCursor") {
                None | Some(JsonValue::Null) => None,
                Some(JsonValue::String(value))
                    if !value.is_empty()
                        && value.len() <= 1024
                        && !value.chars().any(char::is_control) =>
                {
                    Some(value.clone())
                }
                _ => return Err(McpRuntimeError::InvalidProtocol),
            };
            if cursor.is_none() {
                return Ok(tools);
            }
        }
        Err(McpRuntimeError::InvalidProtocol)
    }

    async fn call_tool(
        &mut self,
        name: &str,
        arguments: JsonValue,
        timeout: StdDuration,
        control: &ToolExecutionControl,
    ) -> Result<JsonValue, McpRuntimeError> {
        let result = self
            .peer
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
                timeout,
                Some(control),
            )
            .await?;
        let object = result.as_object().ok_or(McpRuntimeError::InvalidResult)?;
        if !object.get("content").is_some_and(JsonValue::is_array)
            || object
                .get("isError")
                .is_some_and(|value| !value.is_boolean())
            || serde_json::to_vec(&result)
                .map_err(|_| McpRuntimeError::InvalidResult)?
                .len()
                > MAX_MCP_RESULT_BYTES
        {
            return Err(McpRuntimeError::InvalidResult);
        }
        Ok(result)
    }

    async fn close(&mut self) {
        if let Ok(Some(_)) = self.child.try_wait() {
            return;
        }
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(MCP_PROCESS_SHUTDOWN_TIMEOUT, self.child.wait()).await;
    }
}

struct JsonRpcPeer<R, W> {
    reader: BufReader<R>,
    writer: W,
    next_id: u64,
}

impl<R, W> JsonRpcPeer<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    fn new(reader: R, writer: W) -> Self {
        Self {
            reader: BufReader::new(reader),
            writer,
            next_id: 1,
        }
    }

    async fn request(
        &mut self,
        method: &str,
        params: JsonValue,
        timeout: StdDuration,
        control: Option<&ToolExecutionControl>,
    ) -> Result<JsonValue, McpRuntimeError> {
        check_control(control)?;
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(McpRuntimeError::InvalidProtocol)?;
        let deadline = Instant::now() + timeout;
        await_runtime(
            self.send(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            })),
            deadline,
            control,
        )
        .await?;
        loop {
            let message = await_runtime(self.read(), deadline, control).await?;
            let object = message
                .as_object()
                .ok_or(McpRuntimeError::InvalidProtocol)?;
            if object.get("jsonrpc").and_then(JsonValue::as_str) != Some("2.0") {
                return Err(McpRuntimeError::InvalidProtocol);
            }
            if object.contains_key("method") {
                if let Some(request_id) = object.get("id").cloned() {
                    await_runtime(
                        self.send(&json!({
                            "jsonrpc": "2.0",
                            "id": request_id,
                            "error": { "code": -32601, "message": "Method not found" },
                        })),
                        deadline,
                        control,
                    )
                    .await?;
                }
                continue;
            }
            if object.get("id") != Some(&json!(id)) {
                return Err(McpRuntimeError::InvalidProtocol);
            }
            if object.contains_key("error") {
                return Err(McpRuntimeError::Transport);
            }
            return object
                .get("result")
                .cloned()
                .ok_or(McpRuntimeError::InvalidProtocol);
        }
    }

    async fn notify(
        &mut self,
        method: &str,
        params: JsonValue,
        timeout: StdDuration,
        control: Option<&ToolExecutionControl>,
    ) -> Result<(), McpRuntimeError> {
        check_control(control)?;
        await_runtime(
            self.send(&json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params,
            })),
            Instant::now() + timeout,
            control,
        )
        .await
    }

    async fn send(&mut self, value: &JsonValue) -> Result<(), McpRuntimeError> {
        let bytes = serde_json::to_vec(value).map_err(|_| McpRuntimeError::InvalidProtocol)?;
        if bytes.len() > MAX_RPC_MESSAGE_BYTES {
            return Err(McpRuntimeError::InvalidProtocol);
        }
        self.writer
            .write_all(&bytes)
            .await
            .map_err(|_| McpRuntimeError::Transport)?;
        self.writer
            .write_all(b"\n")
            .await
            .map_err(|_| McpRuntimeError::Transport)?;
        self.writer
            .flush()
            .await
            .map_err(|_| McpRuntimeError::Transport)
    }

    async fn read(&mut self) -> Result<JsonValue, McpRuntimeError> {
        let mut bytes = Vec::new();
        let read = self
            .reader
            .read_until(b'\n', &mut bytes)
            .await
            .map_err(|_| McpRuntimeError::Transport)?;
        if read == 0 || bytes.len() > MAX_RPC_MESSAGE_BYTES {
            return Err(McpRuntimeError::InvalidProtocol);
        }
        while matches!(bytes.last(), Some(b'\n' | b'\r')) {
            bytes.pop();
        }
        if bytes.is_empty() {
            return Err(McpRuntimeError::InvalidProtocol);
        }
        serde_json::from_slice(&bytes).map_err(|_| McpRuntimeError::InvalidProtocol)
    }
}

async fn await_runtime<T>(
    future: impl Future<Output = Result<T, McpRuntimeError>>,
    deadline: Instant,
    control: Option<&ToolExecutionControl>,
) -> Result<T, McpRuntimeError> {
    tokio::pin!(future);
    let mut polling = tokio::time::interval(StdDuration::from_millis(25));
    polling.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            result = &mut future => return result,
            _ = tokio::time::sleep_until(deadline) => return Err(McpRuntimeError::Timeout),
            _ = polling.tick() => check_control(control)?,
        }
    }
}

fn check_control(control: Option<&ToolExecutionControl>) -> Result<(), McpRuntimeError> {
    control.map_or(Ok(()), |control| {
        control.check().map_err(|error| match error {
            ToolExecutionControlError::Cancelled => McpRuntimeError::Cancelled,
            ToolExecutionControlError::DeadlineExceeded => McpRuntimeError::Timeout,
        })
    })
}

fn resolve_executable(command: &str) -> Result<PathBuf, McpRuntimeError> {
    let path = Path::new(command);
    if path.components().count() > 1 || path.is_absolute() {
        return canonical_executable(path);
    }
    let search = std::env::var_os("PATH").ok_or(McpRuntimeError::Transport)?;
    for directory in std::env::split_paths(&search) {
        let candidate = directory.join(command);
        if let Ok(path) = canonical_executable(&candidate) {
            return Ok(path);
        }
        #[cfg(windows)]
        if candidate.extension().is_none()
            && let Ok(path) = canonical_executable(&candidate.with_extension("exe"))
        {
            return Ok(path);
        }
    }
    Err(McpRuntimeError::Transport)
}

fn canonical_executable(path: &Path) -> Result<PathBuf, McpRuntimeError> {
    let path = fs::canonicalize(path).map_err(|_| McpRuntimeError::Transport)?;
    let metadata = fs::metadata(&path).map_err(|_| McpRuntimeError::Transport)?;
    if !metadata.is_file()
        || path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                matches!(value.to_ascii_lowercase().as_str(), "cmd" | "bat" | "ps1")
            })
    {
        return Err(McpRuntimeError::Transport);
    }
    Ok(path)
}

fn parse_discovered_tool(value: &JsonValue) -> Result<DiscoveredTool, McpRuntimeError> {
    let object = value.as_object().ok_or(McpRuntimeError::InvalidProtocol)?;
    let name = object
        .get("name")
        .and_then(JsonValue::as_str)
        .ok_or(McpRuntimeError::InvalidProtocol)?;
    if name.is_empty()
        || name.len() > MAX_MCP_TOOL_NAME_BYTES
        || name.trim() != name
        || name.chars().any(char::is_control)
    {
        return Err(McpRuntimeError::InvalidProtocol);
    }
    let description = match object.get("description") {
        None | Some(JsonValue::Null) => format!("MCP tool {name}"),
        Some(JsonValue::String(value)) => normalize_description(value)?,
        _ => return Err(McpRuntimeError::InvalidProtocol),
    };
    let input_schema = object
        .get("inputSchema")
        .cloned()
        .ok_or(McpRuntimeError::InvalidProtocol)?;
    validate_json_schema(&input_schema, true)?;
    let output_schema = object.get("outputSchema").cloned();
    if let Some(schema) = output_schema.as_ref() {
        validate_json_schema(schema, false)?;
    }
    Ok(DiscoveredTool {
        name: name.to_owned(),
        description,
        input_schema,
        output_schema,
    })
}

fn normalize_description(value: &str) -> Result<String, McpRuntimeError> {
    if value.len() > MAX_MCP_DESCRIPTION_BYTES
        || value
            .chars()
            .any(|character| character.is_control() && !character.is_whitespace())
    {
        return Err(McpRuntimeError::InvalidProtocol);
    }
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        Err(McpRuntimeError::InvalidProtocol)
    } else {
        Ok(normalized)
    }
}

fn projected_tool_name(server: &str, tool: &str) -> String {
    let normalized = tool
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-') {
                byte as char
            } else {
                '_'
            }
        })
        .collect::<String>();
    let normalized = normalized.trim_matches('_');
    let normalized = if normalized.is_empty() {
        "tool"
    } else {
        normalized
    };
    let candidate = format!("mcp__{server}__{normalized}");
    if candidate.len() <= MAX_PROVIDER_TOOL_NAME_BYTES {
        return candidate;
    }
    let digest = sha256_hex(candidate.as_bytes());
    let suffix = format!("__{}", &digest[..10]);
    let keep = MAX_PROVIDER_TOOL_NAME_BYTES.saturating_sub(suffix.len());
    format!("{}{}", &candidate[..keep], suffix)
}

fn validate_json_schema(schema: &JsonValue, require_object: bool) -> Result<(), McpRuntimeError> {
    let bytes = serde_json::to_vec(schema).map_err(|_| McpRuntimeError::InvalidProtocol)?;
    if bytes.len() > MAX_MCP_SCHEMA_BYTES || !schema.is_object() {
        return Err(McpRuntimeError::InvalidProtocol);
    }
    let mut nodes = 0usize;
    validate_schema_node(schema, 0, &mut nodes)?;
    if require_object && !schema_declares_type(schema, "object") {
        return Err(McpRuntimeError::InvalidProtocol);
    }
    Ok(())
}

fn validate_schema_node(
    schema: &JsonValue,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), McpRuntimeError> {
    if depth > MAX_MCP_SCHEMA_DEPTH {
        return Err(McpRuntimeError::InvalidProtocol);
    }
    *nodes = nodes
        .checked_add(1)
        .ok_or(McpRuntimeError::InvalidProtocol)?;
    if *nodes > MAX_MCP_SCHEMA_NODES {
        return Err(McpRuntimeError::InvalidProtocol);
    }
    let object = schema.as_object().ok_or(McpRuntimeError::InvalidProtocol)?;
    const ALLOWED: &[&str] = &[
        "$schema",
        "title",
        "description",
        "type",
        "properties",
        "required",
        "additionalProperties",
        "items",
        "enum",
        "const",
        "anyOf",
        "oneOf",
        "allOf",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "minLength",
        "maxLength",
        "pattern",
        "format",
        "minItems",
        "maxItems",
        "uniqueItems",
        "minProperties",
        "maxProperties",
        "default",
        "examples",
    ];
    if object.keys().any(|key| !ALLOWED.contains(&key.as_str())) {
        return Err(McpRuntimeError::InvalidProtocol);
    }
    if let Some(kind) = object.get("type") {
        let valid = match kind {
            JsonValue::String(value) => valid_schema_type(value),
            JsonValue::Array(values) => {
                !values.is_empty()
                    && values.len() <= 8
                    && values
                        .iter()
                        .all(|value| value.as_str().is_some_and(valid_schema_type))
            }
            _ => false,
        };
        if !valid {
            return Err(McpRuntimeError::InvalidProtocol);
        }
    }
    if let Some(properties) = object.get("properties") {
        let properties = properties
            .as_object()
            .ok_or(McpRuntimeError::InvalidProtocol)?;
        for (name, child) in properties {
            if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
                return Err(McpRuntimeError::InvalidProtocol);
            }
            validate_schema_node(child, depth + 1, nodes)?;
        }
    }
    if let Some(required) = object.get("required") {
        let required = required
            .as_array()
            .ok_or(McpRuntimeError::InvalidProtocol)?;
        let names = required
            .iter()
            .map(|value| value.as_str().ok_or(McpRuntimeError::InvalidProtocol))
            .collect::<Result<BTreeSet<_>, _>>()?;
        if names.len() != required.len() {
            return Err(McpRuntimeError::InvalidProtocol);
        }
    }
    if let Some(additional) = object.get("additionalProperties")
        && !additional.is_boolean()
    {
        validate_schema_node(additional, depth + 1, nodes)?;
    }
    if let Some(items) = object.get("items") {
        validate_schema_node(items, depth + 1, nodes)?;
    }
    for keyword in ["anyOf", "oneOf", "allOf"] {
        if let Some(branches) = object.get(keyword) {
            let branches = branches
                .as_array()
                .filter(|branches| !branches.is_empty() && branches.len() <= 16)
                .ok_or(McpRuntimeError::InvalidProtocol)?;
            for branch in branches {
                validate_schema_node(branch, depth + 1, nodes)?;
            }
        }
    }
    if object.get("enum").is_some_and(|value| {
        !value
            .as_array()
            .is_some_and(|values| !values.is_empty() && values.len() <= 128)
    }) || object
        .get("uniqueItems")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(McpRuntimeError::InvalidProtocol);
    }
    for keyword in [
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "minLength",
        "maxLength",
        "minItems",
        "maxItems",
        "minProperties",
        "maxProperties",
    ] {
        if object.get(keyword).is_some_and(|value| !value.is_number()) {
            return Err(McpRuntimeError::InvalidProtocol);
        }
    }
    if let Some(pattern) = object.get("pattern") {
        let pattern = pattern
            .as_str()
            .filter(|pattern| pattern.len() <= 512)
            .ok_or(McpRuntimeError::InvalidProtocol)?;
        Regex::new(pattern).map_err(|_| McpRuntimeError::InvalidProtocol)?;
    }
    Ok(())
}

fn valid_schema_type(value: &str) -> bool {
    matches!(
        value,
        "null" | "boolean" | "object" | "array" | "number" | "integer" | "string"
    )
}

fn schema_declares_type(schema: &JsonValue, expected: &str) -> bool {
    match schema.get("type") {
        Some(JsonValue::String(value)) => value == expected,
        Some(JsonValue::Array(values)) => {
            values.iter().any(|value| value.as_str() == Some(expected))
        }
        _ => false,
    }
}

fn schema_accepts(
    schema: &JsonValue,
    value: &JsonValue,
    depth: usize,
) -> Result<bool, McpRuntimeError> {
    if depth > MAX_MCP_SCHEMA_DEPTH {
        return Err(McpRuntimeError::InvalidProtocol);
    }
    let object = schema.as_object().ok_or(McpRuntimeError::InvalidProtocol)?;
    if let Some(expected) = object.get("const")
        && expected != value
    {
        return Ok(false);
    }
    if let Some(values) = object.get("enum").and_then(JsonValue::as_array)
        && !values.contains(value)
    {
        return Ok(false);
    }
    if let Some(branches) = object.get("allOf").and_then(JsonValue::as_array) {
        for branch in branches {
            if !schema_accepts(branch, value, depth + 1)? {
                return Ok(false);
            }
        }
    }
    if let Some(branches) = object.get("anyOf").and_then(JsonValue::as_array) {
        let mut accepted = false;
        for branch in branches {
            accepted |= schema_accepts(branch, value, depth + 1)?;
        }
        if !accepted {
            return Ok(false);
        }
    }
    if let Some(branches) = object.get("oneOf").and_then(JsonValue::as_array) {
        let mut matches = 0usize;
        for branch in branches {
            matches += usize::from(schema_accepts(branch, value, depth + 1)?);
        }
        if matches != 1 {
            return Ok(false);
        }
    }
    if let Some(kind) = object.get("type") {
        let accepted = match kind {
            JsonValue::String(kind) => value_has_type(value, kind),
            JsonValue::Array(kinds) => kinds
                .iter()
                .filter_map(JsonValue::as_str)
                .any(|kind| value_has_type(value, kind)),
            _ => false,
        };
        if !accepted {
            return Ok(false);
        }
    }
    match value {
        JsonValue::Object(values) => {
            let properties = object.get("properties").and_then(JsonValue::as_object);
            if let Some(required) = object.get("required").and_then(JsonValue::as_array)
                && required
                    .iter()
                    .filter_map(JsonValue::as_str)
                    .any(|name| !values.contains_key(name))
            {
                return Ok(false);
            }
            for (name, child) in values {
                if let Some(child_schema) = properties.and_then(|properties| properties.get(name)) {
                    if !schema_accepts(child_schema, child, depth + 1)? {
                        return Ok(false);
                    }
                } else if let Some(additional) = object.get("additionalProperties") {
                    match additional {
                        JsonValue::Bool(false) => return Ok(false),
                        JsonValue::Bool(true) => {}
                        schema => {
                            if !schema_accepts(schema, child, depth + 1)? {
                                return Ok(false);
                            }
                        }
                    }
                }
            }
            if !numeric_size_accepts(object, values.len(), "minProperties", "maxProperties") {
                return Ok(false);
            }
        }
        JsonValue::Array(values) => {
            if !numeric_size_accepts(object, values.len(), "minItems", "maxItems") {
                return Ok(false);
            }
            if object.get("uniqueItems").and_then(JsonValue::as_bool) == Some(true) {
                for (index, value) in values.iter().enumerate() {
                    if values[index + 1..].contains(value) {
                        return Ok(false);
                    }
                }
            }
            if let Some(items) = object.get("items") {
                for item in values {
                    if !schema_accepts(items, item, depth + 1)? {
                        return Ok(false);
                    }
                }
            }
        }
        JsonValue::String(value) => {
            if !numeric_size_accepts(object, value.chars().count(), "minLength", "maxLength") {
                return Ok(false);
            }
            if let Some(pattern) = object.get("pattern").and_then(JsonValue::as_str)
                && !Regex::new(pattern)
                    .map_err(|_| McpRuntimeError::InvalidProtocol)?
                    .is_match(value)
            {
                return Ok(false);
            }
        }
        JsonValue::Number(value) => {
            let Some(number) = value.as_f64() else {
                return Ok(false);
            };
            if object
                .get("minimum")
                .and_then(JsonValue::as_f64)
                .is_some_and(|minimum| number < minimum)
                || object
                    .get("maximum")
                    .and_then(JsonValue::as_f64)
                    .is_some_and(|maximum| number > maximum)
                || object
                    .get("exclusiveMinimum")
                    .and_then(JsonValue::as_f64)
                    .is_some_and(|minimum| number <= minimum)
                || object
                    .get("exclusiveMaximum")
                    .and_then(JsonValue::as_f64)
                    .is_some_and(|maximum| number >= maximum)
            {
                return Ok(false);
            }
        }
        JsonValue::Null | JsonValue::Bool(_) => {}
    }
    Ok(true)
}

fn value_has_type(value: &JsonValue, expected: &str) -> bool {
    match expected {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "number" => value.is_number(),
        "integer" => value
            .as_number()
            .is_some_and(|number| number.as_i64().is_some() || number.as_u64().is_some()),
        "string" => value.is_string(),
        _ => false,
    }
}

fn numeric_size_accepts(
    schema: &JsonMap<String, JsonValue>,
    actual: usize,
    minimum: &str,
    maximum: &str,
) -> bool {
    schema
        .get(minimum)
        .and_then(JsonValue::as_u64)
        .is_none_or(|minimum| actual >= minimum as usize)
        && schema
            .get(maximum)
            .and_then(JsonValue::as_u64)
            .is_none_or(|maximum| actual <= maximum as usize)
}

fn redact_json(value: &mut JsonValue, secrets: &[SecretString]) {
    match value {
        JsonValue::String(value) => {
            for secret in secrets {
                let secret = secret.expose_secret();
                if !secret.is_empty() && value.contains(secret) {
                    *value = value.replace(secret, "[REDACTED]");
                }
            }
        }
        JsonValue::Array(values) => {
            for value in values {
                redact_json(value, secrets);
            }
        }
        JsonValue::Object(values) => {
            for value in values.values_mut() {
                redact_json(value, secrets);
            }
        }
        JsonValue::Null | JsonValue::Bool(_) | JsonValue::Number(_) => {}
    }
}

fn yaml_key(value: &str) -> YamlValue {
    YamlValue::String(value.to_owned())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        convert::Infallible,
        fs,
        sync::{Barrier, Mutex},
        thread,
    };

    use axum::{
        Json, Router,
        body::{Body, Bytes},
        extract::{Query, State},
        http::{HeaderMap, Response, StatusCode, header},
        routing::{get, post},
    };
    use keyring_core::CredentialStore;
    use secrecy::SecretString;
    use serde_json::json;
    use tempfile::TempDir;
    use tokio::sync::mpsc;

    use super::*;

    const TOKEN_SECRET: &str = "MCP_TOKEN";

    struct Fixture {
        home: TempDir,
        store: Arc<CredentialStore>,
        profiles: Arc<ProfileService>,
        service: McpService,
    }

    impl Fixture {
        fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            let store: Arc<CredentialStore> = keyring_core::mock::Store::new().unwrap();
            let profiles = Arc::new(ProfileService::with_credential_store(
                home.path().to_owned(),
                store.clone(),
            ));
            let service = McpService::new(profiles.clone());
            Self {
                home,
                store,
                profiles,
                service,
            }
        }

        fn config_path(&self) -> std::path::PathBuf {
            self.home.path().join("config.yaml")
        }
    }

    fn stdio_request(name: &str) -> CreateMcpServer {
        CreateMcpServer::Stdio {
            name: name.to_owned(),
            command: "npx".to_owned(),
            args: vec!["-y".to_owned(), "@example/mcp".to_owned()],
            enabled: true,
            timeout_seconds: 30,
            env_secret_names: vec![TOKEN_SECRET.to_owned()],
        }
    }

    fn http_request(name: &str) -> CreateMcpServer {
        CreateMcpServer::StreamableHttp {
            name: name.to_owned(),
            url: "https://example.com/mcp".to_owned(),
            enabled: true,
            timeout_seconds: 45,
            bearer_token_secret_name: Some(TOKEN_SECRET.to_owned()),
        }
    }

    #[test]
    fn all_transports_round_trip_with_references_only() {
        let fixture = Fixture::new();
        fixture
            .profiles
            .put_secret(
                "default",
                TOKEN_SECRET,
                &SecretString::from("never-project-this".to_owned()),
            )
            .unwrap();
        let stdio = fixture
            .service
            .create_server("default", &stdio_request("local"), "stdio-key-0001")
            .unwrap();
        let http = fixture
            .service
            .create_server("default", &http_request("remote"), "http-key-00001")
            .unwrap();
        let sse_request = CreateMcpServer::Sse {
            name: "events".to_owned(),
            url: "http://127.0.0.1:9000/sse".to_owned(),
            enabled: false,
            timeout_seconds: 60,
            bearer_token_secret_name: None,
        };
        let sse = fixture
            .service
            .create_server("default", &sse_request, "sse-key-000001")
            .unwrap();

        assert_eq!(stdio.value.transport, McpTransport::Stdio);
        assert_eq!(http.value.transport, McpTransport::StreamableHttp);
        assert_eq!(sse.value.transport, McpTransport::Sse);
        let listed = fixture.service.list_servers("default").unwrap();
        assert_eq!(listed.value.len(), 3);
        assert!(
            listed
                .value
                .iter()
                .all(|server| server.missing_secret_names.is_empty())
        );
        let public = serde_json::to_string(&listed.value).unwrap();
        assert!(!public.contains("never-project-this"));

        let persisted = fs::read_to_string(fixture.config_path()).unwrap();
        assert!(persisted.contains("${MCP_TOKEN}"));
        assert!(persisted.contains("Bearer ${MCP_TOKEN}"));
        assert!(!persisted.contains("never-project-this"));
        assert!(!persisted.contains("process.env"));
    }

    #[test]
    fn missing_secret_status_detects_external_keychain_deletion() {
        let fixture = Fixture::new();
        fixture
            .profiles
            .put_secret(
                "default",
                TOKEN_SECRET,
                &SecretString::from("configured".to_owned()),
            )
            .unwrap();
        fixture
            .service
            .create_server("default", &stdio_request("local"), "stale-key-0001")
            .unwrap();
        fixture
            .store
            .build("cc.synthchat.v1.hermes.secrets", "default:MCP_TOKEN", None)
            .unwrap()
            .delete_credential()
            .unwrap();

        let listed = fixture.service.list_servers("default").unwrap();
        assert_eq!(
            listed.value[0].missing_secret_names,
            vec![TOKEN_SECRET.to_owned()]
        );
    }

    #[test]
    fn referenced_secrets_fail_closed_when_keychain_is_unavailable() {
        let home = tempfile::tempdir().unwrap();
        fs::write(
            home.path().join("config.yaml"),
            "mcp_servers:\n  local:\n    command: npx\n    args: []\n    env:\n      MCP_TOKEN: '${MCP_TOKEN}'\n",
        )
        .unwrap();
        let profiles = Arc::new(ProfileService::without_credential_store(
            home.path().to_owned(),
        ));
        let service = McpService::new(profiles);
        assert!(matches!(
            service.list_servers("default"),
            Err(McpError::Profile(ProfileError::SecretStorageUnavailable))
        ));
    }

    #[test]
    fn no_secret_configuration_works_without_a_keychain() {
        let home = tempfile::tempdir().unwrap();
        let profiles = Arc::new(ProfileService::without_credential_store(
            home.path().to_owned(),
        ));
        let service = McpService::new(profiles);
        let request = CreateMcpServer::StreamableHttp {
            name: "public".to_owned(),
            url: "https://example.com/mcp".to_owned(),
            enabled: true,
            timeout_seconds: 30,
            bearer_token_secret_name: None,
        };
        let created = service
            .create_server("default", &request, "no-secret-key01")
            .unwrap();
        assert!(created.value.missing_secret_names.is_empty());
        assert_eq!(service.list_servers("default").unwrap().value.len(), 1);
    }

    #[test]
    fn keychain_failure_cannot_commit_create_or_patch() {
        let home = tempfile::tempdir().unwrap();
        let profiles = Arc::new(ProfileService::without_credential_store(
            home.path().to_owned(),
        ));
        let service = McpService::new(profiles.clone());
        let before = profiles.get_config("default").unwrap();
        assert!(matches!(
            service.create_server("default", &stdio_request("blocked"), "blocked-create1"),
            Err(McpError::Profile(ProfileError::SecretStorageUnavailable))
        ));
        assert_eq!(profiles.get_config("default").unwrap().etag, before.etag);
        assert!(!home.path().join("config.yaml").exists());

        let request = CreateMcpServer::Stdio {
            name: "local".to_owned(),
            command: "npx".to_owned(),
            args: Vec::new(),
            enabled: true,
            timeout_seconds: 30,
            env_secret_names: Vec::new(),
        };
        let created = service
            .create_server("default", &request, "allowed-create1")
            .unwrap();
        let bytes_before = fs::read(home.path().join("config.yaml")).unwrap();
        assert!(matches!(
            service.update_server(
                "default",
                &created.value.id,
                &created.etag,
                &McpServerPatch {
                    env_secret_names: Some(vec![TOKEN_SECRET.to_owned()]),
                    ..McpServerPatch::default()
                }
            ),
            Err(McpError::Profile(ProfileError::SecretStorageUnavailable))
        ));
        assert_eq!(
            fs::read(home.path().join("config.yaml")).unwrap(),
            bytes_before
        );
        assert_eq!(profiles.get_config("default").unwrap().etag, created.etag);
    }

    #[test]
    fn legacy_ids_are_stable_and_unknown_yaml_survives_mutation() {
        let fixture = Fixture::new();
        fs::write(
            fixture.config_path(),
            "unknown:\n  nested: 42\nmcp_servers:\n  legacy:\n    command: npx\n    args: ['-y', '@example/mcp']\n    env: {}\n    tools:\n      resources: false\n",
        )
        .unwrap();
        let initial = fixture.service.list_servers("default").unwrap();
        let id = initial.value[0].id.clone();
        let updated = fixture
            .service
            .update_server(
                "default",
                &id,
                &initial.etag,
                &McpServerPatch {
                    enabled: Some(false),
                    ..McpServerPatch::default()
                },
            )
            .unwrap();
        assert_eq!(updated.value.id, id);
        let document: YamlValue =
            serde_yaml_ng::from_slice(&fs::read(fixture.config_path()).unwrap()).unwrap();
        assert_eq!(
            document
                .as_mapping()
                .unwrap()
                .get(yaml_key("unknown"))
                .unwrap()
                .as_mapping()
                .unwrap()
                .get(yaml_key("nested"))
                .unwrap()
                .as_u64(),
            Some(42)
        );
        let legacy = document
            .as_mapping()
            .unwrap()
            .get(yaml_key("mcp_servers"))
            .unwrap()
            .as_mapping()
            .unwrap()
            .get(yaml_key("legacy"))
            .unwrap()
            .as_mapping()
            .unwrap();
        assert!(legacy.contains_key(yaml_key("tools")));
        assert_eq!(
            legacy
                .get(yaml_key("_synthchat"))
                .unwrap()
                .as_mapping()
                .unwrap()
                .get(yaml_key("id"))
                .unwrap()
                .as_str(),
            Some(id.as_str())
        );
        let reconstructed = McpService::new(fixture.profiles.clone());
        assert_eq!(
            reconstructed.list_servers("default").unwrap().value[0].id,
            id
        );
    }

    #[test]
    fn literal_stored_secrets_are_rejected_without_disclosure() {
        for yaml in [
            "mcp_servers:\n  bad:\n    command: npx\n    env:\n      MCP_TOKEN: plaintext-value\n",
            "mcp_servers:\n  bad:\n    url: https://example.com/mcp\n    headers:\n      Authorization: Bearer plaintext-value\n",
            "mcp_servers:\n  bad:\n    url: https://example.com/mcp\n    headers:\n      X-Api-Key: plaintext-value\n",
        ] {
            let fixture = Fixture::new();
            fs::write(fixture.config_path(), yaml).unwrap();
            let error = fixture.service.list_servers("default").unwrap_err();
            assert!(matches!(error, McpError::StoredConfigInvalid));
            assert!(!error.to_string().contains("plaintext-value"));
        }
    }

    #[test]
    fn create_replay_conflict_and_deleted_resource_are_durable() {
        let fixture = Fixture::new();
        let first = fixture
            .service
            .create_server("default", &http_request("remote"), "durable-key-0001")
            .unwrap();
        let replay = fixture
            .service
            .create_server("default", &http_request("remote"), "durable-key-0001")
            .unwrap();
        assert_eq!(replay.value.id, first.value.id);
        assert_eq!(replay.etag, first.etag);

        assert!(matches!(
            fixture
                .service
                .create_server("default", &http_request("other"), "durable-key-0001"),
            Err(McpError::IdempotencyConflict)
        ));
        let deleted = fixture
            .service
            .delete_server("default", &first.value.id, &first.etag)
            .unwrap();
        assert_ne!(deleted.etag, first.etag);
        assert!(matches!(
            fixture
                .service
                .create_server("default", &http_request("remote"), "durable-key-0001"),
            Err(McpError::IdempotencyResourceGone)
        ));
    }

    #[test]
    fn reordered_secret_set_has_stable_response_and_idempotent_fingerprint() {
        let fixture = Fixture::new();
        let mut first_request = stdio_request("local");
        let CreateMcpServer::Stdio {
            env_secret_names, ..
        } = &mut first_request
        else {
            unreachable!()
        };
        *env_secret_names = vec!["Z_TOKEN".to_owned(), "A_TOKEN".to_owned()];
        let mut replay_request = first_request.clone();
        let CreateMcpServer::Stdio {
            env_secret_names, ..
        } = &mut replay_request
        else {
            unreachable!()
        };
        env_secret_names.reverse();
        let first = fixture
            .service
            .create_server("default", &first_request, "ordered-key-001")
            .unwrap();
        let replay = fixture
            .service
            .create_server("default", &replay_request, "ordered-key-001")
            .unwrap();
        assert_eq!(first.value.id, replay.value.id);
        assert_eq!(first.value.env_secret_names, vec!["A_TOKEN", "Z_TOKEN"]);
        assert_eq!(replay.value.env_secret_names, first.value.env_secret_names);
        assert_eq!(replay.etag, first.etag);
    }

    #[test]
    fn create_replay_projects_the_current_server_secret_references() {
        let fixture = Fixture::new();
        fixture
            .profiles
            .put_secret(
                "default",
                "B_TOKEN",
                &SecretString::from("configured-b".to_owned()),
            )
            .unwrap();
        let mut request = stdio_request("local");
        let CreateMcpServer::Stdio {
            env_secret_names, ..
        } = &mut request
        else {
            unreachable!()
        };
        *env_secret_names = vec!["A_TOKEN".to_owned()];
        let created = fixture
            .service
            .create_server("default", &request, "changed-ref-key1")
            .unwrap();
        assert_eq!(created.value.missing_secret_names, vec!["A_TOKEN"]);
        let updated = fixture
            .service
            .update_server(
                "default",
                &created.value.id,
                &created.etag,
                &McpServerPatch {
                    env_secret_names: Some(vec!["B_TOKEN".to_owned()]),
                    ..McpServerPatch::default()
                },
            )
            .unwrap();
        assert!(updated.value.missing_secret_names.is_empty());
        let replay = fixture
            .service
            .create_server("default", &request, "changed-ref-key1")
            .unwrap();
        assert_eq!(replay.value.id, created.value.id);
        assert_eq!(replay.value.env_secret_names, vec!["B_TOKEN"]);
        assert!(replay.value.missing_secret_names.is_empty());
    }

    #[test]
    fn patch_rename_collision_noop_etag_and_delete_idempotency() {
        let fixture = Fixture::new();
        let first = fixture
            .service
            .create_server("default", &http_request("one"), "rename-key-0001")
            .unwrap();
        let second = fixture
            .service
            .create_server("default", &http_request("two"), "rename-key-0002")
            .unwrap();
        let no_op = fixture
            .service
            .update_server(
                "default",
                &first.value.id,
                &second.etag,
                &McpServerPatch::default(),
            )
            .unwrap();
        assert_eq!(no_op.etag, second.etag);
        assert!(matches!(
            fixture.service.update_server(
                "default",
                &first.value.id,
                &no_op.etag,
                &McpServerPatch {
                    name: Some("two".to_owned()),
                    ..McpServerPatch::default()
                }
            ),
            Err(McpError::NameConflict)
        ));
        let deleted = fixture
            .service
            .delete_server("default", &first.value.id, &no_op.etag)
            .unwrap();
        let replay = fixture
            .service
            .delete_server("default", &first.value.id, &deleted.etag)
            .unwrap();
        assert_eq!(replay.etag, deleted.etag);
    }

    #[test]
    fn concurrent_updates_with_one_revision_have_one_winner() {
        let fixture = Fixture::new();
        let created = fixture
            .service
            .create_server("default", &http_request("remote"), "concurrent-key1")
            .unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let first_service = fixture.service.clone();
        let second_service = fixture.service.clone();
        let id = created.value.id.clone();
        let etag = created.etag.clone();
        let first_barrier = barrier.clone();
        let first_id = id.clone();
        let first_etag = etag.clone();
        let first = thread::spawn(move || {
            first_barrier.wait();
            first_service.update_server(
                "default",
                &first_id,
                &first_etag,
                &McpServerPatch {
                    enabled: Some(false),
                    ..McpServerPatch::default()
                },
            )
        });
        let second_barrier = barrier.clone();
        let second = thread::spawn(move || {
            second_barrier.wait();
            second_service.update_server(
                "default",
                &id,
                &etag,
                &McpServerPatch {
                    timeout_seconds: Some(31),
                    ..McpServerPatch::default()
                },
            )
        });
        barrier.wait();
        let results = [first.join().unwrap(), second.join().unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(
                    result,
                    Err(McpError::Profile(ProfileError::RevisionConflict { .. }))
                ))
                .count(),
            1
        );
    }

    #[test]
    fn exact_input_bounds_and_transport_combinations_are_enforced() {
        let valid = StoredServer {
            id: new_server_id(),
            name: "a".repeat(MAX_NAME_BYTES),
            transport: McpTransport::Stdio,
            command: Some("C:\\Program Files\\mcp.exe".to_owned()),
            args: vec!["x".repeat(MAX_ARG_BYTES); MAX_ARGS],
            url: None,
            enabled: true,
            timeout_seconds: MAX_TIMEOUT_SECONDS,
            env_secret_names: vec!["MCP_TOKEN_1".to_owned()],
            bearer_token_secret_name: None,
        };
        assert!(
            validate_server(&valid).is_err(),
            "aggregate arg limit is authoritative"
        );
        let mut valid = valid;
        valid.args = vec!["x".repeat(MAX_ARGS_BYTES / MAX_ARGS); MAX_ARGS];
        assert!(validate_server(&valid).is_ok());
        valid.name.push('x');
        assert!(validate_server(&valid).is_err());

        for command in [
            "bash",
            "cmd.exe",
            "powershell",
            "npx --yes package",
            "npx;curl",
            "npx\nother",
        ] {
            assert!(validate_command(command).is_err(), "accepted {command}");
        }
        for arg in [
            "--api-key=plaintext",
            "--token",
            "PASSWORD=plaintext",
            "--authorization=Bearer-value",
            "--credential-file=path",
        ] {
            assert!(sensitive_argument(arg), "accepted sensitive argument {arg}");
        }
        assert!(!sensitive_argument("--transport=http"));
        for url in [
            "http://example.com/mcp",
            "https://user@example.com/mcp",
            "https://example.com/mcp?token=x",
            "https://169.254.169.254/latest",
            "https://10.0.0.1/mcp",
            "https://[fe80::1]/mcp",
        ] {
            assert!(validate_remote_url(url).is_err(), "accepted {url}");
        }
        for url in [
            "https://example.com/mcp",
            "https://8.8.8.8/mcp",
            "http://127.0.0.1:9000/mcp",
            "http://localhost:9000/sse",
        ] {
            assert!(validate_remote_url(url).is_ok(), "rejected {url}");
        }
        assert!(validate_idempotency_key("short").is_err());
        assert!(validate_idempotency_key("valid-key").is_ok());
        assert!(
            serde_json::from_value::<CreateMcpServer>(json!({
                "transport": "stdio",
                "name": "bad",
                "command": "npx",
                "args": [],
                "enabled": true,
                "timeoutSeconds": 30,
                "envSecretNames": [],
                "url": "https://example.com"
            }))
            .is_err()
        );
    }

    #[test]
    fn dynamic_projection_and_json_schema_are_bounded_and_enforced() {
        let schema = json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "minLength": 2, "pattern": "^[a-z]+$"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 5}
            },
            "required": ["query"],
            "additionalProperties": false
        });
        validate_json_schema(&schema, true).unwrap();
        assert!(schema_accepts(&schema, &json!({"query": "rust", "limit": 3}), 0).unwrap());
        assert!(!schema_accepts(&schema, &json!({"query": "R", "extra": true}), 0).unwrap());
        assert!(
            validate_json_schema(&json!({"$ref": "https://example.com/schema"}), true).is_err()
        );

        let short = projected_tool_name("local", "search.files");
        assert_eq!(short, "mcp__local__search_files");
        let long = projected_tool_name("server-name", &"x".repeat(200));
        assert!(long.len() <= MAX_PROVIDER_TOOL_NAME_BYTES);
        assert!(long.starts_with("mcp__server-name__"));
        assert_eq!(long, projected_tool_name("server-name", &"x".repeat(200)));
    }

    #[tokio::test]
    async fn json_rpc_peer_frames_correlates_and_observes_cancellation() {
        let (client, server) = tokio::io::duplex(16 * 1024);
        let (client_read, client_write) = tokio::io::split(client);
        let (server_read, mut server_write) = tokio::io::split(server);
        let server_task = tokio::spawn(async move {
            let mut reader = BufReader::new(server_read);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let request: JsonValue = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "initialize");
            server_write
                .write_all(
                    b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{}}}\n",
                )
                .await
                .unwrap();
        });
        let mut peer = JsonRpcPeer::new(client_read, client_write);
        let result = peer
            .request("initialize", json!({}), StdDuration::from_secs(1), None)
            .await
            .unwrap();
        assert_eq!(result["protocolVersion"], MCP_PROTOCOL_VERSION);
        server_task.await.unwrap();

        let (client, _server) = tokio::io::duplex(1024);
        let (read, write) = tokio::io::split(client);
        let mut peer = JsonRpcPeer::new(read, write);
        let control =
            ToolExecutionControl::new(std::time::Instant::now() + StdDuration::from_secs(5));
        control.cancel();
        assert_eq!(
            peer.request(
                "tools/list",
                json!({}),
                StdDuration::from_secs(5),
                Some(&control)
            )
            .await,
            Err(McpRuntimeError::Cancelled)
        );
    }

    #[tokio::test]
    async fn stdio_runtime_discovers_calls_redacts_and_reaps_the_child() {
        let fixture = Fixture::new();
        fixture
            .profiles
            .put_secret(
                "default",
                TOKEN_SECRET,
                &SecretString::from("fixture-secret-value".to_owned()),
            )
            .unwrap();
        let executable = compile_stdio_fixture(&fixture.home);
        let server = StoredServer {
            id: "mcp_0123456789abcdef0123456789abcdef".to_owned(),
            name: "fixture".to_owned(),
            transport: McpTransport::Stdio,
            command: Some(executable.to_string_lossy().into_owned()),
            args: Vec::new(),
            url: None,
            enabled: true,
            timeout_seconds: 5,
            env_secret_names: vec![TOKEN_SECRET.to_owned()],
            bearer_token_secret_name: None,
        };
        let discovered = fixture
            .service
            .discover_stdio("default", &server)
            .await
            .unwrap();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].name, "echo");
        let binding = McpToolBinding {
            provider_name: projected_tool_name(&server.name, &discovered[0].name),
            server: server.clone(),
            upstream_name: discovered[0].name.clone(),
            description: discovered[0].description.clone(),
            input_schema: discovered[0].input_schema.clone(),
            output_schema: discovered[0].output_schema.clone(),
        };
        let control =
            ToolExecutionControl::new(std::time::Instant::now() + StdDuration::from_secs(10));
        let output = fixture
            .service
            .call_tool("default", &binding, r#"{"text":"hello"}"#, &control)
            .await
            .unwrap();
        assert!(output.provider_content.contains("[REDACTED]"));
        assert!(!output.provider_content.contains("fixture-secret-value"));

        let environment = fixture
            .service
            .secret_environment("default", &server)
            .unwrap();
        let mut client = StdioClient::spawn(&server, &environment).await.unwrap();
        client
            .initialize(StdDuration::from_secs(5), None)
            .await
            .unwrap();
        client.close().await;
        assert!(client.child.try_wait().unwrap().is_some());
    }

    const REMOTE_SECRET: &str = "MCP_REMOTE_TOKEN";
    const REMOTE_SECRET_VALUE: &str = "fixture-bearer-secret";

    #[derive(Clone)]
    struct StreamableFixture {
        requests: Arc<Mutex<Vec<String>>>,
    }

    async fn streamable_post(
        State(fixture): State<StreamableFixture>,
        headers: HeaderMap,
        Json(request): Json<JsonValue>,
    ) -> Response<Body> {
        if headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            != Some("Bearer fixture-bearer-secret")
        {
            return fixture_response(StatusCode::UNAUTHORIZED, "missing bearer");
        }
        let method = request
            .get("method")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        fixture.requests.lock().unwrap().push(method.to_owned());
        let initialized = method == "initialize";
        if initialized
            && (headers.contains_key("mcp-session-id")
                || headers.contains_key("mcp-protocol-version"))
        {
            return fixture_response(StatusCode::BAD_REQUEST, "unexpected initial headers");
        }
        if !initialized
            && (headers
                .get("mcp-session-id")
                .and_then(|value| value.to_str().ok())
                != Some("fixture-session")
                || headers
                    .get("mcp-protocol-version")
                    .and_then(|value| value.to_str().ok())
                    != Some("2025-06-18"))
        {
            return fixture_response(StatusCode::BAD_REQUEST, "missing negotiated headers");
        }
        let id = request.get("id").cloned();
        match method {
            "initialize" => fixture_json(
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {"protocolVersion": "2025-06-18", "capabilities": {}}
                }),
                "fixture-session",
            ),
            "notifications/initialized" => fixture_empty(StatusCode::ACCEPTED, "fixture-session"),
            "tools/list" => fixture_sse(
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "tools": [{
                            "name": "echo",
                            "description": "Fixture echo",
                            "inputSchema": {
                                "type": "object",
                                "properties": {"text": {"type": "string"}},
                                "required": ["text"],
                                "additionalProperties": false
                            }
                        }]
                    }
                }),
                "fixture-session",
            ),
            "tools/call" => fixture_json(
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{"type": "text", "text": REMOTE_SECRET_VALUE}],
                        "isError": false
                    }
                }),
                "fixture-session",
            ),
            _ => fixture_response(StatusCode::BAD_REQUEST, "unknown method"),
        }
    }

    async fn streamable_delete(headers: HeaderMap) -> Response<Body> {
        if headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            != Some("Bearer fixture-bearer-secret")
            || headers
                .get("mcp-session-id")
                .and_then(|value| value.to_str().ok())
                != Some("fixture-session")
            || headers
                .get("mcp-protocol-version")
                .and_then(|value| value.to_str().ok())
                != Some("2025-06-18")
        {
            return fixture_response(StatusCode::BAD_REQUEST, "missing close headers");
        }
        fixture_empty(StatusCode::NO_CONTENT, "fixture-session")
    }

    fn fixture_json(value: JsonValue, session: &str) -> Response<Body> {
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .header("mcp-session-id", session)
            .body(Body::from(value.to_string()))
            .unwrap()
    }

    fn fixture_sse(value: JsonValue, session: &str) -> Response<Body> {
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header("mcp-session-id", session)
            .body(Body::from(format!("event: message\ndata: {value}\n\n")))
            .unwrap()
    }

    fn fixture_empty(status: StatusCode, session: &str) -> Response<Body> {
        Response::builder()
            .status(status)
            .header("mcp-session-id", session)
            .body(Body::empty())
            .unwrap()
    }

    fn fixture_response(status: StatusCode, message: &str) -> Response<Body> {
        Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from(message.to_owned()))
            .unwrap()
    }

    #[derive(Clone)]
    struct LegacySseFixture {
        channels: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<String>>>>,
        next_session: Arc<std::sync::atomic::AtomicUsize>,
    }

    async fn legacy_sse_get(
        State(fixture): State<LegacySseFixture>,
        headers: HeaderMap,
    ) -> Response<Body> {
        if headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            != Some("Bearer fixture-bearer-secret")
        {
            return fixture_response(StatusCode::UNAUTHORIZED, "missing bearer");
        }
        let number = fixture
            .next_session
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let session = format!("fixture-{number}");
        let (sender, mut receiver) = mpsc::unbounded_channel();
        fixture
            .channels
            .lock()
            .unwrap()
            .insert(session.clone(), sender);
        let initial = format!("event: endpoint\ndata: /messages?session={session}\n\n");
        let stream = async_stream::stream! {
            yield Ok::<_, Infallible>(Bytes::from(initial));
            while let Some(event) = receiver.recv().await {
                yield Ok::<_, Infallible>(Bytes::from(event));
            }
        };
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from_stream(stream))
            .unwrap()
    }

    async fn legacy_sse_post(
        State(fixture): State<LegacySseFixture>,
        Query(query): Query<HashMap<String, String>>,
        headers: HeaderMap,
        Json(request): Json<JsonValue>,
    ) -> Response<Body> {
        if headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            != Some("Bearer fixture-bearer-secret")
        {
            return fixture_response(StatusCode::UNAUTHORIZED, "missing bearer");
        }
        let Some(session) = query.get("session") else {
            return fixture_response(StatusCode::BAD_REQUEST, "missing session");
        };
        let Some(sender) = fixture.channels.lock().unwrap().get(session).cloned() else {
            return fixture_response(StatusCode::NOT_FOUND, "unknown session");
        };
        let method = request
            .get("method")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let id = request.get("id").cloned();
        let result = match method {
            "initialize" => {
                Some(json!({"protocolVersion": MCP_PROTOCOL_VERSION, "capabilities": {}}))
            }
            "tools/list" => Some(json!({
                "tools": [{
                    "name": "echo",
                    "description": "Fixture echo",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"text": {"type": "string"}},
                        "required": ["text"],
                        "additionalProperties": false
                    }
                }]
            })),
            "tools/call" => Some(json!({
                "content": [{"type": "text", "text": REMOTE_SECRET_VALUE}],
                "isError": false
            })),
            "notifications/initialized" => None,
            _ => return fixture_response(StatusCode::BAD_REQUEST, "unknown method"),
        };
        if let (Some(id), Some(result)) = (id, result) {
            let response = json!({"jsonrpc": "2.0", "id": id, "result": result});
            if sender
                .send(format!("event: message\ndata: {response}\n\n"))
                .is_err()
            {
                return fixture_response(StatusCode::CONFLICT, "stream closed");
            }
        }
        Response::builder()
            .status(StatusCode::ACCEPTED)
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn remote_runtime_exercises_streamable_http_and_legacy_sse() {
        let streamable = StreamableFixture {
            requests: Arc::new(Mutex::new(Vec::new())),
        };
        let streamable_router = Router::new()
            .route("/mcp", post(streamable_post).delete(streamable_delete))
            .with_state(streamable.clone());
        let (streamable_url, streamable_task) = spawn_mcp_fixture(streamable_router).await;
        exercise_remote_transport(&streamable_url, McpTransport::StreamableHttp).await;
        let calls = streamable.requests.lock().unwrap().clone();
        assert!(
            calls
                .iter()
                .filter(|call| call.as_str() == "initialize")
                .count()
                >= 2
        );
        assert!(calls.iter().any(|call| call == "tools/list"));
        assert!(calls.iter().any(|call| call == "tools/call"));
        streamable_task.abort();

        let legacy = LegacySseFixture {
            channels: Arc::new(Mutex::new(HashMap::new())),
            next_session: Arc::new(std::sync::atomic::AtomicUsize::new(1)),
        };
        let legacy_router = Router::new()
            .route("/sse", get(legacy_sse_get))
            .route("/messages", post(legacy_sse_post))
            .with_state(legacy);
        let (legacy_url, legacy_task) = spawn_mcp_fixture(legacy_router).await;
        exercise_remote_transport(&legacy_url, McpTransport::Sse).await;
        legacy_task.abort();
    }

    async fn spawn_mcp_fixture(router: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (format!("http://{address}"), task)
    }

    async fn exercise_remote_transport(base_url: &str, transport: McpTransport) {
        let fixture = Fixture::new();
        fixture
            .profiles
            .put_secret(
                "default",
                REMOTE_SECRET,
                &SecretString::from(REMOTE_SECRET_VALUE.to_owned()),
            )
            .unwrap();
        let (name, url, request) = match transport {
            McpTransport::StreamableHttp => (
                "streamable",
                format!("{base_url}/mcp"),
                CreateMcpServer::StreamableHttp {
                    name: "streamable".to_owned(),
                    url: format!("{base_url}/mcp"),
                    enabled: true,
                    timeout_seconds: 5,
                    bearer_token_secret_name: Some(REMOTE_SECRET.to_owned()),
                },
            ),
            McpTransport::Sse => (
                "legacy",
                format!("{base_url}/sse"),
                CreateMcpServer::Sse {
                    name: "legacy".to_owned(),
                    url: format!("{base_url}/sse"),
                    enabled: true,
                    timeout_seconds: 5,
                    bearer_token_secret_name: Some(REMOTE_SECRET.to_owned()),
                },
            ),
            McpTransport::Stdio => panic!("not a remote transport"),
        };
        fixture
            .service
            .create_server("default", &request, "remote-runtime-fixture")
            .unwrap();
        let tools = fixture.service.discover_tools("default").await.unwrap();
        assert_eq!(tools.len(), 1, "{url}");
        assert_eq!(tools[0].provider_name(), format!("mcp__{name}__echo"));
        let control =
            ToolExecutionControl::new(std::time::Instant::now() + StdDuration::from_secs(10));
        let output = fixture
            .service
            .call_tool("default", &tools[0], r#"{"text":"hello"}"#, &control)
            .await
            .unwrap();
        assert!(output.provider_content.contains("[REDACTED]"));
        assert!(!output.provider_content.contains(REMOTE_SECRET_VALUE));
    }

    fn compile_stdio_fixture(home: &TempDir) -> PathBuf {
        let source = home.path().join("mcp_stdio_fixture.rs");
        let executable = home.path().join(if cfg!(windows) {
            "mcp_stdio_fixture.exe"
        } else {
            "mcp_stdio_fixture"
        });
        fs::write(
            &source,
            r#"
use std::io::{self, BufRead, Write};

fn request_id(line: &str) -> &str {
    line.split("\"id\":").nth(1).unwrap().split(|ch| ch == ',' || ch == '}').next().unwrap()
}

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line.unwrap();
        if line.contains("\"method\":\"initialize\"") {
            writeln!(stdout, "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{{}}}}}}", request_id(&line)).unwrap();
        } else if line.contains("\"method\":\"tools/list\"") {
            writeln!(stdout, "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"tools\":[{{\"name\":\"echo\",\"description\":\"Echo a value\",\"inputSchema\":{{\"type\":\"object\",\"properties\":{{\"text\":{{\"type\":\"string\"}}}},\"required\":[\"text\"],\"additionalProperties\":false}}}}]}}}}", request_id(&line)).unwrap();
        } else if line.contains("\"method\":\"tools/call\"") {
            let secret = std::env::var("MCP_TOKEN").unwrap();
            writeln!(stdout, "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{}\"}}],\"isError\":false}}}}", request_id(&line), secret).unwrap();
        }
        stdout.flush().unwrap();
    }
}
"#,
        )
        .unwrap();
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let status = std::process::Command::new(rustc)
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .status()
            .unwrap();
        assert!(status.success());
        executable
    }
}
