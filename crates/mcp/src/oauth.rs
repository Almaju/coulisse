// AES-256-GCM authenticated encryption for connect URLs and OAuth state
// tokens. We previously used HMAC over `base64url(json_payload)`, which
// left the `exp`, `server`, and `user_id` fields visible to anyone who
// could base64-decode the URL — including the LLM relaying the URL to
// the user. Claude in particular treats a visible `exp` field as
// permission to "freshen" old URLs from conversation history by bumping
// the timestamp and inventing a new signature, producing a forged URL
// that fails HMAC validation. AES-GCM removes the affordance: the entire
// payload is ciphertext + 16-byte authentication tag, and any
// modification breaks decryption.

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::error::McpError;

/// Wire format: `base64url(nonce[12] || ciphertext || gcm_tag[16])`.
/// Expires after 600 seconds.
pub struct StateToken {
    /// PKCE verifier (RFC 7636) tucked inside the encrypted state so it
    /// survives Todoist's authorize → callback round-trip without ever
    /// touching the URL as cleartext. Present on the state token used in
    /// the authorize redirect; absent on the simpler "the user clicked
    /// the connect link" state token, which doesn't need PKCE because
    /// it never reaches the OAuth provider.
    pub code_verifier: Option<String>,
    pub server: String,
    pub user_id: String,
}

/// PKCE keypair (RFC 7636): random verifier + SHA-256 challenge. Both
/// base64url-encoded without padding. The verifier MUST be passed back
/// in the token exchange; the challenge goes in the authorize URL.
#[must_use]
pub fn pkce_pair() -> (String, String) {
    let mut verifier_bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut verifier_bytes);
    let verifier = B64URL.encode(verifier_bytes);
    let challenge = B64URL.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// Note: the parameter name says `hmac_key` for backwards compatibility
/// with `COULISSE_HMAC_KEY` / `auth.mcp_consumer_secret` plumbing — the
/// key itself (32 random bytes) is suitable as either an HMAC key or an
/// AES-256 key. The function uses it as the latter.
///
/// # Panics
///
/// Panics if `hmac_key` is not exactly 32 bytes. The caller (Coulisse's
/// secrets loader) only invokes this with `COULISSE_HMAC_KEY` material,
/// which is validated to 32 bytes at startup, so this is an invariant
/// violation rather than a runtime concern.
#[must_use]
pub fn generate_state(hmac_key: &[u8], server: &str, user_id: &str) -> String {
    encrypt_state(hmac_key, server, user_id, None)
}

/// Variant that bundles a PKCE verifier into the encrypted payload, used
/// for the `state` parameter that round-trips through the OAuth
/// authorization server. The verifier is needed at the token-exchange
/// step (RFC 7636 §4.5); stashing it in the AES-GCM ciphertext keeps it
/// off the wire as cleartext and avoids needing a server-side store.
#[must_use]
pub fn generate_state_with_pkce(
    hmac_key: &[u8],
    server: &str,
    user_id: &str,
    code_verifier: &str,
) -> String {
    encrypt_state(hmac_key, server, user_id, Some(code_verifier))
}

fn encrypt_state(
    hmac_key: &[u8],
    server: &str,
    user_id: &str,
    code_verifier: Option<&str>,
) -> String {
    let exp = coulisse_core::now_secs() + 600;
    let mut payload = serde_json::json!({
        "exp": exp,
        "server": server,
        "user_id": user_id,
    });
    if let Some(v) = code_verifier {
        payload["code_verifier"] = serde_json::Value::String(v.to_string());
    }
    let cipher = Aes256Gcm::new_from_slice(hmac_key).expect("AES-256 requires a 32-byte key");
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, payload.to_string().as_bytes())
        .expect("AES-GCM encrypt never fails for plaintext < 64 GiB");
    let mut blob = nonce.to_vec();
    blob.extend_from_slice(&ciphertext);
    B64URL.encode(&blob)
}

/// Decrypt and validate a state token. Returns `McpError::StateInvalid`
/// if the token doesn't decrypt cleanly (wrong key, mutated bytes, bad
/// shape), `McpError::StateExpired` if it decrypts but is past its
/// `exp`.
///
/// # Errors
///
/// Returns an error if the state token is invalid or expired.
pub fn validate_state(hmac_key: &[u8], token: &str) -> Result<StateToken, McpError> {
    let blob = B64URL.decode(token).map_err(|_| McpError::StateInvalid)?;
    if blob.len() < 12 {
        return Err(McpError::StateInvalid);
    }
    let mut nonce_arr = [0u8; 12];
    nonce_arr.copy_from_slice(&blob[..12]);
    let ciphertext = &blob[12..];
    #[allow(deprecated)]
    let nonce = aes_gcm::aead::generic_array::GenericArray::from(nonce_arr);
    let cipher = Aes256Gcm::new_from_slice(hmac_key).map_err(|_| McpError::StateInvalid)?;
    let plaintext = cipher
        .decrypt(&nonce, ciphertext)
        .map_err(|_| McpError::StateInvalid)?;
    let payload: serde_json::Value =
        serde_json::from_slice(&plaintext).map_err(|_| McpError::StateInvalid)?;
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
    let code_verifier = payload
        .get("code_verifier")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Ok(StateToken {
        code_verifier,
        server,
        user_id,
    })
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
        assert!(
            state.code_verifier.is_none(),
            "no PKCE expected from generate_state"
        );
    }

    /// PKCE verifier round-trip: a state minted with a verifier must
    /// decrypt back to the same verifier. This is the path the callback
    /// handler takes — without it, the token exchange would lose the
    /// verifier mid-flow and fail.
    #[test]
    fn round_trip_state_carries_pkce_verifier() {
        let token = generate_state_with_pkce(KEY, "github", "user-42", "the-verifier-xyz");
        let state = validate_state(KEY, &token).unwrap();
        assert_eq!(state.code_verifier.as_deref(), Some("the-verifier-xyz"));
    }

    /// Any modification to the ciphertext breaks the GCM tag — this is
    /// exactly the property we needed (and didn't have under HMAC, where
    /// the payload was visible in cleartext).
    #[test]
    fn appended_bytes_rejected() {
        let token = generate_state(KEY, "github", "user-42");
        let tampered = format!("{token}AB");
        assert!(matches!(
            validate_state(KEY, &tampered),
            Err(McpError::StateInvalid)
        ));
    }

    /// Substituting a single base64 character (the LLM's "freshen the
    /// exp field" failure mode) yields an invalid token. Under the old
    /// HMAC scheme this would also fail — but only because the model
    /// can't sign; here the payload itself is unreadable, removing the
    /// affordance.
    #[test]
    fn substituted_char_rejected() {
        let token = generate_state(KEY, "github", "user-42");
        // Flip one character in the middle of the token.
        let mut chars: Vec<char> = token.chars().collect();
        let mid = chars.len() / 2;
        chars[mid] = if chars[mid] == 'A' { 'B' } else { 'A' };
        let tampered: String = chars.into_iter().collect();
        assert!(matches!(
            validate_state(KEY, &tampered),
            Err(McpError::StateInvalid)
        ));
    }

    /// Wrong key — same payload encrypted under a different secret must
    /// not decrypt with this one.
    #[test]
    fn wrong_key_rejected() {
        let other_key: &[u8] = b"other-test-key-32-bytes-padding!";
        let token = generate_state(other_key, "github", "user-42");
        assert!(matches!(
            validate_state(KEY, &token),
            Err(McpError::StateInvalid)
        ));
    }

    /// Malformed (not base64url at all) input must produce `StateInvalid`,
    /// not panic.
    #[test]
    fn garbage_input_rejected() {
        assert!(matches!(
            validate_state(KEY, "!!!not base64!!!"),
            Err(McpError::StateInvalid)
        ));
    }

    /// Payload too short to contain a nonce.
    #[test]
    fn short_input_rejected() {
        let tiny = B64URL.encode(b"abc");
        assert!(matches!(
            validate_state(KEY, &tiny),
            Err(McpError::StateInvalid)
        ));
    }

    /// Each call must produce a different ciphertext (fresh nonce) even
    /// for identical input — guarantees the URL isn't replayable across
    /// users who happen to share `(server, user_id, exp)`.
    #[test]
    fn each_token_uses_a_fresh_nonce() {
        let a = generate_state(KEY, "github", "user-42");
        let b = generate_state(KEY, "github", "user-42");
        assert_ne!(a, b);
    }
}
