use std::{
    collections::HashSet,
    env,
    io::{self, BufRead},
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

use axum::http::{HeaderValue, Uri};
use thiserror::Error;

use crate::{
    api::AppConfig,
    profiles::ProfileService,
    skills::{SkillRegistryRuntimeConfig, SkillRegistryRuntimeConfigError},
    web::{WebRuntimeConfig, WebRuntimeConfigError},
};

const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8642";
const DEFAULT_TAURI_ORIGINS: [&str; 3] = [
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
];
const MIN_TOKEN_BYTES: usize = 32;
const MAX_TOKEN_BYTES: usize = 128;

pub struct RuntimeConfig {
    bind_addr: SocketAddr,
    desktop_token: String,
    allowed_origins: Vec<HeaderValue>,
    watch_parent_stdin: bool,
    hermes_home: PathBuf,
    skill_registry: SkillRegistryRuntimeConfig,
    web: WebRuntimeConfig,
}

impl RuntimeConfig {
    pub fn from_env_or_stdin() -> Result<Self, ConfigError> {
        let bind_addr = match env::var("SYNTHCHAT_BACKEND_ADDR") {
            Ok(value) => parse_bind_addr(&value)?,
            Err(env::VarError::NotPresent) => parse_bind_addr(DEFAULT_BIND_ADDR)?,
            Err(env::VarError::NotUnicode(_)) => {
                return Err(ConfigError::NonUnicodeEnvironment("SYNTHCHAT_BACKEND_ADDR"));
            }
        };

        let (desktop_token, watch_parent_stdin) = match env::var("SYNTHCHAT_DESKTOP_TOKEN") {
            Ok(value) => (value, false),
            Err(env::VarError::NotPresent) => (read_desktop_token(io::stdin().lock())?, true),
            Err(env::VarError::NotUnicode(_)) => {
                return Err(ConfigError::NonUnicodeEnvironment(
                    "SYNTHCHAT_DESKTOP_TOKEN",
                ));
            }
        };
        validate_token(&desktop_token)?;

        let extra_origins = match env::var("SYNTHCHAT_ALLOWED_ORIGINS") {
            Ok(value) => value,
            Err(env::VarError::NotPresent) => String::new(),
            Err(env::VarError::NotUnicode(_)) => {
                return Err(ConfigError::NonUnicodeEnvironment(
                    "SYNTHCHAT_ALLOWED_ORIGINS",
                ));
            }
        };
        let allowed_origins = parse_allowed_origins(&extra_origins)?;
        let hermes_home = resolve_hermes_home()?;
        let skill_registry = SkillRegistryRuntimeConfig::from_env()?;
        let web = WebRuntimeConfig::from_env()?;

        Ok(Self {
            bind_addr,
            desktop_token,
            allowed_origins,
            watch_parent_stdin,
            hermes_home,
            skill_registry,
            web,
        })
    }

    pub fn bind_addr(&self) -> SocketAddr {
        self.bind_addr
    }

    pub fn watch_parent_stdin(&self) -> bool {
        self.watch_parent_stdin
    }

    pub fn into_app_config(self) -> AppConfig {
        let profiles = ProfileService::with_system_store(self.hermes_home);
        AppConfig::new(self.desktop_token, self.allowed_origins, profiles)
            .with_skill_registry_runtime_config(self.skill_registry)
            .with_web_runtime_config(self.web)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("desktop token is required through SYNTHCHAT_DESKTOP_TOKEN or stdin")]
    MissingDesktopToken,
    #[error("failed to read the desktop token from stdin")]
    DesktopTokenReadFailed,
    #[error("{0} must contain valid Unicode")]
    NonUnicodeEnvironment(&'static str),
    #[error("SYNTHCHAT_BACKEND_ADDR must be an IP socket address")]
    InvalidBindAddress,
    #[error("SYNTHCHAT_BACKEND_ADDR must use a loopback address")]
    NonLoopbackBindAddress,
    #[error("SYNTHCHAT_DESKTOP_TOKEN must contain at least 32 bytes")]
    TokenTooShort,
    #[error("SYNTHCHAT_DESKTOP_TOKEN must contain at most 128 bytes")]
    TokenTooLong,
    #[error("SYNTHCHAT_DESKTOP_TOKEN must contain only visible ASCII characters")]
    InvalidTokenCharacters,
    #[error("invalid CORS origin: {0}")]
    InvalidOrigin(String),
    #[error("HERMES_HOME must not be empty")]
    EmptyHermesHome,
    #[error("the user home directory is unavailable; set HERMES_HOME explicitly")]
    HomeDirectoryUnavailable,
    #[error("the current directory is unavailable")]
    CurrentDirectoryUnavailable,
    #[error(transparent)]
    SkillRegistryRuntime(#[from] SkillRegistryRuntimeConfigError),
    #[error(transparent)]
    WebRuntime(#[from] WebRuntimeConfigError),
}

fn parse_bind_addr(value: &str) -> Result<SocketAddr, ConfigError> {
    let addr = value
        .parse::<SocketAddr>()
        .map_err(|_| ConfigError::InvalidBindAddress)?;

    if !is_loopback(addr.ip()) {
        return Err(ConfigError::NonLoopbackBindAddress);
    }
    Ok(addr)
}

fn is_loopback(ip: IpAddr) -> bool {
    ip.is_loopback()
}

fn validate_token(token: &str) -> Result<(), ConfigError> {
    if token.len() < MIN_TOKEN_BYTES {
        return Err(ConfigError::TokenTooShort);
    }
    if token.len() > MAX_TOKEN_BYTES {
        return Err(ConfigError::TokenTooLong);
    }
    if !token.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        return Err(ConfigError::InvalidTokenCharacters);
    }
    Ok(())
}

fn read_desktop_token(reader: impl BufRead) -> Result<String, ConfigError> {
    let mut token = String::new();
    let mut limited = reader.take((MAX_TOKEN_BYTES + 3) as u64);
    let bytes_read = limited
        .read_line(&mut token)
        .map_err(|_| ConfigError::DesktopTokenReadFailed)?;
    if bytes_read == 0 {
        return Err(ConfigError::MissingDesktopToken);
    }

    while token.ends_with(['\r', '\n']) {
        token.pop();
    }
    validate_token(&token)?;
    Ok(token)
}

fn parse_allowed_origins(extra_origins: &str) -> Result<Vec<HeaderValue>, ConfigError> {
    let candidates = DEFAULT_TAURI_ORIGINS
        .into_iter()
        .chain(extra_origins.split(',').map(str::trim))
        .filter(|origin| !origin.is_empty());
    let mut seen = HashSet::new();
    let mut result = Vec::new();

    for origin in candidates {
        validate_origin(origin)?;
        if seen.insert(origin.to_owned()) {
            let header = HeaderValue::from_str(origin)
                .map_err(|_| ConfigError::InvalidOrigin(origin.to_owned()))?;
            result.push(header);
        }
    }

    Ok(result)
}

fn resolve_hermes_home() -> Result<PathBuf, ConfigError> {
    let configured = match env::var("HERMES_HOME") {
        Ok(value) => {
            if value.trim().is_empty() {
                return Err(ConfigError::EmptyHermesHome);
            }
            PathBuf::from(value)
        }
        Err(env::VarError::NotPresent) => dirs::home_dir()
            .ok_or(ConfigError::HomeDirectoryUnavailable)?
            .join(".hermes"),
        Err(env::VarError::NotUnicode(_)) => {
            return Err(ConfigError::NonUnicodeEnvironment("HERMES_HOME"));
        }
    };
    if configured.is_absolute() {
        Ok(configured)
    } else {
        env::current_dir()
            .map(|current| current.join(configured))
            .map_err(|_| ConfigError::CurrentDirectoryUnavailable)
    }
}

fn validate_origin(origin: &str) -> Result<(), ConfigError> {
    if origin == "*" {
        return Err(ConfigError::InvalidOrigin(origin.to_owned()));
    }

    let uri = origin
        .parse::<Uri>()
        .map_err(|_| ConfigError::InvalidOrigin(origin.to_owned()))?;
    let Some(scheme) = uri.scheme_str() else {
        return Err(ConfigError::InvalidOrigin(origin.to_owned()));
    };
    if !matches!(scheme, "http" | "https" | "tauri") || uri.authority().is_none() {
        return Err(ConfigError::InvalidOrigin(origin.to_owned()));
    }

    let Some((_, authority)) = origin.split_once("://") else {
        return Err(ConfigError::InvalidOrigin(origin.to_owned()));
    };
    if authority.is_empty() || authority.contains(['/', '?', '#', '@']) || uri.query().is_some() {
        return Err(ConfigError::InvalidOrigin(origin.to_owned()));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_address_must_be_loopback() {
        assert_eq!(
            parse_bind_addr("0.0.0.0:8642"),
            Err(ConfigError::NonLoopbackBindAddress)
        );
        assert_eq!(
            parse_bind_addr("127.0.0.1:8642").unwrap(),
            "127.0.0.1:8642".parse().unwrap()
        );
        assert_eq!(
            parse_bind_addr("[::1]:8642").unwrap(),
            "[::1]:8642".parse().unwrap()
        );
    }

    #[test]
    fn bind_address_allows_the_os_to_assign_a_loopback_port() {
        assert_eq!(
            parse_bind_addr("127.0.0.1:0").unwrap(),
            "127.0.0.1:0".parse().unwrap()
        );
        assert_eq!(
            parse_bind_addr("[::1]:0").unwrap(),
            "[::1]:0".parse().unwrap()
        );
    }

    #[test]
    fn token_must_be_suitable_for_an_http_bearer_header() {
        assert_eq!(validate_token("short"), Err(ConfigError::TokenTooShort));
        assert_eq!(
            validate_token("0123456789012345678901234567890 "),
            Err(ConfigError::InvalidTokenCharacters)
        );
        assert!(validate_token("01234567890123456789012345678901").is_ok());
        assert_eq!(
            validate_token(&"x".repeat(MAX_TOKEN_BYTES + 1)),
            Err(ConfigError::TokenTooLong)
        );
    }

    #[test]
    fn desktop_token_can_be_read_from_a_bounded_stdin_line() {
        let token = "01234567890123456789012345678901";
        assert_eq!(
            read_desktop_token(format!("{token}\r\n").as_bytes()).unwrap(),
            token
        );
        assert_eq!(
            read_desktop_token("\n".as_bytes()),
            Err(ConfigError::TokenTooShort)
        );
        assert_eq!(
            read_desktop_token("x".repeat(MAX_TOKEN_BYTES + 1).as_bytes()),
            Err(ConfigError::TokenTooLong)
        );
    }

    #[test]
    fn origins_are_explicit_and_deduplicated() {
        let origins =
            parse_allowed_origins("http://localhost:1420, tauri://localhost,http://localhost:1420")
                .unwrap();
        let values: Vec<_> = origins
            .iter()
            .map(|origin| origin.to_str().unwrap())
            .collect();

        assert_eq!(values.len(), 4);
        assert!(values.contains(&"tauri://localhost"));
        assert!(values.contains(&"http://localhost:1420"));
        assert_eq!(
            parse_allowed_origins("*"),
            Err(ConfigError::InvalidOrigin("*".to_owned()))
        );
        assert_eq!(
            parse_allowed_origins("http://localhost:1420/path"),
            Err(ConfigError::InvalidOrigin(
                "http://localhost:1420/path".to_owned()
            ))
        );
        assert_eq!(
            parse_allowed_origins("ftp://localhost:1420"),
            Err(ConfigError::InvalidOrigin(
                "ftp://localhost:1420".to_owned()
            ))
        );
    }
}
