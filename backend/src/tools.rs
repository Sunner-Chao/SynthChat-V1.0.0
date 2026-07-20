use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use serde::Serialize;
use serde_json::{Map as JsonMap, Value as JsonValue};
use thiserror::Error;

use crate::profiles::{ProfileConfig, ProfileError, ProfileService, Versioned, WebProviderStatus};

mod clarify;
mod fuzzy;
mod runtime;
mod terminal;
mod v4a;
mod workspace;

pub(crate) use clarify::PreparedClarification;
pub(crate) use runtime::{
    PreparedToolCall, ToolExecutionContext, ToolExecutionError, ToolRegistry, ToolRisk,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolExecutionControlError {
    Cancelled,
    DeadlineExceeded,
}

#[derive(Clone)]
pub(crate) struct ToolExecutionControl {
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
}

impl ToolExecutionControl {
    pub(crate) fn new(deadline: Instant) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            deadline,
        }
    }

    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub(crate) fn check(&self) -> Result<(), ToolExecutionControlError> {
        if Instant::now() >= self.deadline {
            Err(ToolExecutionControlError::DeadlineExceeded)
        } else if self.cancelled.load(Ordering::Acquire) {
            Err(ToolExecutionControlError::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Toolset {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub enabled: bool,
    pub configured: bool,
    pub tools: Vec<String>,
}

#[derive(Debug, Error)]
pub enum ToolsetError {
    #[error("toolset not found")]
    NotFound,
    #[error(transparent)]
    Profile(#[from] ProfileError),
}

#[derive(Clone, Copy)]
struct ToolsetDescriptor {
    id: &'static str,
    display_name: &'static str,
    description: &'static str,
    tools: &'static [&'static str],
}

// Compatibility snapshot of CONFIGURABLE_TOOLSETS plus resolved tool names from
// NousResearch/hermes-agent 0.18.2 at commit
// 3f2a389c7e1f1729cad91ae63c26fb08c7753c74. This is Rust-owned metadata;
// the backend neither imports nor executes the upstream Python runtime.
const CATALOG: &[ToolsetDescriptor] = &[
    ToolsetDescriptor {
        id: "web",
        display_name: "Web Search & Scraping",
        description: "Web search and content extraction",
        tools: &["web_search", "web_extract"],
    },
    ToolsetDescriptor {
        id: "browser",
        display_name: "Browser Automation",
        description: "Navigate, inspect, and interact with web pages",
        tools: &[
            "browser_navigate",
            "browser_snapshot",
            "browser_click",
            "browser_download",
            "browser_type",
            "browser_scroll",
            "browser_back",
            "browser_press",
            "browser_get_images",
            "browser_vision",
            "browser_console",
            "browser_cdp",
            "browser_dialog",
            "web_search",
        ],
    },
    ToolsetDescriptor {
        id: "terminal",
        display_name: "Terminal & Processes",
        description: "Terminal commands and process management",
        tools: &["terminal", "process"],
    },
    ToolsetDescriptor {
        id: "file",
        display_name: "File Operations",
        description: "Read, write, patch, and search files",
        tools: &["read_file", "write_file", "patch", "search_files"],
    },
    ToolsetDescriptor {
        id: "code_execution",
        display_name: "Code Execution",
        description: "Execute code that can coordinate tool calls",
        tools: &["execute_code"],
    },
    ToolsetDescriptor {
        id: "vision",
        display_name: "Vision / Image Analysis",
        description: "Analyze images with a vision-capable model",
        tools: &["vision_analyze"],
    },
    ToolsetDescriptor {
        id: "video",
        display_name: "Video Analysis",
        description: "Analyze video with a video-capable model",
        tools: &["video_analyze"],
    },
    ToolsetDescriptor {
        id: "image_gen",
        display_name: "Image Generation",
        description: "Generate images from text prompts",
        tools: &["image_generate"],
    },
    ToolsetDescriptor {
        id: "video_gen",
        display_name: "Video Generation",
        description: "Generate, edit, and extend videos",
        tools: &["video_generate", "xai_video_edit", "xai_video_extend"],
    },
    ToolsetDescriptor {
        id: "x_search",
        display_name: "X (Twitter) Search",
        description: "Search X posts and threads",
        tools: &["x_search"],
    },
    ToolsetDescriptor {
        id: "tts",
        display_name: "Text-to-Speech",
        description: "Convert text to speech",
        tools: &["text_to_speech"],
    },
    ToolsetDescriptor {
        id: "skills",
        display_name: "Skills",
        description: "List, inspect, and manage skills",
        tools: &["skills_list", "skill_view", "skill_manage"],
    },
    ToolsetDescriptor {
        id: "todo",
        display_name: "Task Planning",
        description: "Plan and track multi-step work",
        tools: &["todo"],
    },
    ToolsetDescriptor {
        id: "memory",
        display_name: "Memory",
        description: "Use persistent memory across sessions",
        tools: &["memory"],
    },
    ToolsetDescriptor {
        id: "context_engine",
        display_name: "Context Engine",
        description: "Use tools exposed by the active context engine",
        tools: &[],
    },
    ToolsetDescriptor {
        id: "session_search",
        display_name: "Session Search",
        description: "Search and recall past conversations",
        tools: &["session_search"],
    },
    ToolsetDescriptor {
        id: "clarify",
        display_name: "Clarifying Questions",
        description: "Ask multiple-choice or open-ended questions",
        tools: &["clarify"],
    },
    ToolsetDescriptor {
        id: "delegation",
        display_name: "Task Delegation",
        description: "Delegate work to isolated subagents",
        tools: &["delegate_task"],
    },
    ToolsetDescriptor {
        id: "cronjob",
        display_name: "Cron Jobs",
        description: "Create and manage scheduled tasks",
        tools: &["cronjob"],
    },
    ToolsetDescriptor {
        id: "homeassistant",
        display_name: "Home Assistant",
        description: "Inspect and control smart-home entities",
        tools: &[
            "ha_list_entities",
            "ha_get_state",
            "ha_list_services",
            "ha_call_service",
        ],
    },
    ToolsetDescriptor {
        id: "spotify",
        display_name: "Spotify",
        description: "Control playback, search, playlists, and the music library",
        tools: &[
            "spotify_playback",
            "spotify_devices",
            "spotify_queue",
            "spotify_search",
            "spotify_playlists",
            "spotify_albums",
            "spotify_library",
        ],
    },
    ToolsetDescriptor {
        id: "discord",
        display_name: "Discord (read/participate)",
        description: "Read and participate in Discord conversations",
        tools: &["discord"],
    },
    ToolsetDescriptor {
        id: "discord_admin",
        display_name: "Discord Server Admin",
        description: "Manage Discord channels, roles, and pinned messages",
        tools: &["discord_admin"],
    },
    ToolsetDescriptor {
        id: "yuanbao",
        display_name: "Yuanbao",
        description: "Inspect groups, query members, and send messages",
        tools: &[
            "yb_query_group_info",
            "yb_query_group_members",
            "yb_send_dm",
            "yb_search_sticker",
            "yb_send_sticker",
        ],
    },
    ToolsetDescriptor {
        id: "computer_use",
        display_name: "Computer Use (macOS/Windows/Linux)",
        description: "Control a desktop through the computer-use driver",
        tools: &["computer_use"],
    },
];

pub fn list_toolsets(
    profiles: &ProfileService,
    profile_id: &str,
) -> Result<Versioned<Vec<Toolset>>, ToolsetError> {
    let config = profiles.get_config(profile_id)?;
    let web_configured = web_configured(profiles, profile_id);
    let code_execution_configured = crate::code_execution::is_available();
    let browser_configured = crate::browser::browser_binary_available();
    Ok(Versioned {
        value: CATALOG
            .iter()
            .map(|descriptor| {
                project(
                    descriptor,
                    &config.value,
                    web_configured,
                    code_execution_configured,
                    browser_configured,
                )
            })
            .collect(),
        etag: config.etag,
    })
}

pub fn update_toolset(
    profiles: &ProfileService,
    profile_id: &str,
    toolset_id: &str,
    enabled: bool,
    expected_etag: &str,
) -> Result<Versioned<Toolset>, ToolsetError> {
    let descriptor = descriptor(toolset_id).ok_or(ToolsetError::NotFound)?;
    let current = profiles.get_config(profile_id)?;
    let web_configured = web_configured(profiles, profile_id);
    let code_execution_configured = crate::code_execution::is_available();
    let browser_configured = crate::browser::browser_binary_available();
    let current_toolset = project(
        descriptor,
        &current.value,
        web_configured,
        code_execution_configured,
        browser_configured,
    );
    if current_toolset.enabled == enabled {
        let verified = profiles.update_config(
            profile_id,
            expected_etag,
            &JsonValue::Object(JsonMap::new()),
        )?;
        return Ok(Versioned {
            value: project(
                descriptor,
                &verified.value,
                web_configured,
                code_execution_configured,
                browser_configured,
            ),
            etag: verified.etag,
        });
    }

    let mut toolsets = JsonMap::new();
    toolsets.insert(toolset_id.to_owned(), JsonValue::Bool(enabled));
    let mut patch = JsonMap::new();
    patch.insert("toolsets".to_owned(), JsonValue::Object(toolsets));

    let config = profiles.update_config(profile_id, expected_etag, &JsonValue::Object(patch))?;
    Ok(Versioned {
        value: project(
            descriptor,
            &config.value,
            web_configured,
            code_execution_configured,
            browser_configured,
        ),
        etag: config.etag,
    })
}

fn descriptor(id: &str) -> Option<&'static ToolsetDescriptor> {
    CATALOG.iter().find(|descriptor| descriptor.id == id)
}

pub(crate) fn catalog_contains_tool(toolset_id: &str, tool_name: &str) -> bool {
    descriptor(toolset_id).is_some_and(|descriptor| descriptor.tools.contains(&tool_name))
}

fn project(
    descriptor: &ToolsetDescriptor,
    config: &ProfileConfig,
    web_configured: bool,
    code_execution_configured: bool,
    browser_configured: bool,
) -> Toolset {
    Toolset {
        id: descriptor.id.to_owned(),
        display_name: descriptor.display_name.to_owned(),
        description: descriptor.description.to_owned(),
        enabled: config.toolsets.get(descriptor.id).copied().unwrap_or(false),
        configured: matches!(descriptor.id, "session_search" | "memory")
            || descriptor.id == "web" && web_configured
            || descriptor.id == "code_execution" && code_execution_configured
            || descriptor.id == "browser" && browser_configured,
        tools: descriptor
            .tools
            .iter()
            .map(|tool| (*tool).to_owned())
            .collect(),
    }
}

fn web_configured(profiles: &ProfileService, profile_id: &str) -> bool {
    profiles
        .get_web_config(profile_id)
        .map(|config| {
            config.value.effective_search.status == WebProviderStatus::Ready
                || config.value.effective_extract.status == WebProviderStatus::Ready
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs, sync::Arc};

    use tempfile::TempDir;

    use super::*;

    struct Fixture {
        home: TempDir,
        profiles: ProfileService,
    }

    impl Fixture {
        fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            let profiles = ProfileService::without_credential_store(home.path().to_owned());
            Self { home, profiles }
        }
    }

    #[test]
    fn locked_management_catalog_has_25_unique_entries() {
        let ids: Vec<_> = CATALOG.iter().map(|descriptor| descriptor.id).collect();
        let unique: BTreeSet<_> = ids.iter().copied().collect();

        assert_eq!(ids.len(), 25);
        assert_eq!(unique.len(), ids.len());
        assert_eq!(ids.first(), Some(&"web"));
        assert_eq!(ids.last(), Some(&"computer_use"));
        assert!(ids.contains(&"context_engine"));
        assert!(ids.contains(&"video_gen"));
    }

    #[test]
    fn list_is_a_single_versioned_profile_config_projection() {
        let fixture = Fixture::new();
        let initial = fixture.profiles.get_config("default").unwrap();
        let list = list_toolsets(&fixture.profiles, "default").unwrap();

        assert_eq!(list.etag, initial.etag);
        assert_eq!(list.value.len(), 25);
        assert!(list.value.iter().all(|toolset| !toolset.enabled));
        let code_execution = list
            .value
            .iter()
            .find(|toolset| toolset.id == "code_execution")
            .unwrap();
        assert_eq!(
            code_execution.configured,
            crate::code_execution::is_available()
        );
        assert!(
            list.value
                .iter()
                .find(|toolset| toolset.id == "memory")
                .unwrap()
                .configured
        );
        assert!(
            list.value
                .iter()
                .find(|toolset| toolset.id == "session_search")
                .unwrap()
                .configured
        );
        assert!(
            list.value
                .iter()
                .filter(|toolset| toolset.configured)
                .all(|toolset| matches!(
                    toolset.id.as_str(),
                    "browser" | "code_execution" | "memory" | "session_search"
                ))
        );
        assert_eq!(
            list.value
                .iter()
                .find(|toolset| toolset.id == "file")
                .unwrap()
                .tools,
            ["read_file", "write_file", "patch", "search_files"]
        );
    }

    #[test]
    fn code_execution_projection_requires_an_available_interpreter() {
        let fixture = Fixture::new();
        let config = fixture.profiles.get_config("default").unwrap();
        let descriptor = descriptor("code_execution").unwrap();

        assert!(!project(descriptor, &config.value, false, false, false).configured);
        assert!(project(descriptor, &config.value, false, true, false).configured);
    }

    #[test]
    fn web_toolset_is_configured_only_when_a_web_capability_is_ready() {
        let home = tempfile::tempdir().unwrap();
        let store: Arc<keyring_core::CredentialStore> = keyring_core::mock::Store::new().unwrap();
        let profiles = ProfileService::with_credential_store(home.path().to_owned(), store);
        let missing_secret = list_toolsets(&profiles, "default").unwrap();
        assert!(
            !missing_secret
                .value
                .iter()
                .find(|toolset| toolset.id == "web")
                .unwrap()
                .configured
        );

        profiles
            .put_secret(
                "default",
                "TAVILY_API_KEY",
                &secrecy::SecretString::from("tvly-toolset-test-secret".to_owned()),
            )
            .unwrap();
        let ready = list_toolsets(&profiles, "default").unwrap();
        assert!(
            ready
                .value
                .iter()
                .find(|toolset| toolset.id == "web")
                .unwrap()
                .configured
        );
    }

    #[test]
    fn update_reuses_config_revision_and_preserves_unknown_yaml() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.home.path()).unwrap();
        fs::write(
            fixture.home.path().join("config.yaml"),
            "unknown:\n  nested: 42\nplatform_toolsets:\n  cli: []\n",
        )
        .unwrap();
        let initial = list_toolsets(&fixture.profiles, "default").unwrap();
        let before = fs::read(fixture.home.path().join("config.yaml")).unwrap();

        let initial_no_op = update_toolset(
            &fixture.profiles,
            "default",
            "terminal",
            false,
            &initial.etag,
        )
        .unwrap();
        assert_eq!(initial_no_op.etag, initial.etag);
        assert_eq!(
            fs::read(fixture.home.path().join("config.yaml")).unwrap(),
            before
        );

        let enabled = update_toolset(
            &fixture.profiles,
            "default",
            "terminal",
            true,
            &initial.etag,
        )
        .unwrap();
        assert!(enabled.value.enabled);
        assert_ne!(enabled.etag, initial.etag);

        let no_op = update_toolset(
            &fixture.profiles,
            "default",
            "terminal",
            true,
            &enabled.etag,
        )
        .unwrap();
        assert_eq!(no_op.etag, enabled.etag);
        assert!(no_op.value.enabled);

        assert!(matches!(
            update_toolset(
                &fixture.profiles,
                "default",
                "terminal",
                false,
                &initial.etag,
            ),
            Err(ToolsetError::Profile(ProfileError::RevisionConflict { .. }))
        ));

        let disabled = update_toolset(
            &fixture.profiles,
            "default",
            "terminal",
            false,
            &enabled.etag,
        )
        .unwrap();
        assert!(!disabled.value.enabled);
        assert_ne!(disabled.etag, enabled.etag);
        let persisted = fs::read_to_string(fixture.home.path().join("config.yaml")).unwrap();
        assert!(persisted.contains("unknown:"));
        assert!(persisted.contains("nested: 42"));
        assert!(persisted.contains("disabled_toolsets:"));
        assert!(persisted.contains("terminal"));
    }

    #[test]
    fn absent_false_is_noop_but_stale_same_state_still_conflicts() {
        let fixture = Fixture::new();
        let initial = list_toolsets(&fixture.profiles, "default").unwrap();

        let no_op = update_toolset(
            &fixture.profiles,
            "default",
            "terminal",
            false,
            &initial.etag,
        )
        .unwrap();
        assert_eq!(no_op.etag, initial.etag);
        assert!(!fixture.home.path().join("config.yaml").exists());

        let changed =
            update_toolset(&fixture.profiles, "default", "browser", true, &initial.etag).unwrap();
        assert!(matches!(
            update_toolset(
                &fixture.profiles,
                "default",
                "terminal",
                false,
                &initial.etag,
            ),
            Err(ToolsetError::Profile(ProfileError::RevisionConflict {
                current_etag
            })) if current_etag == changed.etag
        ));
    }

    #[test]
    fn unknown_toolset_never_mutates_profile_config() {
        let fixture = Fixture::new();
        let initial = fixture.profiles.get_config("default").unwrap();

        assert!(matches!(
            update_toolset(
                &fixture.profiles,
                "default",
                "not_registered",
                true,
                &initial.etag,
            ),
            Err(ToolsetError::NotFound)
        ));
        assert_eq!(fixture.profiles.get_config("default").unwrap(), initial);
    }
}
