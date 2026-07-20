mod openai_compatible;

use async_trait::async_trait;
use secrecy::SecretString;
use serde_json::Value as JsonValue;
use std::ops::Deref;
use thiserror::Error;
use tokio::sync::{mpsc, watch};
use url::Url;

pub use openai_compatible::OpenAiCompatibleProvider;

/// A normalized chat message passed to a model provider.
#[derive(Clone, Debug, PartialEq)]
pub enum ProviderMessage {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        content: Option<String>,
        tool_calls: Vec<ProviderToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
    #[doc(hidden)]
    Unsupported {
        role: String,
        content: String,
    },
}

impl ProviderMessage {
    /// Compatibility constructor for text-only session history.
    ///
    /// Structured tool results must use [`Self::tool`]. Unknown roles are
    /// retained but rejected by the provider request validator.
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        let role = role.into();
        let content = content.into();
        match role.as_str() {
            "system" => Self::System { content },
            "user" => Self::User { content },
            "assistant" => Self::Assistant {
                content: Some(content),
                tool_calls: Vec::new(),
            },
            _ => Self::Unsupported { role, content },
        }
    }

    pub fn assistant(content: Option<String>, tool_calls: Vec<ProviderToolCall>) -> Self {
        Self::Assistant {
            content,
            tool_calls,
        }
    }

    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::Tool {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: JsonValue,
    pub strict: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderToolCall {
    pub id: String,
    pub name: String,
    pub arguments_json: String,
}

/// Everything required to start one provider request.
///
/// This type intentionally does not implement `Debug` or `Serialize`: the
/// credential must never enter structured logs or persisted run snapshots.
pub struct ProviderRequest {
    pub provider_id: String,
    pub model: String,
    pub base_url: Url,
    pub url: Url,
    pub secret: Option<SecretString>,
    pub reasoning_effort: Option<String>,
    pub messages: Vec<ProviderMessage>,
    pub tools: Vec<ProviderToolDefinition>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProviderUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cost: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderTurn {
    pub usage: ProviderUsage,
    pub finish: ProviderFinish,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ProviderFinish {
    Stop,
    ToolCalls(Vec<ProviderToolCall>),
    Length,
    ContentFilter,
}

impl Deref for ProviderTurn {
    type Target = ProviderUsage;

    fn deref(&self) -> &Self::Target {
        &self.usage
    }
}

impl PartialEq<ProviderUsage> for ProviderTurn {
    fn eq(&self, other: &ProviderUsage) -> bool {
        self.usage == *other
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ProviderEvent {
    TextDelta(String),
    ReasoningDelta(String),
    Usage(ProviderUsage),
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ProviderError {
    #[error("provider request was cancelled")]
    Cancelled,
    #[error("provider is unavailable")]
    Unavailable,
    #[error("provider request timed out")]
    Timeout,
    #[error("provider returned an invalid response")]
    InvalidResponse,
}

#[async_trait]
pub trait ProviderTransport: Send + Sync {
    async fn stream_chat(
        &self,
        request: ProviderRequest,
        events: mpsc::Sender<ProviderEvent>,
        cancelled: watch::Receiver<bool>,
    ) -> Result<ProviderTurn, ProviderError>;
}
