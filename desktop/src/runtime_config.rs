use std::{env, time::Duration};

use serde::Serialize;

const STARTUP_TIMEOUT: MillisecondSetting =
    MillisecondSetting::new("SYNTHCHAT_DESKTOP_STARTUP_TIMEOUT_MS", 8_000, 100, 120_000);
const PROBE_TIMEOUT: MillisecondSetting =
    MillisecondSetting::new("SYNTHCHAT_DESKTOP_PROBE_TIMEOUT_MS", 250, 10, 10_000);
const MONITOR_INTERVAL: MillisecondSetting =
    MillisecondSetting::new("SYNTHCHAT_DESKTOP_MONITOR_INTERVAL_MS", 500, 10, 60_000);
const PROCESS_POLL_INTERVAL: MillisecondSetting =
    MillisecondSetting::new("SYNTHCHAT_DESKTOP_PROCESS_POLL_INTERVAL_MS", 20, 1, 1_000);
const SHUTDOWN_GRACE_TIMEOUT: MillisecondSetting = MillisecondSetting::new(
    "SYNTHCHAT_DESKTOP_SHUTDOWN_GRACE_TIMEOUT_MS",
    2_000,
    10,
    60_000,
);
const TERMINATION_TIMEOUT: MillisecondSetting = MillisecondSetting::new(
    "SYNTHCHAT_DESKTOP_TERMINATION_TIMEOUT_MS",
    1_000,
    10,
    30_000,
);
const RESTART_BACKOFF_INITIAL: MillisecondSetting = MillisecondSetting::new(
    "SYNTHCHAT_DESKTOP_RESTART_BACKOFF_INITIAL_MS",
    250,
    10,
    60_000,
);
const RESTART_BACKOFF_MAX: MillisecondSetting = MillisecondSetting::new(
    "SYNTHCHAT_DESKTOP_RESTART_BACKOFF_MAX_MS",
    8_000,
    10,
    300_000,
);
const STABLE_WINDOW: MillisecondSetting =
    MillisecondSetting::new("SYNTHCHAT_DESKTOP_STABLE_WINDOW_MS", 30_000, 100, 3_600_000);
const DIAGNOSTIC_TIMEOUT: MillisecondSetting =
    MillisecondSetting::new("SYNTHCHAT_DESKTOP_DIAGNOSTIC_TIMEOUT_MS", 250, 10, 5_000);
const STDERR_MAX_BYTES: IntegerSetting =
    IntegerSetting::new("SYNTHCHAT_DESKTOP_STDERR_MAX_BYTES", 4_096, 256, 65_536);

const FRONTEND_BACKEND_HEALTH_TIMEOUT: IntegerSetting = IntegerSetting::new(
    "SYNTHCHAT_FRONTEND_BACKEND_HEALTH_TIMEOUT_MS",
    4_000,
    100,
    120_000,
);
const FRONTEND_BACKEND_STATUS_POLL_INTERVAL: IntegerSetting = IntegerSetting::new(
    "SYNTHCHAT_FRONTEND_BACKEND_STATUS_POLL_INTERVAL_MS",
    15_000,
    1_000,
    3_600_000,
);
const FRONTEND_CHAT_RECONNECT_INITIAL_DELAY: IntegerSetting = IntegerSetting::new(
    "SYNTHCHAT_FRONTEND_CHAT_RECONNECT_INITIAL_DELAY_MS",
    250,
    10,
    60_000,
);
const FRONTEND_CHAT_RECONNECT_MAX_ATTEMPTS: IntegerSetting = IntegerSetting::new(
    "SYNTHCHAT_FRONTEND_CHAT_RECONNECT_MAX_ATTEMPTS",
    30,
    0,
    10_000,
);
const FRONTEND_CHAT_RECONNECT_MAX_DELAY: IntegerSetting = IntegerSetting::new(
    "SYNTHCHAT_FRONTEND_CHAT_RECONNECT_MAX_DELAY_MS",
    8_000,
    10,
    300_000,
);
const FRONTEND_CHAT_RUN_STATUS_POLL_INTERVAL: IntegerSetting = IntegerSetting::new(
    "SYNTHCHAT_FRONTEND_CHAT_RUN_STATUS_POLL_INTERVAL_MS",
    2_000,
    500,
    60_000,
);
const FRONTEND_SKILL_OPERATION_MAX_POLLS: IntegerSetting =
    IntegerSetting::new("SYNTHCHAT_FRONTEND_SKILL_OPERATION_MAX_POLLS", 30, 1, 1_000);
const FRONTEND_SKILL_OPERATION_INITIAL_BACKOFF: IntegerSetting = IntegerSetting::new(
    "SYNTHCHAT_FRONTEND_SKILL_OPERATION_INITIAL_BACKOFF_MS",
    250,
    1,
    60_000,
);
const FRONTEND_SKILL_OPERATION_MAX_BACKOFF: IntegerSetting = IntegerSetting::new(
    "SYNTHCHAT_FRONTEND_SKILL_OPERATION_MAX_BACKOFF_MS",
    2_000,
    1,
    300_000,
);
const FRONTEND_PET_FRAME_URL: StringSetting =
    StringSetting::new("SYNTHCHAT_FRONTEND_PET_FRAME_URL", "pet/index.html", 2_048);
const FRONTEND_PET_MODEL_URL: StringSetting = StringSetting::new(
    "SYNTHCHAT_FRONTEND_PET_MODEL_URL",
    "pet/model/Hiyori/Hiyori.model3.json",
    2_048,
);
const FRONTEND_PET_STATUS_POLL_INTERVAL: IntegerSetting = IntegerSetting::new(
    "SYNTHCHAT_FRONTEND_PET_STATUS_POLL_INTERVAL_MS",
    5_000,
    1_000,
    3_600_000,
);

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FrontendRuntimeConfig {
    backend: FrontendBackendRuntimeConfig,
    chat: FrontendChatRuntimeConfig,
    skill_operations: FrontendSkillOperationRuntimeConfig,
    pet: FrontendPetRuntimeConfig,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct FrontendBackendRuntimeConfig {
    health_timeout_ms: usize,
    status_poll_interval_ms: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct FrontendChatRuntimeConfig {
    reconnect_initial_delay_ms: usize,
    reconnect_max_attempts: usize,
    reconnect_max_delay_ms: usize,
    run_status_poll_interval_ms: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct FrontendSkillOperationRuntimeConfig {
    max_polls: usize,
    initial_backoff_ms: usize,
    max_backoff_ms: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct FrontendPetRuntimeConfig {
    frame_url: String,
    model_url: String,
    status_poll_interval_ms: usize,
}

impl FrontendRuntimeConfig {
    pub(crate) fn from_env() -> Result<Self, String> {
        Self::from_lookup(|name| match env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(env::VarError::NotPresent) => Ok(None),
            Err(env::VarError::NotUnicode(_)) => Err(format!(
                "{name} must contain valid Unicode and does not expose its configured value"
            )),
        })
    }

    pub(crate) fn from_lookup(
        mut lookup: impl FnMut(&str) -> Result<Option<String>, String>,
    ) -> Result<Self, String> {
        let backend = FrontendBackendRuntimeConfig {
            health_timeout_ms: FRONTEND_BACKEND_HEALTH_TIMEOUT.read(&mut lookup)?,
            status_poll_interval_ms: FRONTEND_BACKEND_STATUS_POLL_INTERVAL.read(&mut lookup)?,
        };
        let chat = FrontendChatRuntimeConfig {
            reconnect_initial_delay_ms: FRONTEND_CHAT_RECONNECT_INITIAL_DELAY.read(&mut lookup)?,
            reconnect_max_attempts: FRONTEND_CHAT_RECONNECT_MAX_ATTEMPTS.read(&mut lookup)?,
            reconnect_max_delay_ms: FRONTEND_CHAT_RECONNECT_MAX_DELAY.read(&mut lookup)?,
            run_status_poll_interval_ms: FRONTEND_CHAT_RUN_STATUS_POLL_INTERVAL
                .read(&mut lookup)?,
        };
        if chat.reconnect_initial_delay_ms > chat.reconnect_max_delay_ms {
            return Err(format!(
                "{} must not exceed {}",
                FRONTEND_CHAT_RECONNECT_INITIAL_DELAY.name, FRONTEND_CHAT_RECONNECT_MAX_DELAY.name
            ));
        }

        let skill_operations = FrontendSkillOperationRuntimeConfig {
            max_polls: FRONTEND_SKILL_OPERATION_MAX_POLLS.read(&mut lookup)?,
            initial_backoff_ms: FRONTEND_SKILL_OPERATION_INITIAL_BACKOFF.read(&mut lookup)?,
            max_backoff_ms: FRONTEND_SKILL_OPERATION_MAX_BACKOFF.read(&mut lookup)?,
        };
        if skill_operations.initial_backoff_ms > skill_operations.max_backoff_ms {
            return Err(format!(
                "{} must not exceed {}",
                FRONTEND_SKILL_OPERATION_INITIAL_BACKOFF.name,
                FRONTEND_SKILL_OPERATION_MAX_BACKOFF.name
            ));
        }

        Ok(Self {
            backend,
            chat,
            skill_operations,
            pet: FrontendPetRuntimeConfig {
                frame_url: FRONTEND_PET_FRAME_URL.read(&mut lookup)?,
                model_url: FRONTEND_PET_MODEL_URL.read(&mut lookup)?,
                status_poll_interval_ms: FRONTEND_PET_STATUS_POLL_INTERVAL.read(&mut lookup)?,
            },
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DesktopRuntimeConfig {
    pub(crate) startup_timeout: Duration,
    pub(crate) probe_timeout: Duration,
    pub(crate) monitor_interval: Duration,
    pub(crate) process_poll_interval: Duration,
    pub(crate) shutdown_grace_timeout: Duration,
    pub(crate) termination_timeout: Duration,
    pub(crate) restart_backoff_initial: Duration,
    pub(crate) restart_backoff_max: Duration,
    pub(crate) stable_window: Duration,
    pub(crate) diagnostic_timeout: Duration,
    pub(crate) stderr_max_bytes: usize,
}

impl DesktopRuntimeConfig {
    pub(crate) fn from_env() -> Result<Self, String> {
        Self::from_lookup(|name| match env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(env::VarError::NotPresent) => Ok(None),
            Err(env::VarError::NotUnicode(_)) => Err(format!("{name} must contain valid Unicode")),
        })
    }

    pub(crate) fn from_lookup(
        mut lookup: impl FnMut(&str) -> Result<Option<String>, String>,
    ) -> Result<Self, String> {
        let config = Self {
            startup_timeout: STARTUP_TIMEOUT.read(&mut lookup)?,
            probe_timeout: PROBE_TIMEOUT.read(&mut lookup)?,
            monitor_interval: MONITOR_INTERVAL.read(&mut lookup)?,
            process_poll_interval: PROCESS_POLL_INTERVAL.read(&mut lookup)?,
            shutdown_grace_timeout: SHUTDOWN_GRACE_TIMEOUT.read(&mut lookup)?,
            termination_timeout: TERMINATION_TIMEOUT.read(&mut lookup)?,
            restart_backoff_initial: RESTART_BACKOFF_INITIAL.read(&mut lookup)?,
            restart_backoff_max: RESTART_BACKOFF_MAX.read(&mut lookup)?,
            stable_window: STABLE_WINDOW.read(&mut lookup)?,
            diagnostic_timeout: DIAGNOSTIC_TIMEOUT.read(&mut lookup)?,
            stderr_max_bytes: STDERR_MAX_BYTES.read(&mut lookup)?,
        };
        if config.restart_backoff_initial > config.restart_backoff_max {
            return Err(format!(
                "{} must not exceed {}",
                RESTART_BACKOFF_INITIAL.name, RESTART_BACKOFF_MAX.name
            ));
        }
        Ok(config)
    }
}

impl Default for DesktopRuntimeConfig {
    fn default() -> Self {
        Self {
            startup_timeout: Duration::from_millis(STARTUP_TIMEOUT.default_ms),
            probe_timeout: Duration::from_millis(PROBE_TIMEOUT.default_ms),
            monitor_interval: Duration::from_millis(MONITOR_INTERVAL.default_ms),
            process_poll_interval: Duration::from_millis(PROCESS_POLL_INTERVAL.default_ms),
            shutdown_grace_timeout: Duration::from_millis(SHUTDOWN_GRACE_TIMEOUT.default_ms),
            termination_timeout: Duration::from_millis(TERMINATION_TIMEOUT.default_ms),
            restart_backoff_initial: Duration::from_millis(RESTART_BACKOFF_INITIAL.default_ms),
            restart_backoff_max: Duration::from_millis(RESTART_BACKOFF_MAX.default_ms),
            stable_window: Duration::from_millis(STABLE_WINDOW.default_ms),
            diagnostic_timeout: Duration::from_millis(DIAGNOSTIC_TIMEOUT.default_ms),
            stderr_max_bytes: STDERR_MAX_BYTES.default,
        }
    }
}

#[derive(Clone, Copy)]
struct IntegerSetting {
    name: &'static str,
    default: usize,
    minimum: usize,
    maximum: usize,
}

impl IntegerSetting {
    const fn new(name: &'static str, default: usize, minimum: usize, maximum: usize) -> Self {
        Self {
            name,
            default,
            minimum,
            maximum,
        }
    }

    fn read(
        self,
        lookup: &mut impl FnMut(&str) -> Result<Option<String>, String>,
    ) -> Result<usize, String> {
        let Some(value) = lookup(self.name)? else {
            return Ok(self.default);
        };
        let parsed = value.parse::<usize>().map_err(|_| self.range_error())?;
        if !(self.minimum..=self.maximum).contains(&parsed) {
            return Err(self.range_error());
        }
        Ok(parsed)
    }

    fn range_error(self) -> String {
        format!(
            "{} must be an integer between {} and {}",
            self.name, self.minimum, self.maximum
        )
    }
}

#[derive(Clone, Copy)]
struct MillisecondSetting {
    name: &'static str,
    default_ms: u64,
    minimum_ms: u64,
    maximum_ms: u64,
}

impl MillisecondSetting {
    const fn new(name: &'static str, default_ms: u64, minimum_ms: u64, maximum_ms: u64) -> Self {
        Self {
            name,
            default_ms,
            minimum_ms,
            maximum_ms,
        }
    }

    fn read(
        self,
        lookup: &mut impl FnMut(&str) -> Result<Option<String>, String>,
    ) -> Result<Duration, String> {
        let Some(value) = lookup(self.name)? else {
            return Ok(Duration::from_millis(self.default_ms));
        };
        let milliseconds = value.parse::<u64>().map_err(|_| self.range_error())?;
        if !(self.minimum_ms..=self.maximum_ms).contains(&milliseconds) {
            return Err(self.range_error());
        }
        Ok(Duration::from_millis(milliseconds))
    }

    fn range_error(self) -> String {
        format!(
            "{} must be an integer number of milliseconds between {} and {}",
            self.name, self.minimum_ms, self.maximum_ms
        )
    }
}

#[derive(Clone, Copy)]
struct StringSetting {
    name: &'static str,
    default: &'static str,
    maximum_bytes: usize,
}

impl StringSetting {
    const fn new(name: &'static str, default: &'static str, maximum_bytes: usize) -> Self {
        Self {
            name,
            default,
            maximum_bytes,
        }
    }

    fn read(
        self,
        lookup: &mut impl FnMut(&str) -> Result<Option<String>, String>,
    ) -> Result<String, String> {
        let Some(value) = lookup(self.name)? else {
            return Ok(self.default.to_owned());
        };
        let value = value.trim();
        if value.is_empty() || value.len() > self.maximum_bytes {
            return Err(format!(
                "{} must be a non-empty string no longer than {} bytes",
                self.name, self.maximum_bytes
            ));
        }
        Ok(value.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn frontend_config(values: &[(&str, &str)]) -> Result<FrontendRuntimeConfig, String> {
        let values: HashMap<_, _> = values.iter().copied().collect();
        FrontendRuntimeConfig::from_lookup(|name| {
            Ok(values.get(name).map(|value| (*value).to_owned()))
        })
    }

    #[test]
    fn frontend_defaults_match_the_browser_contract() {
        let config = frontend_config(&[]).expect("frontend defaults should be valid");

        assert_eq!(config.backend.health_timeout_ms, 4_000);
        assert_eq!(config.backend.status_poll_interval_ms, 15_000);
        assert_eq!(config.chat.reconnect_initial_delay_ms, 250);
        assert_eq!(config.chat.reconnect_max_attempts, 30);
        assert_eq!(config.chat.reconnect_max_delay_ms, 8_000);
        assert_eq!(config.chat.run_status_poll_interval_ms, 2_000);
        assert_eq!(config.skill_operations.max_polls, 30);
        assert_eq!(config.skill_operations.initial_backoff_ms, 250);
        assert_eq!(config.skill_operations.max_backoff_ms, 2_000);
        assert_eq!(config.pet.frame_url, "pet/index.html");
        assert_eq!(config.pet.model_url, "pet/model/Hiyori/Hiyori.model3.json");
        assert_eq!(config.pet.status_poll_interval_ms, 5_000);
    }

    #[test]
    fn frontend_overrides_are_bounded_and_typed() {
        let config = frontend_config(&[
            ("SYNTHCHAT_FRONTEND_BACKEND_HEALTH_TIMEOUT_MS", "6500"),
            (
                "SYNTHCHAT_FRONTEND_BACKEND_STATUS_POLL_INTERVAL_MS",
                "20000",
            ),
            ("SYNTHCHAT_FRONTEND_CHAT_RECONNECT_INITIAL_DELAY_MS", "400"),
            ("SYNTHCHAT_FRONTEND_CHAT_RECONNECT_MAX_ATTEMPTS", "45"),
            ("SYNTHCHAT_FRONTEND_CHAT_RECONNECT_MAX_DELAY_MS", "12000"),
            (
                "SYNTHCHAT_FRONTEND_CHAT_RUN_STATUS_POLL_INTERVAL_MS",
                "3500",
            ),
            ("SYNTHCHAT_FRONTEND_SKILL_OPERATION_MAX_POLLS", "50"),
            (
                "SYNTHCHAT_FRONTEND_SKILL_OPERATION_INITIAL_BACKOFF_MS",
                "300",
            ),
            ("SYNTHCHAT_FRONTEND_SKILL_OPERATION_MAX_BACKOFF_MS", "3000"),
            ("SYNTHCHAT_FRONTEND_PET_FRAME_URL", "/pet/custom.html"),
            (
                "SYNTHCHAT_FRONTEND_PET_MODEL_URL",
                "/pet/custom.model3.json",
            ),
            ("SYNTHCHAT_FRONTEND_PET_STATUS_POLL_INTERVAL_MS", "8000"),
        ])
        .expect("bounded frontend overrides should be valid");

        assert_eq!(config.backend.health_timeout_ms, 6_500);
        assert_eq!(config.backend.status_poll_interval_ms, 20_000);
        assert_eq!(config.chat.reconnect_initial_delay_ms, 400);
        assert_eq!(config.chat.reconnect_max_attempts, 45);
        assert_eq!(config.chat.reconnect_max_delay_ms, 12_000);
        assert_eq!(config.chat.run_status_poll_interval_ms, 3_500);
        assert_eq!(config.skill_operations.max_polls, 50);
        assert_eq!(config.skill_operations.initial_backoff_ms, 300);
        assert_eq!(config.skill_operations.max_backoff_ms, 3_000);
        assert_eq!(config.pet.frame_url, "/pet/custom.html");
        assert_eq!(config.pet.model_url, "/pet/custom.model3.json");
        assert_eq!(config.pet.status_poll_interval_ms, 8_000);
    }

    #[test]
    fn frontend_rejects_invalid_ranges_and_backoff_order() {
        let invalid_timeout =
            frontend_config(&[("SYNTHCHAT_FRONTEND_BACKEND_HEALTH_TIMEOUT_MS", "99")])
                .expect_err("too-small timeouts must fail");
        assert!(invalid_timeout.contains("SYNTHCHAT_FRONTEND_BACKEND_HEALTH_TIMEOUT_MS"));

        let invalid_chat_order = frontend_config(&[
            ("SYNTHCHAT_FRONTEND_CHAT_RECONNECT_INITIAL_DELAY_MS", "2000"),
            ("SYNTHCHAT_FRONTEND_CHAT_RECONNECT_MAX_DELAY_MS", "1000"),
        ])
        .expect_err("inverted chat backoff must fail");
        assert!(invalid_chat_order.contains("RECONNECT_INITIAL_DELAY_MS"));
        assert!(invalid_chat_order.contains("RECONNECT_MAX_DELAY_MS"));

        let invalid_run_poll =
            frontend_config(&[("SYNTHCHAT_FRONTEND_CHAT_RUN_STATUS_POLL_INTERVAL_MS", "499")])
                .expect_err("too-fast Run status polling must fail");
        assert!(invalid_run_poll.contains("CHAT_RUN_STATUS_POLL_INTERVAL_MS"));

        let invalid_skill_order = frontend_config(&[
            (
                "SYNTHCHAT_FRONTEND_SKILL_OPERATION_INITIAL_BACKOFF_MS",
                "3000",
            ),
            ("SYNTHCHAT_FRONTEND_SKILL_OPERATION_MAX_BACKOFF_MS", "2000"),
        ])
        .expect_err("inverted skill-operation backoff must fail");
        assert!(invalid_skill_order.contains("INITIAL_BACKOFF_MS"));
        assert!(invalid_skill_order.contains("MAX_BACKOFF_MS"));
    }

    #[test]
    fn frontend_errors_never_echo_configured_values() {
        let configured_value = "not-a-number-secret-marker";
        let error = frontend_config(&[(
            "SYNTHCHAT_FRONTEND_BACKEND_HEALTH_TIMEOUT_MS",
            configured_value,
        )])
        .expect_err("malformed numbers must fail");

        assert!(error.contains("SYNTHCHAT_FRONTEND_BACKEND_HEALTH_TIMEOUT_MS"));
        assert!(!error.contains(configured_value));
    }
}
