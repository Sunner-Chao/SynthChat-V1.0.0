use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::OffsetDateTime;
use zeroize::Zeroizing;

use super::SessionError;

type HmacSha256 = Hmac<Sha256>;
const CURSOR_VERSION: u8 = 1;
const CURSOR_MAX_AGE_SECONDS: i64 = 24 * 60 * 60;
const MAX_CURSOR_BYTES: usize = 4096;

pub(crate) struct CursorCodec {
    key: Zeroizing<[u8; 32]>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CursorPayload {
    pub(crate) version: u8,
    pub(crate) kind: String,
    pub(crate) filter_hash: String,
    pub(crate) snapshot: i64,
    pub(crate) before_updated_at: Option<String>,
    pub(crate) before_id: Option<String>,
    pub(crate) before_sequence: Option<u64>,
    pub(crate) issued_at: i64,
}

impl CursorCodec {
    pub(crate) fn new(desktop_token: &str) -> Self {
        let mut digest = Sha256::new();
        digest.update(b"synthchat-session-cursor-v1\0");
        digest.update(desktop_token.as_bytes());
        Self {
            key: Zeroizing::new(digest.finalize().into()),
        }
    }

    pub(crate) fn filter_hash(parts: &[&str]) -> String {
        let mut digest = Sha256::new();
        digest.update(b"synthchat-session-filter-v1\0");
        for part in parts {
            digest.update((part.len() as u64).to_be_bytes());
            digest.update(part.as_bytes());
        }
        hex(&digest.finalize())
    }

    pub(crate) fn encode(&self, mut payload: CursorPayload) -> Result<String, SessionError> {
        payload.version = CURSOR_VERSION;
        payload.issued_at = OffsetDateTime::now_utc().unix_timestamp();
        let bytes = serde_json::to_vec(&payload).map_err(|_| SessionError::DataInvalid)?;
        let signature = self.sign(&bytes);
        let token = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(bytes),
            URL_SAFE_NO_PAD.encode(signature),
        );
        if token.len() > MAX_CURSOR_BYTES {
            return Err(SessionError::DataInvalid);
        }
        Ok(token)
    }

    pub(crate) fn decode(
        &self,
        token: &str,
        expected_kind: &str,
        expected_filter_hash: &str,
    ) -> Result<CursorPayload, SessionError> {
        if token.is_empty() || token.len() > MAX_CURSOR_BYTES {
            return Err(SessionError::InvalidCursor);
        }
        let mut parts = token.split('.');
        let (Some(payload), Some(signature), None) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(SessionError::InvalidCursor);
        };
        let payload = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| SessionError::InvalidCursor)?;
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| SessionError::InvalidCursor)?;
        if signature.len() != 32
            || !bool::from(self.sign(&payload).as_slice().ct_eq(signature.as_slice()))
        {
            return Err(SessionError::InvalidCursor);
        }
        let decoded: CursorPayload =
            serde_json::from_slice(&payload).map_err(|_| SessionError::InvalidCursor)?;
        let now = OffsetDateTime::now_utc().unix_timestamp();
        if decoded.version != CURSOR_VERSION
            || decoded.kind != expected_kind
            || decoded.filter_hash != expected_filter_hash
            || decoded.snapshot < 0
            || decoded.issued_at > now + 300
            || now.saturating_sub(decoded.issued_at) > CURSOR_MAX_AGE_SECONDS
        {
            return Err(SessionError::InvalidCursor);
        }
        Ok(decoded)
    }

    fn sign(&self, bytes: &[u8]) -> [u8; 32] {
        let mut mac = HmacSha256::new_from_slice(self.key.as_ref())
            .expect("a SHA-256 digest is a valid HMAC key");
        mac.update(bytes);
        mac.finalize().into_bytes().into()
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}
