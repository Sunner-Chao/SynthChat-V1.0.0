pub mod api;
pub(crate) mod browser;
mod code_execution;
pub mod compat;
pub mod config;
pub mod files;
pub mod mcp;
pub mod memory;
pub(crate) mod operations;
mod processes;
pub mod profiles;
pub mod providers;
pub mod runs;
pub mod sessions;
pub mod skills;
pub mod tools;
mod web;

pub use api::{AppConfig, AppShutdown, build_router, build_router_with_shutdown};
pub use config::RuntimeConfig;
pub use files::FileService;
pub use memory::MemoryService;
#[doc(hidden)]
pub use processes::{
    PROCESS_GUARDIAN_MODE_ARG, PROCESS_GUARDIAN_PROTOCOL_EXIT, PROCESS_GUARDIAN_RUNTIME_EXIT,
    ProcessGuardianError, ProcessGuardianLaunch, encode_process_guardian_launch,
    encode_process_guardian_stdin, encode_process_guardian_stdin_close, process_guardian_command,
    process_guardian_mode_requested, run_process_guardian_stdio,
};
pub use profiles::ProfileService;
pub use sessions::SessionService;
pub use skills::{
    SKILL_GITHUB_API_BASE_URL_ENV, SKILL_GITHUB_RAW_BASE_URL_ENV, SKILL_REGISTRY_INDEX_URL_ENV,
    SkillRegistryRuntimeConfig, SkillRegistryRuntimeConfigError,
};
pub use web::{TAVILY_BASE_URL_ENV, WebRuntimeConfig, WebRuntimeConfigError};
