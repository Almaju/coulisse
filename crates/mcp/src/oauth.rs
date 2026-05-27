use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::error::McpError;

type HmacSha256 = Hmac<Sha256>;

/// Signed state token passed through the OAuth redirect. Format:
/// `base64url(json_payload).<base64url(hmac)>`.
/// Expires after 600 seconds.
pub struct StateToken {
    pub server: String,
    pub user_id: String,
}

#[must_use]
pub fn generate_state(hmac_key: &[u8], server: &str, user_id: &str) -> String {
    let exp = coulisse_core::now_secs() + 600;
    let payload = serde_json::json!({
        "exp": exp,
        "server": server,
        "user_id": user_id,
    })
    .to_string();
    let payload_b64 = B64URL.encode(payload.as_bytes());
    let sig = sign(hmac_key, &payload_b64);
    format!("{payload_b64}.{sig}")
}

/// Validate and decode a state token. Returns `McpError::StateInvalid` if
/// the HMAC doesn't match and `McpError::StateExpired` if the token is past
/// its expiry time.
///
/// # Errors
///
/// Returns an error if the state token is invalid or expired.
pub fn validate_state(hmac_key: &[u8], token: &str) -> Result<StateToken, McpError> {
    let (payload_b64, sig) = token.split_once('.').ok_or(McpError::StateInvalid)?;
    let expected = sign(hmac_key, payload_b64);
    if !constant_time_eq(sig.as_bytes(), expected.as_bytes()) {
        return Err(McpError::StateInvalid);
    }
    let payload_bytes = B64URL
        .decode(payload_b64)
        .map_err(|_| McpError::StateInvalid)?;
    let payload: serde_json::Value =
        serde_json::from_slice(&payload_bytes).map_err(|_| McpError::StateInvalid)?;
    let exp = payload["exp"].as_u64().ok_or(McpError::StateInvalid)?;
    if coulisse_core::now_secs() > exp {
        return Err(McpError::StateExpired);
    }
    let server = payload["server"]
        .as_str()
        .ok_or(McpError::StateInvalid)?
        .to_string();
    let user_id = payload["user_id"]
        .as_str()
        .ok_or(McpError::StateInvalid)?
        .to_string();
    Ok(StateToken { server, user_id })
}

fn sign(key: &[u8], payload: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(payload.as_bytes());
    B64URL.encode(mac.finalize().into_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"test-hmac-key-32-bytes-padding!!";

    #[test]
    fn round_trip_valid_token() {
        let token = generate_state(KEY, "github", "user-42");
        let state = validate_state(KEY, &token).unwrap();
        assert_eq!(state.server, "github");
        assert_eq!(state.user_id, "user-42");
    }

    #[test]
    fn tampered_signature_rejected() {
        let token = generate_state(KEY, "github", "user-42");
        let tampered = format!("{token}x");
        assert!(matches!(
            validate_state(KEY, &tampered),
            Err(McpError::StateInvalid)
        ));
    }

    #[test]
    fn tampered_payload_rejected() {
        let token = generate_state(KEY, "github", "user-42");
        let parts: Vec<&str> = token.splitn(2, '.').collect();
        let fake_payload =
            B64URL.encode(b"{\"exp\":9999999999,\"server\":\"evil\",\"user_id\":\"hacked\"}");
        let tampered = format!("{}.{}", fake_payload, parts[1]);
        assert!(matches!(
            validate_state(KEY, &tampered),
            Err(McpError::StateInvalid)
        ));
    }

    #[test]
    fn no_dot_separator_rejected() {
        assert!(matches!(
            validate_state(KEY, "nodot"),
            Err(McpError::StateInvalid)
        ));
    }
}
