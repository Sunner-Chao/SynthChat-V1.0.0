use std::{
    env,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

use thiserror::Error;
use url::{Host, Url};

pub const SKILL_REGISTRY_INDEX_URL_ENV: &str = "SYNTHCHAT_SKILL_REGISTRY_INDEX_URL";
pub const SKILL_GITHUB_API_BASE_URL_ENV: &str = "SYNTHCHAT_SKILL_GITHUB_API_BASE_URL";
pub const SKILL_GITHUB_RAW_BASE_URL_ENV: &str = "SYNTHCHAT_SKILL_GITHUB_RAW_BASE_URL";

const DEFAULT_REGISTRY_INDEX_URL: &str =
    "https://hermes-agent.nousresearch.com/docs/api/skills-index.json";
const DEFAULT_GITHUB_API_BASE_URL: &str = "https://api.github.com/";
const DEFAULT_GITHUB_RAW_BASE_URL: &str = "https://raw.githubusercontent.com/";
const MAX_ENDPOINT_URL_BYTES: usize = 2_048;
const MAX_ENDPOINT_PATH_BYTES: usize = 1_024;
const MAX_JOIN_SEGMENT_BYTES: usize = 2_048;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillRegistryRuntimeConfig {
    registry_index_url: Url,
    github_api_base_url: Url,
    github_raw_base_url: Url,
}

impl SkillRegistryRuntimeConfig {
    pub fn from_env() -> Result<Self, SkillRegistryRuntimeConfigError> {
        Self::from_env_values(
            env::var(SKILL_REGISTRY_INDEX_URL_ENV),
            env::var(SKILL_GITHUB_API_BASE_URL_ENV),
            env::var(SKILL_GITHUB_RAW_BASE_URL_ENV),
        )
    }

    pub(crate) fn registry_index_url(&self) -> &Url {
        &self.registry_index_url
    }

    #[cfg(test)]
    pub(crate) fn github_api_base_url(&self) -> &Url {
        &self.github_api_base_url
    }

    #[cfg(test)]
    pub(crate) fn github_raw_base_url(&self) -> &Url {
        &self.github_raw_base_url
    }

    pub(crate) fn github_api_url(
        &self,
        segments: &[&str],
    ) -> Result<Url, SkillRegistryRuntimeConfigError> {
        append_path_segments(&self.github_api_base_url, segments)
    }

    pub(crate) fn github_raw_url(
        &self,
        segments: &[&str],
    ) -> Result<Url, SkillRegistryRuntimeConfigError> {
        append_path_segments(&self.github_raw_base_url, segments)
    }

    #[cfg(test)]
    pub(crate) fn from_urls_for_tests(
        registry_index_url: impl AsRef<str>,
        github_api_base_url: impl AsRef<str>,
        github_raw_base_url: impl AsRef<str>,
    ) -> Result<Self, SkillRegistryRuntimeConfigError> {
        Self::from_urls(
            registry_index_url.as_ref(),
            github_api_base_url.as_ref(),
            github_raw_base_url.as_ref(),
        )
    }

    fn from_env_values(
        registry_index_url: Result<String, env::VarError>,
        github_api_base_url: Result<String, env::VarError>,
        github_raw_base_url: Result<String, env::VarError>,
    ) -> Result<Self, SkillRegistryRuntimeConfigError> {
        let registry_index_url = environment_url(
            registry_index_url,
            DEFAULT_REGISTRY_INDEX_URL,
            SkillRegistryRuntimeConfigError::NonUnicodeRegistryIndexUrl,
        )?;
        let github_api_base_url = environment_url(
            github_api_base_url,
            DEFAULT_GITHUB_API_BASE_URL,
            SkillRegistryRuntimeConfigError::NonUnicodeGithubApiBaseUrl,
        )?;
        let github_raw_base_url = environment_url(
            github_raw_base_url,
            DEFAULT_GITHUB_RAW_BASE_URL,
            SkillRegistryRuntimeConfigError::NonUnicodeGithubRawBaseUrl,
        )?;
        Self::from_urls(
            &registry_index_url,
            &github_api_base_url,
            &github_raw_base_url,
        )
    }

    fn from_urls(
        registry_index_url: &str,
        github_api_base_url: &str,
        github_raw_base_url: &str,
    ) -> Result<Self, SkillRegistryRuntimeConfigError> {
        Ok(Self {
            registry_index_url: parse_public_https_url(registry_index_url, false)
                .map_err(|_| SkillRegistryRuntimeConfigError::InvalidRegistryIndexUrl)?,
            github_api_base_url: parse_public_https_url(github_api_base_url, true)
                .map_err(|_| SkillRegistryRuntimeConfigError::InvalidGithubApiBaseUrl)?,
            github_raw_base_url: parse_public_https_url(github_raw_base_url, true)
                .map_err(|_| SkillRegistryRuntimeConfigError::InvalidGithubRawBaseUrl)?,
        })
    }
}

impl Default for SkillRegistryRuntimeConfig {
    fn default() -> Self {
        Self::from_urls(
            DEFAULT_REGISTRY_INDEX_URL,
            DEFAULT_GITHUB_API_BASE_URL,
            DEFAULT_GITHUB_RAW_BASE_URL,
        )
        .expect("the official Skill registry endpoints must remain valid")
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum SkillRegistryRuntimeConfigError {
    #[error("SYNTHCHAT_SKILL_REGISTRY_INDEX_URL must contain valid Unicode")]
    NonUnicodeRegistryIndexUrl,
    #[error("SYNTHCHAT_SKILL_GITHUB_API_BASE_URL must contain valid Unicode")]
    NonUnicodeGithubApiBaseUrl,
    #[error("SYNTHCHAT_SKILL_GITHUB_RAW_BASE_URL must contain valid Unicode")]
    NonUnicodeGithubRawBaseUrl,
    #[error(
        "SYNTHCHAT_SKILL_REGISTRY_INDEX_URL must be a public HTTPS URL without userinfo, query, or fragment"
    )]
    InvalidRegistryIndexUrl,
    #[error(
        "SYNTHCHAT_SKILL_GITHUB_API_BASE_URL must be a public HTTPS base URL without userinfo, query, or fragment"
    )]
    InvalidGithubApiBaseUrl,
    #[error(
        "SYNTHCHAT_SKILL_GITHUB_RAW_BASE_URL must be a public HTTPS base URL without userinfo, query, or fragment"
    )]
    InvalidGithubRawBaseUrl,
    #[error("the configured Skill endpoint path could not be joined safely")]
    InvalidEndpointPath,
}

fn environment_url(
    value: Result<String, env::VarError>,
    default: &str,
    non_unicode_error: SkillRegistryRuntimeConfigError,
) -> Result<String, SkillRegistryRuntimeConfigError> {
    match value {
        Ok(value) => Ok(value),
        Err(env::VarError::NotPresent) => Ok(default.to_owned()),
        Err(env::VarError::NotUnicode(_)) => Err(non_unicode_error),
    }
}

fn parse_public_https_url(
    value: &str,
    normalize_as_base: bool,
) -> Result<Url, SkillRegistryRuntimeConfigError> {
    if value.is_empty()
        || value != value.trim()
        || value.len() > MAX_ENDPOINT_URL_BYTES
        || value.chars().any(char::is_control)
        || value.contains('%')
        || value.contains('\\')
        || raw_path_has_dot_segments(value)
    {
        return Err(SkillRegistryRuntimeConfigError::InvalidEndpointPath);
    }

    let mut url =
        Url::parse(value).map_err(|_| SkillRegistryRuntimeConfigError::InvalidEndpointPath)?;
    if url.scheme() != "https"
        || url.cannot_be_a_base()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host().is_none()
        || url.port() == Some(0)
    {
        return Err(SkillRegistryRuntimeConfigError::InvalidEndpointPath);
    }

    match url
        .host()
        .ok_or(SkillRegistryRuntimeConfigError::InvalidEndpointPath)?
    {
        Host::Ipv4(address) => ensure_public_ip(address.into())?,
        Host::Ipv6(address) => ensure_public_ip(address.into())?,
        Host::Domain(domain) => {
            if domain.ends_with('.') {
                return Err(SkillRegistryRuntimeConfigError::InvalidEndpointPath);
            }
            reject_special_hostname(domain)?;
        }
    }

    let path = url.path();
    if path.len() > MAX_ENDPOINT_PATH_BYTES
        || path.contains("//")
        || path.split('/').any(|segment| matches!(segment, "." | ".."))
        || !path.bytes().all(|byte| {
            byte == b'/'
                || byte.is_ascii_alphanumeric()
                || matches!(byte, b'-' | b'.' | b'_' | b'~')
        })
        || (!normalize_as_base && (path == "/" || path.ends_with('/')))
    {
        return Err(SkillRegistryRuntimeConfigError::InvalidEndpointPath);
    }
    if normalize_as_base && !path.ends_with('/') {
        let normalized_path = format!("{path}/");
        url.set_path(&normalized_path);
    }
    Ok(url)
}

fn raw_path_has_dot_segments(value: &str) -> bool {
    value
        .split_once("://")
        .and_then(|(_, authority_and_path)| {
            authority_and_path
                .find('/')
                .map(|offset| &authority_and_path[offset..])
        })
        .is_some_and(|path| path.split('/').any(|segment| matches!(segment, "." | "..")))
}

fn append_path_segments(
    base: &Url,
    segments: &[&str],
) -> Result<Url, SkillRegistryRuntimeConfigError> {
    if segments.is_empty() {
        return Err(SkillRegistryRuntimeConfigError::InvalidEndpointPath);
    }
    let mut url = base.clone();
    let base_path = base.path().to_owned();
    let mut target = url
        .path_segments_mut()
        .map_err(|_| SkillRegistryRuntimeConfigError::InvalidEndpointPath)?;
    target.pop_if_empty();
    for segment in segments {
        if segment.is_empty()
            || segment.len() > MAX_JOIN_SEGMENT_BYTES
            || matches!(*segment, "." | "..")
            || segment.contains(['/', '\\', '?', '#', '%'])
            || segment.chars().any(char::is_control)
        {
            return Err(SkillRegistryRuntimeConfigError::InvalidEndpointPath);
        }
        target.push(segment);
    }
    drop(target);
    if !url.path().starts_with(&base_path)
        || url.query().is_some()
        || url.fragment().is_some()
        || !same_origin(base, &url)
    {
        return Err(SkillRegistryRuntimeConfigError::InvalidEndpointPath);
    }
    Ok(url)
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn reject_special_hostname(host: &str) -> Result<(), SkillRegistryRuntimeConfigError> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty()
        || host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.ends_with(".home.arpa")
        || host == "metadata.google.internal"
        || host == "metadata.aws.internal"
        || host == "instance-data"
    {
        Err(SkillRegistryRuntimeConfigError::InvalidEndpointPath)
    } else {
        Ok(())
    }
}

fn ensure_public_ip(address: IpAddr) -> Result<(), SkillRegistryRuntimeConfigError> {
    let public = match address {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => is_public_ipv6(address),
    };
    if public {
        Ok(())
    } else {
        Err(SkillRegistryRuntimeConfigError::InvalidEndpointPath)
    }
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [a, b, c, d] = address.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 88 && c == 99)
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224
        || (a == 255 && b == 255 && c == 255 && d == 255))
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    if address.to_ipv4_mapped().is_some() {
        return false;
    }
    let segments = address.segments();
    if !(0x2000..=0x3fff).contains(&segments[0]) {
        return false;
    }
    let reserved_2001 = segments[0] == 0x2001
        && (matches!(segments[1], 0 | 2 | 0x0db8) || (0x0010..=0x002f).contains(&segments[1]));
    !(reserved_2001 || (segments[0] == 0x3fff && segments[1] <= 0x0fff) || segments[0] == 0x2002)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_unicode_environment_values_are_reported_without_echoing_them() {
        let non_unicode = || env::VarError::NotUnicode(std::ffi::OsString::new());
        assert_eq!(
            SkillRegistryRuntimeConfig::from_env_values(
                Err(non_unicode()),
                Err(env::VarError::NotPresent),
                Err(env::VarError::NotPresent),
            ),
            Err(SkillRegistryRuntimeConfigError::NonUnicodeRegistryIndexUrl)
        );
        assert_eq!(
            SkillRegistryRuntimeConfig::from_env_values(
                Err(env::VarError::NotPresent),
                Err(non_unicode()),
                Err(env::VarError::NotPresent),
            ),
            Err(SkillRegistryRuntimeConfigError::NonUnicodeGithubApiBaseUrl)
        );
        assert_eq!(
            SkillRegistryRuntimeConfig::from_env_values(
                Err(env::VarError::NotPresent),
                Err(env::VarError::NotPresent),
                Err(non_unicode()),
            ),
            Err(SkillRegistryRuntimeConfigError::NonUnicodeGithubRawBaseUrl)
        );
    }
}
