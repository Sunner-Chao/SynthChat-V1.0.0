use std::env;

use thiserror::Error;
use url::{Host, Url};

pub const TAVILY_BASE_URL_ENV: &str = "SYNTHCHAT_TAVILY_BASE_URL";
const DEFAULT_TAVILY_BASE_URL: &str = "https://api.tavily.com";
const MAX_BASE_URL_BYTES: usize = 2_048;
const MAX_BASE_PATH_BYTES: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebRuntimeConfig {
    tavily_base_url: Url,
}

impl WebRuntimeConfig {
    pub fn from_env() -> Result<Self, WebRuntimeConfigError> {
        Self::from_env_value(env::var(TAVILY_BASE_URL_ENV))
    }

    pub fn from_tavily_base_url(value: impl AsRef<str>) -> Result<Self, WebRuntimeConfigError> {
        Ok(Self {
            tavily_base_url: parse_tavily_base_url(value.as_ref())?,
        })
    }

    pub fn tavily_base_url(&self) -> &Url {
        &self.tavily_base_url
    }

    pub fn effective_tavily_base_url(&self) -> String {
        catalog_base_url(&self.tavily_base_url)
    }

    fn from_env_value(value: Result<String, env::VarError>) -> Result<Self, WebRuntimeConfigError> {
        match value {
            Ok(value) => Self::from_tavily_base_url(value),
            Err(env::VarError::NotPresent) => Self::from_tavily_base_url(DEFAULT_TAVILY_BASE_URL),
            Err(env::VarError::NotUnicode(_)) => {
                Err(WebRuntimeConfigError::NonUnicodeTavilyBaseUrl)
            }
        }
    }
}

impl Default for WebRuntimeConfig {
    fn default() -> Self {
        Self::from_tavily_base_url(DEFAULT_TAVILY_BASE_URL)
            .expect("the official Tavily base URL must remain valid")
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum WebRuntimeConfigError {
    #[error("SYNTHCHAT_TAVILY_BASE_URL must contain valid Unicode")]
    NonUnicodeTavilyBaseUrl,
    #[error(
        "SYNTHCHAT_TAVILY_BASE_URL must be a public HTTPS base URL without userinfo, query, or fragment"
    )]
    InvalidTavilyBaseUrl,
}

fn parse_tavily_base_url(value: &str) -> Result<Url, WebRuntimeConfigError> {
    if value.is_empty()
        || value != value.trim()
        || value.len() > MAX_BASE_URL_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(WebRuntimeConfigError::InvalidTavilyBaseUrl);
    }

    let mut url = Url::parse(value).map_err(|_| WebRuntimeConfigError::InvalidTavilyBaseUrl)?;
    if url.scheme() != "https"
        || url.cannot_be_a_base()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host().is_none()
        || url.port() == Some(0)
    {
        return Err(WebRuntimeConfigError::InvalidTavilyBaseUrl);
    }

    match url
        .host()
        .ok_or(WebRuntimeConfigError::InvalidTavilyBaseUrl)?
    {
        Host::Ipv4(address) => super::ensure_public_ip(address.into())
            .map_err(|_| WebRuntimeConfigError::InvalidTavilyBaseUrl)?,
        Host::Ipv6(address) => super::ensure_public_ip(address.into())
            .map_err(|_| WebRuntimeConfigError::InvalidTavilyBaseUrl)?,
        Host::Domain(domain) => {
            if domain.ends_with('.') || super::reject_special_hostname(domain).is_err() {
                return Err(WebRuntimeConfigError::InvalidTavilyBaseUrl);
            }
        }
    }

    let path = url.path();
    if path.len() > MAX_BASE_PATH_BYTES
        || path.contains("//")
        || !path.bytes().all(|byte| {
            byte == b'/'
                || byte.is_ascii_alphanumeric()
                || matches!(byte, b'-' | b'.' | b'_' | b'~')
        })
    {
        return Err(WebRuntimeConfigError::InvalidTavilyBaseUrl);
    }
    if !path.ends_with('/') {
        let normalized_path = format!("{path}/");
        url.set_path(&normalized_path);
    }
    Ok(url)
}

fn catalog_base_url(url: &Url) -> String {
    url.as_str().trim_end_matches('/').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_environment_uses_the_official_endpoint() {
        let config = WebRuntimeConfig::from_env_value(Err(env::VarError::NotPresent)).unwrap();
        assert_eq!(config.tavily_base_url().as_str(), "https://api.tavily.com/");
        assert_eq!(config.effective_tavily_base_url(), "https://api.tavily.com");
    }

    #[test]
    fn custom_https_base_path_is_canonicalized_once() {
        let config = WebRuntimeConfig::from_tavily_base_url(
            "https://web-gateway.example.test:8443/providers/tavily",
        )
        .unwrap();
        assert_eq!(
            config.tavily_base_url().as_str(),
            "https://web-gateway.example.test:8443/providers/tavily/"
        );
        assert_eq!(
            config.effective_tavily_base_url(),
            "https://web-gateway.example.test:8443/providers/tavily"
        );
    }

    #[test]
    fn endpoint_validation_rejects_unsafe_authorities_and_url_components() {
        for value in [
            "http://api.tavily.com",
            "https://user@api.tavily.com",
            "https://api.tavily.com?key=value",
            "https://api.tavily.com#fragment",
            "https://127.0.0.1",
            "https://metadata.google.internal",
            "https://api.tavily.com/a//b",
            "https://api.tavily.com/a%2Fb",
        ] {
            assert_eq!(
                WebRuntimeConfig::from_tavily_base_url(value),
                Err(WebRuntimeConfigError::InvalidTavilyBaseUrl),
                "{value} should be rejected"
            );
        }
    }

    #[test]
    fn non_unicode_environment_is_reported_without_echoing_its_value() {
        assert_eq!(
            WebRuntimeConfig::from_env_value(Err(env::VarError::NotUnicode(
                std::ffi::OsString::new(),
            ))),
            Err(WebRuntimeConfigError::NonUnicodeTavilyBaseUrl)
        );
    }
}
