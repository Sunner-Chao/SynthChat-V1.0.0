use serde::Serialize;

/// Structured error payload sent to the frontend.
///
/// Using a typed envelope instead of a plain string lets the frontend
/// differentiate error domains and show domain-appropriate UI (e.g. an
/// "API key invalid" banner vs. a generic network error toast).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorPayload {
    /// Machine-readable dot-separated code, e.g. `"llm.rate_limit"`.
    pub code: &'static str,
    /// Human-readable description (safe to display to the user).
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    // -----------------------------------------------------------------------
    // Input / request errors
    // -----------------------------------------------------------------------
    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("validation error: {0}")]
    Validation(String),

    // -----------------------------------------------------------------------
    // LLM errors
    // -----------------------------------------------------------------------
    #[error("llm error: {0}")]
    Llm(String),

    #[error("llm rate limit: {0}")]
    LlmRateLimit(String),

    #[error("llm authentication failed: {0}")]
    LlmAuth(String),

    // -----------------------------------------------------------------------
    // MCP (Model Context Protocol) errors
    // -----------------------------------------------------------------------
    #[error("mcp server error: {0}")]
    Mcp(String),

    #[error("mcp transport error: {0}")]
    McpTransport(String),

    #[error("mcp tool call failed: {tool} — {reason}")]
    McpToolCall { tool: String, reason: String },

    // -----------------------------------------------------------------------
    // Agent errors
    // -----------------------------------------------------------------------
    #[error("agent error: {0}")]
    Agent(String),

    #[error("agent run cancelled: {0}")]
    AgentCancelled(String),

    // -----------------------------------------------------------------------
    // Authentication / credential errors
    // -----------------------------------------------------------------------
    #[error("authentication error: {0}")]
    Auth(String),

    #[error("oauth error: {0}")]
    OAuth(String),

    // -----------------------------------------------------------------------
    // Network / HTTP errors
    // -----------------------------------------------------------------------
    #[error("network error: {0}")]
    Network(String),

    #[error("http error {status}: {message}")]
    Http { status: u16, message: String },

    // -----------------------------------------------------------------------
    // Cryptographic errors
    // -----------------------------------------------------------------------
    #[error("crypto error: {0}")]
    Crypto(String),

    // -----------------------------------------------------------------------
    // Process / subprocess errors
    // -----------------------------------------------------------------------
    #[error("process error: {0}")]
    Process(String),

    // -----------------------------------------------------------------------
    // Skill errors
    // -----------------------------------------------------------------------
    #[error("skill error: {0}")]
    Skill(String),

    // -----------------------------------------------------------------------
    // Storage / persistence errors (transparent conversions)
    // -----------------------------------------------------------------------
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl AppError {
    /// Returns a stable machine-readable error code for the frontend.
    fn code(&self) -> &'static str {
        match self {
            AppError::BadRequest(_) => "request.bad_request",
            AppError::NotFound(_) => "request.not_found",
            AppError::Validation(_) => "request.validation",
            AppError::Llm(_) => "llm.error",
            AppError::LlmRateLimit(_) => "llm.rate_limit",
            AppError::LlmAuth(_) => "llm.auth",
            AppError::Mcp(_) => "mcp.error",
            AppError::McpTransport(_) => "mcp.transport",
            AppError::McpToolCall { .. } => "mcp.tool_call",
            AppError::Agent(_) => "agent.error",
            AppError::AgentCancelled(_) => "agent.cancelled",
            AppError::Auth(_) => "auth.error",
            AppError::OAuth(_) => "auth.oauth",
            AppError::Network(_) => "network.error",
            AppError::Http { .. } => "network.http",
            AppError::Crypto(_) => "crypto.error",
            AppError::Process(_) => "process.error",
            AppError::Skill(_) => "skill.error",
            AppError::Io(_) => "io.error",
            AppError::Json(_) => "io.json",
        }
    }
}

impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        ErrorPayload {
            code: self.code(),
            message: self.to_string(),
        }
        .serialize(serializer)
    }
}

pub type AppResult<T> = Result<T, AppError>;
