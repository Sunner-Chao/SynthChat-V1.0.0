use std::{collections::BTreeSet, net::IpAddr, sync::Arc, time::Duration};

use axum::http::{HeaderMap, HeaderValue};
use base64::{Engine as _, engine::general_purpose};
use futures_util::StreamExt;
use qrcode::QrCode;
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use url::Url;
use uuid::Uuid;

use crate::{
    product_catalog::{ProductCatalogError, ProductCatalogService},
    profiles::{ProfileError, ProfileService, Versioned},
};

const DEFAULT_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
const OFFICIAL_HOST: &str = "ilinkai.weixin.qq.com";
const DEFAULT_TIMEOUT_SECONDS: u64 = 35;
const MIN_TIMEOUT_SECONDS: u64 = 5;
const MAX_TIMEOUT_SECONDS: u64 = 60;
const EXTENSION_KEY: &str = "wechat";
const CHANNEL_VERSION: &str = "2.4.6";
const MAX_QR_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_SEND_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_POLL_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_POLL_MESSAGES: usize = 100;
const MAX_CURSOR_BYTES: usize = 16 * 1024;
const MAX_PEER_CHARS: usize = 256;
const MAX_MESSAGE_CHARS: usize = 16_000;

#[derive(Clone)]
pub struct WechatService {
    profiles: Arc<ProfileService>,
    product_catalog: Arc<ProductCatalogService>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredConfig {
    #[serde(default = "default_base_url")]
    base_url: String,
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
    #[serde(default)]
    accounts: Vec<StoredAccount>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredAccount {
    id: String,
    note: String,
    online: bool,
    created_at: String,
    last_login_at: String,
    ilink_user_id: String,
    login_base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    linked_persona_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WechatConfig {
    pub revision: String,
    pub base_url: String,
    pub timeout_seconds: u64,
    pub accounts: Vec<WechatAccount>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WechatAccount {
    pub id: String,
    pub note: String,
    pub online: bool,
    pub created_at: String,
    pub last_login_at: String,
    pub ilink_user_id: String,
    pub login_base_url: String,
    pub credential_configured: bool,
    pub linked_persona_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WechatConfigPatch {
    pub base_url: Option<String>,
    pub timeout_seconds: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QrStartRequest {
    pub base_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QrStatusRequest {
    pub qrcode: String,
    pub base_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct QrStartResult {
    pub qrcode: String,
    pub qr_image: String,
    pub base_url: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct QrStatusResult {
    pub status: String,
    pub message: Option<String>,
    pub account: Option<WechatAccount>,
    pub host: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum NullablePersonaId {
    Linked(String),
    Unlinked(()),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WechatAccountLinkPatch {
    pub linked_persona_id: NullablePersonaId,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WechatPollRequest {
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WechatInboundMessage {
    pub id: String,
    pub peer: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WechatPollResult {
    pub messages: Vec<WechatInboundMessage>,
    pub next_cursor: Option<String>,
    pub received_count: usize,
    pub skipped_count: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WechatSendRequest {
    pub peer: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WechatSendResult {
    pub accepted: bool,
    pub message_id: Option<String>,
}

#[derive(Debug, Error)]
pub enum WechatError {
    #[error(transparent)]
    Profile(#[from] ProfileError),
    #[error("invalid WeChat configuration")]
    InvalidConfig,
    #[error("invalid WeChat QR request")]
    InvalidQrRequest,
    #[error("invalid WeChat request")]
    InvalidRequest,
    #[error("WeChat account not found")]
    AccountNotFound,
    #[error("linked Persona not found")]
    PersonaNotFound,
    #[error("Persona is already linked to another WeChat account")]
    PersonaLinkConflict,
    #[error("product catalog is unavailable")]
    ProductCatalogUnavailable,
    #[error("WeChat account credential is not configured")]
    CredentialNotConfigured,
    #[error("WeChat service is unavailable")]
    Unavailable,
    #[error("WeChat service rejected the request")]
    Rejected,
    #[error("WeChat service returned an invalid response")]
    InvalidResponse,
    #[error("WeChat QR login completed without a bot credential")]
    MissingCredential,
}

impl WechatService {
    pub fn new(profiles: Arc<ProfileService>, product_catalog: Arc<ProductCatalogService>) -> Self {
        Self {
            profiles,
            product_catalog,
        }
    }

    pub fn get_config(&self, profile_id: &str) -> Result<Versioned<WechatConfig>, WechatError> {
        let profile = self.profiles.get_config(profile_id)?;
        let stored = read_stored(&profile.value.extensions)?;
        let configured: BTreeSet<String> = self
            .profiles
            .list_secret_statuses(profile_id)?
            .into_iter()
            .filter(|item| item.configured)
            .map(|item| item.name)
            .collect();
        let accounts = stored
            .accounts
            .iter()
            .map(|account| {
                public_account(account, configured.contains(&credential_name(&account.id)))
            })
            .collect();
        Ok(Versioned {
            value: WechatConfig {
                revision: profile.value.revision,
                base_url: stored.base_url,
                timeout_seconds: stored.timeout_seconds,
                accounts,
            },
            etag: profile.etag,
        })
    }

    pub fn update_config(
        &self,
        profile_id: &str,
        etag: &str,
        patch: &WechatConfigPatch,
    ) -> Result<Versioned<WechatConfig>, WechatError> {
        let current = self.profiles.get_config(profile_id)?;
        if current.etag != etag {
            return Err(ProfileError::RevisionConflict {
                current_etag: current.etag,
            }
            .into());
        }
        let mut stored = read_stored(&current.value.extensions)?;
        if let Some(base_url) = patch.base_url.as_deref() {
            stored.base_url = normalize_base_url(base_url)?
                .to_string()
                .trim_end_matches('/')
                .to_owned();
        }
        if let Some(timeout) = patch.timeout_seconds {
            if !(MIN_TIMEOUT_SECONDS..=MAX_TIMEOUT_SECONDS).contains(&timeout) {
                return Err(WechatError::InvalidConfig);
            }
            stored.timeout_seconds = timeout;
        }
        if patch.base_url.is_some() || patch.timeout_seconds.is_some() {
            write_stored(&self.profiles, profile_id, etag, &stored)?;
        }
        self.get_config(profile_id)
    }

    pub fn update_account_link(
        &self,
        profile_id: &str,
        account_id: &str,
        etag: &str,
        patch: &WechatAccountLinkPatch,
    ) -> Result<Versioned<WechatConfig>, WechatError> {
        let account_id = validated_identifier(account_id, MAX_PEER_CHARS)?;
        let current = self.profiles.get_config(profile_id)?;
        if current.etag != etag {
            return Err(ProfileError::RevisionConflict {
                current_etag: current.etag,
            }
            .into());
        }
        let mut stored = read_stored(&current.value.extensions)?;
        let account_index = stored
            .accounts
            .iter()
            .position(|account| account.id == account_id)
            .ok_or(WechatError::AccountNotFound)?;
        let linked_persona_id = match &patch.linked_persona_id {
            NullablePersonaId::Linked(persona_id) => {
                let persona_id = validated_identifier(persona_id, MAX_PEER_CHARS)?;
                match self.product_catalog.get_persona(profile_id, &persona_id) {
                    Ok(_) => {}
                    Err(ProductCatalogError::InvalidRequest) => {
                        return Err(WechatError::InvalidRequest);
                    }
                    Err(ProductCatalogError::NotFound) => {
                        return Err(WechatError::PersonaNotFound);
                    }
                    Err(
                        ProductCatalogError::RevisionConflict { .. }
                        | ProductCatalogError::StorageUnavailable
                        | ProductCatalogError::LimitReached,
                    ) => return Err(WechatError::ProductCatalogUnavailable),
                }
                if stored.accounts.iter().enumerate().any(|(index, account)| {
                    index != account_index
                        && account.linked_persona_id.as_deref() == Some(persona_id.as_str())
                }) {
                    return Err(WechatError::PersonaLinkConflict);
                }
                Some(persona_id)
            }
            NullablePersonaId::Unlinked(()) => None,
        };
        stored.accounts[account_index].linked_persona_id = linked_persona_id;
        write_stored(&self.profiles, profile_id, etag, &stored)?;
        self.get_config(profile_id)
    }

    pub async fn start_qr(
        &self,
        profile_id: &str,
        request: &QrStartRequest,
    ) -> Result<QrStartResult, WechatError> {
        let config = self.get_config(profile_id)?;
        let base_url = normalize_base_url(
            request
                .base_url
                .as_deref()
                .unwrap_or(&config.value.base_url),
        )?;
        let response = wechat_client(config.value.timeout_seconds.min(20))?
            .get(endpoint(&base_url, "/ilink/bot/get_bot_qrcode")?)
            .headers(common_headers())
            .query(&[("bot_type", "3")])
            .send()
            .await
            .map_err(|_| WechatError::Unavailable)?;
        let raw = parse_response(response, MAX_QR_RESPONSE_BYTES).await?;
        let qrcode = first_string(
            &raw,
            &[
                &["qrcode"],
                &["data", "qrcode"],
                &["qrCode"],
                &["data", "qrCode"],
            ],
        )
        .filter(|value| !value.trim().is_empty())
        .ok_or(WechatError::InvalidResponse)?;
        let content = first_string(
            &raw,
            &[
                &["qrcode_img_content"],
                &["data", "qrcode_img_content"],
                &["qr_image"],
                &["data", "qr_image"],
            ],
        )
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| qrcode.clone());
        Ok(QrStartResult {
            qrcode,
            qr_image: qr_svg(&content)?,
            base_url: base_url.to_string().trim_end_matches('/').to_owned(),
        })
    }

    pub async fn check_qr(
        &self,
        profile_id: &str,
        request: &QrStatusRequest,
    ) -> Result<QrStatusResult, WechatError> {
        let qrcode = request.qrcode.trim();
        if qrcode.is_empty() || qrcode.len() > 4096 || qrcode.chars().any(char::is_control) {
            return Err(WechatError::InvalidQrRequest);
        }
        let config = self.get_config(profile_id)?;
        let base_url = normalize_base_url(
            request
                .base_url
                .as_deref()
                .unwrap_or(&config.value.base_url),
        )?;
        let response = wechat_client(config.value.timeout_seconds)?
            .get(endpoint(&base_url, "/ilink/bot/get_qrcode_status")?)
            .headers(common_headers())
            .query(&[("qrcode", qrcode)])
            .send()
            .await
            .map_err(|_| WechatError::Unavailable)?;
        let raw = parse_response(response, MAX_QR_RESPONSE_BYTES).await?;
        let status = first_string(
            &raw,
            &[
                &["status"],
                &["data", "status"],
                &["state"],
                &["data", "state"],
            ],
        )
        .unwrap_or_else(|| "unknown".to_owned());
        let message = first_string(
            &raw,
            &[
                &["message"],
                &["msg"],
                &["data", "message"],
                &["data", "msg"],
            ],
        );
        let host = first_string(
            &raw,
            &[
                &["host"],
                &["data", "host"],
                &["redirect_host"],
                &["data", "redirect_host"],
            ],
        );
        let account = if is_confirmed(&status) {
            Some(self.persist_account(profile_id, &base_url, &raw)?)
        } else {
            None
        };
        Ok(QrStatusResult {
            status,
            message,
            account,
            host,
        })
    }

    pub async fn poll_messages(
        &self,
        profile_id: &str,
        account_id: &str,
        request: &WechatPollRequest,
    ) -> Result<WechatPollResult, WechatError> {
        let cursor = request.cursor.as_deref().unwrap_or("");
        validate_cursor(cursor)?;
        let (config, account, credential) = self.account_credential(profile_id, account_id)?;
        let base_url = account_base_url(&config, &account)?;
        let response = wechat_client(config.timeout_seconds)?
            .post(endpoint(&base_url, "/ilink/bot/getupdates")?)
            .headers(authenticated_headers(&credential)?)
            .json(&json!({
                "get_updates_buf": cursor,
                "base_info": wechat_base_info()
            }))
            .send()
            .await
            .map_err(|_| WechatError::Unavailable)?;
        let raw = parse_response(response, MAX_POLL_RESPONSE_BYTES).await?;
        ensure_upstream_ok(&raw)?;
        let next_cursor = first_string(
            &raw,
            &[
                &["get_updates_buf"],
                &["getUpdatesBuf"],
                &["data", "get_updates_buf"],
                &["data", "getUpdatesBuf"],
            ],
        )
        .filter(|value| !value.is_empty());
        if let Some(cursor) = next_cursor.as_deref() {
            validate_cursor(cursor)?;
        }
        let messages = raw
            .get("msgs")
            .or_else(|| raw.get("data").and_then(|value| value.get("msgs")))
            .and_then(JsonValue::as_array)
            .cloned()
            .unwrap_or_default();
        if messages.len() > MAX_POLL_MESSAGES {
            return Err(WechatError::InvalidResponse);
        }
        let received_count = messages.len();
        let normalized = messages
            .iter()
            .filter_map(normalize_inbound_message)
            .collect::<Vec<_>>();
        Ok(WechatPollResult {
            skipped_count: received_count.saturating_sub(normalized.len()),
            received_count,
            messages: normalized,
            next_cursor,
        })
    }

    pub async fn send_message(
        &self,
        profile_id: &str,
        account_id: &str,
        request: &WechatSendRequest,
    ) -> Result<WechatSendResult, WechatError> {
        let peer = validated_identifier(&request.peer, MAX_PEER_CHARS)?;
        let text = normalized_message_text(&request.text)?;
        let (config, account, credential) = self.account_credential(profile_id, account_id)?;
        let base_url = account_base_url(&config, &account)?;
        let response = wechat_client(config.timeout_seconds.min(30))?
            .post(endpoint(&base_url, "/ilink/bot/sendmessage")?)
            .headers(authenticated_headers(&credential)?)
            .json(&json!({
                "msg": {
                    "to_user_id": peer,
                    "from_user_id": "",
                    "client_id": format!("synthchat-weixin-{}", Uuid::new_v4().simple()),
                    "message_type": 2,
                    "message_state": 2,
                    "item_list": [{
                        "type": 1,
                        "text_item": { "text": text }
                    }]
                },
                "base_info": wechat_base_info()
            }))
            .send()
            .await
            .map_err(|_| WechatError::Unavailable)?;
        let raw = parse_response(response, MAX_SEND_RESPONSE_BYTES).await?;
        ensure_upstream_ok(&raw)?;
        let message_id = first_string(
            &raw,
            &[
                &["message_id"],
                &["messageId"],
                &["msg_id"],
                &["msgId"],
                &["data", "message_id"],
                &["data", "messageId"],
                &["data", "msg_id"],
                &["data", "msgId"],
            ],
        )
        .and_then(|value| normalized_optional_identifier(&value, 256));
        Ok(WechatSendResult {
            accepted: true,
            message_id,
        })
    }

    fn account_credential(
        &self,
        profile_id: &str,
        account_id: &str,
    ) -> Result<(StoredConfig, StoredAccount, SecretString), WechatError> {
        let account_id = validated_identifier(account_id, MAX_PEER_CHARS)?;
        let profile = self.profiles.get_config(profile_id)?;
        let stored = read_stored(&profile.value.extensions)?;
        let account = stored
            .accounts
            .iter()
            .find(|account| account.id == account_id)
            .cloned()
            .ok_or(WechatError::AccountNotFound)?;
        let secret_name = credential_name(&account.id);
        let credential = self
            .profiles
            .first_secret_snapshot(profile_id, &[secret_name], true)?
            .map(|(_, credential)| credential)
            .ok_or(WechatError::CredentialNotConfigured)?;
        Ok((stored, account, credential))
    }

    fn persist_account(
        &self,
        profile_id: &str,
        base_url: &Url,
        raw: &JsonValue,
    ) -> Result<WechatAccount, WechatError> {
        let token = first_string(
            raw,
            &[
                &["bot_token"],
                &["data", "bot_token"],
                &["ilink_bot_token"],
                &["data", "ilink_bot_token"],
            ],
        )
        .filter(|value| !value.trim().is_empty())
        .ok_or(WechatError::MissingCredential)?;
        let id = bounded(
            first_string(
                raw,
                &[
                    &["ilink_bot_id"],
                    &["data", "ilink_bot_id"],
                    &["bot_id"],
                    &["data", "bot_id"],
                    &["botId"],
                    &["data", "botId"],
                ],
            )
            .unwrap_or_else(|| format!("wechat-{}", Uuid::new_v4().simple())),
            256,
        );
        if id.is_empty() {
            return Err(WechatError::InvalidResponse);
        }
        let user_id = bounded(
            first_string(
                raw,
                &[
                    &["ilink_user_id"],
                    &["data", "ilink_user_id"],
                    &["user_id"],
                    &["data", "user_id"],
                ],
            )
            .unwrap_or_default(),
            256,
        );
        let note = bounded(
            first_string(
                raw,
                &[
                    &["nickname"],
                    &["data", "nickname"],
                    &["name"],
                    &["data", "name"],
                ],
            )
            .unwrap_or_else(|| "微信账号".to_owned()),
            80,
        );
        let current = self.profiles.get_config(profile_id)?;
        let mut stored = read_stored(&current.value.extensions)?;
        let now = now_timestamp()?;
        let previous = stored.accounts.iter().find(|item| item.id == id).cloned();
        let account = StoredAccount {
            id: id.clone(),
            note: if note.is_empty() {
                "微信账号".to_owned()
            } else {
                note
            },
            online: true,
            created_at: previous
                .as_ref()
                .map(|item| item.created_at.clone())
                .unwrap_or_else(|| now.clone()),
            last_login_at: now,
            ilink_user_id: user_id,
            login_base_url: base_url.to_string().trim_end_matches('/').to_owned(),
            linked_persona_id: previous.and_then(|item| item.linked_persona_id),
        };
        self.profiles.put_secret(
            profile_id,
            &credential_name(&id),
            &SecretString::from(token),
        )?;
        if let Some(existing) = stored.accounts.iter_mut().find(|item| item.id == id) {
            *existing = account.clone();
        } else {
            stored.accounts.push(account.clone());
        }
        if let Err(error) = write_stored(&self.profiles, profile_id, &current.etag, &stored) {
            let _ = self
                .profiles
                .delete_secret(profile_id, &credential_name(&id));
            return Err(error);
        }
        Ok(public_account(&account, true))
    }
}

fn default_base_url() -> String {
    DEFAULT_BASE_URL.to_owned()
}
const fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_SECONDS
}

fn read_stored(
    extensions: &serde_json::Map<String, JsonValue>,
) -> Result<StoredConfig, WechatError> {
    let Some(value) = extensions.get(EXTENSION_KEY) else {
        return Ok(StoredConfig::default());
    };
    let mut config: StoredConfig =
        serde_json::from_value(value.clone()).map_err(|_| WechatError::InvalidConfig)?;
    config.base_url = normalize_base_url(&config.base_url)?
        .to_string()
        .trim_end_matches('/')
        .to_owned();
    if !(MIN_TIMEOUT_SECONDS..=MAX_TIMEOUT_SECONDS).contains(&config.timeout_seconds)
        || config.accounts.len() > 16
        || config.accounts.iter().any(invalid_account)
    {
        return Err(WechatError::InvalidConfig);
    }
    Ok(config)
}

fn write_stored(
    profiles: &ProfileService,
    profile_id: &str,
    etag: &str,
    stored: &StoredConfig,
) -> Result<(), WechatError> {
    let value = serde_json::to_value(stored).map_err(|_| WechatError::InvalidConfig)?;
    let extensions = serde_json::Map::from_iter([(EXTENSION_KEY.to_owned(), value)]);
    profiles.update_config(profile_id, etag, &json!({ "extensions": extensions }))?;
    Ok(())
}

impl Default for StoredConfig {
    fn default() -> Self {
        Self {
            base_url: default_base_url(),
            timeout_seconds: default_timeout(),
            accounts: Vec::new(),
        }
    }
}

fn invalid_account(account: &StoredAccount) -> bool {
    account.id.is_empty()
        || account.id.len() > 256
        || account.note.len() > 320
        || account.ilink_user_id.len() > 256
        || account
            .linked_persona_id
            .as_ref()
            .is_some_and(|value| normalized_optional_identifier(value, MAX_PEER_CHARS).is_none())
        || account.id.chars().any(char::is_control)
        || account.note.chars().any(char::is_control)
        || account.ilink_user_id.chars().any(char::is_control)
}

fn public_account(account: &StoredAccount, credential_configured: bool) -> WechatAccount {
    WechatAccount {
        id: account.id.clone(),
        note: account.note.clone(),
        online: account.online,
        created_at: account.created_at.clone(),
        last_login_at: account.last_login_at.clone(),
        ilink_user_id: account.ilink_user_id.clone(),
        login_base_url: account.login_base_url.clone(),
        credential_configured,
        linked_persona_id: account.linked_persona_id.clone(),
    }
}

fn credential_name(account_id: &str) -> String {
    let digest = Sha256::digest(account_id.as_bytes());
    let suffix = digest[..12]
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<String>();
    format!("WECHAT_BOT_{suffix}")
}

fn normalize_base_url(value: &str) -> Result<Url, WechatError> {
    let url = Url::parse(value.trim()).map_err(|_| WechatError::InvalidConfig)?;
    if url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (url.path() != "" && url.path() != "/")
    {
        return Err(WechatError::InvalidConfig);
    }
    let host = url.host_str().ok_or(WechatError::InvalidConfig)?;
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if !((url.scheme() == "https" && host.eq_ignore_ascii_case(OFFICIAL_HOST))
        || (cfg!(debug_assertions) && url.scheme() == "http" && loopback))
    {
        return Err(WechatError::InvalidConfig);
    }
    Ok(url)
}

fn endpoint(base: &Url, path: &str) -> Result<Url, WechatError> {
    base.join(path).map_err(|_| WechatError::InvalidConfig)
}
fn common_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("iLink-App-Id", HeaderValue::from_static("bot"));
    headers.insert(
        "iLink-App-ClientVersion",
        HeaderValue::from_static("132097"),
    );
    headers.insert("Accept", HeaderValue::from_static("application/json"));
    headers
}

fn authenticated_headers(credential: &SecretString) -> Result<HeaderMap, WechatError> {
    let token = credential.expose_secret().trim();
    if token.is_empty() {
        return Err(WechatError::CredentialNotConfigured);
    }
    let mut headers = common_headers();
    headers.insert(
        "AuthorizationType",
        HeaderValue::from_static("ilink_bot_token"),
    );
    let authorization = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| WechatError::CredentialNotConfigured)?;
    headers.insert("Authorization", authorization);
    Ok(headers)
}

fn wechat_base_info() -> JsonValue {
    json!({
        "channel_version": CHANNEL_VERSION,
        "bot_agent": "OpenClaw"
    })
}

fn account_base_url(config: &StoredConfig, account: &StoredAccount) -> Result<Url, WechatError> {
    normalize_base_url(if account.login_base_url.trim().is_empty() {
        &config.base_url
    } else {
        &account.login_base_url
    })
}

fn validate_cursor(value: &str) -> Result<(), WechatError> {
    if value.len() > MAX_CURSOR_BYTES || value.chars().any(char::is_control) {
        return Err(WechatError::InvalidRequest);
    }
    Ok(())
}

fn validated_identifier(value: &str, maximum: usize) -> Result<String, WechatError> {
    normalized_optional_identifier(value, maximum).ok_or(WechatError::InvalidRequest)
}

fn normalized_optional_identifier(value: &str, maximum: usize) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.chars().count() <= maximum && !value.chars().any(char::is_control))
        .then(|| value.to_owned())
}

fn normalized_message_text(value: &str) -> Result<String, WechatError> {
    let value = value.replace("\r\n", "\n").replace('\r', "\n");
    let value = value.trim();
    if value.is_empty()
        || value.chars().count() > MAX_MESSAGE_CHARS
        || value
            .chars()
            .any(|character| character.is_control() && character != '\n' && character != '\t')
    {
        return Err(WechatError::InvalidRequest);
    }
    Ok(value.to_owned())
}

fn wechat_client(timeout: u64) -> Result<Client, WechatError> {
    Client::builder()
        .timeout(Duration::from_secs(
            timeout.clamp(MIN_TIMEOUT_SECONDS, MAX_TIMEOUT_SECONDS),
        ))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| WechatError::Unavailable)
}

async fn parse_response(
    response: reqwest::Response,
    maximum_bytes: usize,
) -> Result<JsonValue, WechatError> {
    if !response.status().is_success() {
        return if response.status().is_client_error() {
            Err(WechatError::Rejected)
        } else {
            Err(WechatError::Unavailable)
        };
    }
    if response
        .content_length()
        .is_some_and(|length| length > maximum_bytes as u64)
    {
        return Err(WechatError::InvalidResponse);
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim();
    if !content_type.eq_ignore_ascii_case("application/json") && !content_type.ends_with("+json") {
        return Err(WechatError::InvalidResponse);
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| WechatError::Unavailable)?;
        if body.len().saturating_add(chunk.len()) > maximum_bytes {
            return Err(WechatError::InvalidResponse);
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| WechatError::InvalidResponse)
}

fn ensure_upstream_ok(value: &JsonValue) -> Result<(), WechatError> {
    let code = first_string(
        value,
        &[
            &["errcode"],
            &["ret"],
            &["data", "errcode"],
            &["data", "ret"],
        ],
    )
    .and_then(|value| value.parse::<i64>().ok())
    .unwrap_or(0);
    if code == 0 {
        Ok(())
    } else {
        Err(WechatError::Rejected)
    }
}

fn first_string(value: &JsonValue, paths: &[&[&str]]) -> Option<String> {
    paths.iter().find_map(|path| {
        let mut current = value;
        for key in *path {
            current = current.get(*key)?;
        }
        current
            .as_str()
            .map(str::to_owned)
            .or_else(|| current.as_i64().map(|n| n.to_string()))
            .or_else(|| current.as_u64().map(|n| n.to_string()))
    })
}

fn normalize_inbound_message(value: &JsonValue) -> Option<WechatInboundMessage> {
    let peer = first_string(
        value,
        &[
            &["from_user_id"],
            &["fromUserId"],
            &["user_id"],
            &["userId"],
            &["sender"],
        ],
    )
    .and_then(|value| normalized_optional_identifier(&value, MAX_PEER_CHARS))?;
    let text = inbound_text(value).and_then(|value| normalized_message_text(&value).ok())?;
    let id = first_string(
        value,
        &[
            &["message_id"],
            &["messageId"],
            &["msg_id"],
            &["msgId"],
            &["id"],
        ],
    )
    .and_then(|value| normalized_optional_identifier(&value, 256))
    .unwrap_or_else(|| inbound_fingerprint(value));
    Some(WechatInboundMessage { id, peer, text })
}

fn inbound_text(value: &JsonValue) -> Option<String> {
    first_string(
        value,
        &[
            &["text"],
            &["content"],
            &["text_item", "text"],
            &["textItem", "text"],
        ],
    )
    .or_else(|| {
        value
            .get("item_list")
            .or_else(|| value.get("itemList"))
            .and_then(JsonValue::as_array)
            .and_then(|items| {
                items.iter().find_map(|item| {
                    first_string(
                        item,
                        &[
                            &["text_item", "text"],
                            &["textItem", "text"],
                            &["text"],
                            &["content"],
                        ],
                    )
                })
            })
    })
}

fn inbound_fingerprint(value: &JsonValue) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    let digest = Sha256::digest(bytes);
    let suffix = digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("wechat_msg_{suffix}")
}

fn qr_svg(content: &str) -> Result<String, WechatError> {
    let code = QrCode::new(content.as_bytes()).map_err(|_| WechatError::InvalidResponse)?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(224, 224)
        .quiet_zone(true)
        .dark_color(qrcode::render::svg::Color("#111111"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .build();
    Ok(format!(
        "data:image/svg+xml;base64,{}",
        general_purpose::STANDARD.encode(svg.as_bytes())
    ))
}

fn is_confirmed(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "confirmed" | "success" | "ok" | "logged_in" | "login_success" | "2" | "3"
    )
}
fn bounded(value: String, max: usize) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_owned()
}
fn now_timestamp() -> Result<String, WechatError> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|_| WechatError::InvalidResponse)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_official_https_or_debug_loopback_is_allowed() {
        assert!(normalize_base_url(DEFAULT_BASE_URL).is_ok());
        assert!(normalize_base_url("https://example.com").is_err());
        assert!(normalize_base_url("https://user:pass@ilinkai.weixin.qq.com").is_err());
        if cfg!(debug_assertions) {
            assert!(normalize_base_url("http://127.0.0.1:8765").is_ok());
        }
    }

    #[test]
    fn credential_name_does_not_embed_account_id() {
        let name = credential_name("private-account-id");
        assert_eq!(name, credential_name("private-account-id"));
        assert!(!name.contains("private-account-id"));
    }
}
