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
use base64::engine::general_purpose::STANDARD as B64;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use coulisse_core::UserId;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::McpError;
use crate::vault::StoredToken;

/// Lifetime of a state token, in seconds.
const STATE_TTL_SECS: u64 = 600;

macro_rules! secret_newtype {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            #[must_use]
            pub fn new(raw: impl Into<String>) -> Self {
                Self(raw.into())
            }

            #[must_use]
            pub fn expose(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(concat!(stringify!($name), "([redacted])"))
            }
        }
    };
}

macro_rules! public_newtype {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            #[must_use]
            pub fn new(raw: impl Into<String>) -> Self {
                Self(raw.into())
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

secret_newtype!(
    /// Bearer token an MCP endpoint accepts on behalf of one user.
    AccessToken
);
secret_newtype!(
    /// Secret issued to Coulisse as a confidential OAuth client. Absent for
    /// public (PKCE-only) clients.
    ClientSecret
);
secret_newtype!(
    /// PKCE verifier (RFC 7636): proves at the token endpoint that the
    /// same party started the authorize request.
    CodeVerifier
);
secret_newtype!(
    /// Long-lived credential exchanged for a fresh `AccessToken` once the
    /// current one expires.
    RefreshToken
);

public_newtype!(
    /// Identifier the authorization server assigned to this Coulisse
    /// instance.
    ClientId
);
public_newtype!(
    /// PKCE challenge (RFC 7636): base64url(SHA-256(verifier)), sent in
    /// the authorize URL.
    CodeChallenge
);
public_newtype!(
    /// Where the authorization server sends the browser back after the
    /// user consents. Must match what Coulisse registered.
    RedirectUri
);

/// PKCE keypair (RFC 7636): random verifier + SHA-256 challenge. Both
/// base64url-encoded without padding. The verifier MUST be passed back
/// in the token exchange; the challenge goes in the authorize URL.
pub struct PkcePair {
    pub challenge: CodeChallenge,
    pub verifier: CodeVerifier,
}

impl PkcePair {
    #[must_use]
    pub fn generate() -> Self {
        let mut verifier_bytes = [0u8; 32];
        // rabot: allow(ambient-randomness) cryptographic secret: a replayable generator would be a vulnerability
        rand::rng().fill_bytes(&mut verifier_bytes);
        let verifier = B64URL.encode(verifier_bytes);
        let challenge = B64URL.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            challenge: CodeChallenge::new(challenge),
            verifier: CodeVerifier::new(verifier),
        }
    }
}

/// AES-256 key that seals connect links and OAuth `state` parameters.
///
/// Note: the config plumbing calls this material `hmac_key`
/// (`COULISSE_HMAC_KEY` / `auth.mcp_consumer_secret`) for backwards
/// compatibility — 32 random bytes are suitable as either an HMAC key or
/// an AES-256 key, and this type uses them as the latter.
#[derive(Clone)]
pub struct StateKey(Aes256Gcm);

impl StateKey {
    /// Decode a base64-encoded 32-byte key.
    ///
    /// # Errors
    ///
    /// Returns `McpError::StateKeyInvalid` if the key is not valid base64
    /// or not exactly 32 bytes after decoding.
    pub fn from_base64(key_b64: &str) -> Result<Self, McpError> {
        let key_bytes = B64
            .decode(key_b64.trim())
            .map_err(|source| McpError::StateKeyInvalid {
                source: Box::new(source),
            })?;
        let cipher =
            Aes256Gcm::new_from_slice(&key_bytes).map_err(|source| McpError::StateKeyInvalid {
                source: Box::new(source),
            })?;
        Ok(Self(cipher))
    }
}

impl std::fmt::Debug for StateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StateKey([redacted])")
    }
}

/// Wire format: `base64url(nonce[12] || ciphertext || gcm_tag[16])`.
/// Expires after 600 seconds.
pub struct StateToken {
    /// PKCE verifier (RFC 7636) tucked inside the encrypted state so it
    /// survives Todoist's authorize → callback round-trip without ever
    /// touching the URL as cleartext. Present on the state token used in
    /// the authorize redirect; absent on the simpler "the user clicked
    /// the connect link" state token, which doesn't need PKCE because
    /// it never reaches the OAuth provider.
    pub code_verifier: Option<CodeVerifier>,
    pub server: String,
    pub user_id: UserId,
}

#[derive(Deserialize, Serialize)]
struct StatePayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    code_verifier: Option<CodeVerifier>,
    exp: u64,
    server: String,
    user_id: UserId,
}

impl StateToken {
    /// Decrypt and validate a state token. Returns `McpError::StateInvalid`
    /// if the token doesn't decrypt cleanly (wrong key, mutated bytes, bad
    /// shape), `McpError::StateExpired` if it decrypts but is past its
    /// `exp`.
    ///
    /// # Errors
    ///
    /// Returns an error if the state token is invalid or expired.
    pub fn decrypt(key: &StateKey, token: &str) -> Result<Self, McpError> {
        let blob = B64URL
            .decode(token)
            .map_err(|source| McpError::StateInvalid {
                source: Box::new(source),
            })?;
        let nonce_arr: [u8; 12] =
            blob.get(..12)
                .and_then(|b| b.try_into().ok())
                .ok_or_else(|| McpError::StateInvalid {
                    source: "token shorter than the 12-byte nonce".into(),
                })?;
        let ciphertext = &blob[12..];
        #[allow(deprecated)]
        let nonce = aes_gcm::aead::generic_array::GenericArray::from(nonce_arr);
        let plaintext =
            key.0
                .decrypt(&nonce, ciphertext)
                .map_err(|source| McpError::StateInvalid {
                    source: Box::new(source),
                })?;
        let payload: StatePayload =
            serde_json::from_slice(&plaintext).map_err(|source| McpError::StateInvalid {
                source: Box::new(source),
            })?;
        if coulisse_core::now_secs() > payload.exp {
            return Err(McpError::StateExpired);
        }
        Ok(Self {
            code_verifier: payload.code_verifier,
            server: payload.server,
            user_id: payload.user_id,
        })
    }

    /// Seal this token under `key`, stamping it with a 600-second expiry.
    ///
    /// # Errors
    ///
    /// Returns `McpError::Encrypt` if AES-GCM refuses the payload.
    pub fn encrypt(&self, key: &StateKey) -> Result<String, McpError> {
        let payload = StatePayload {
            code_verifier: self.code_verifier.clone(),
            exp: coulisse_core::now_secs() + STATE_TTL_SECS,
            server: self.server.clone(),
            user_id: self.user_id,
        };
        let plaintext = serde_json::to_vec(&payload).map_err(|source| McpError::StateInvalid {
            source: Box::new(source),
        })?;
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext =
            key.0
                .encrypt(&nonce, plaintext.as_slice())
                .map_err(|err| McpError::Encrypt {
                    err,
                    server: self.server.clone(),
                })?;
        let mut blob = nonce.to_vec();
        blob.extend_from_slice(&ciphertext);
        Ok(B64URL.encode(&blob))
    }
}

/// Successful reply from an OAuth token endpoint (RFC 6749 §5.1), for
/// both the authorization-code exchange and the refresh-token grant.
#[derive(Deserialize)]
pub(crate) struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    refresh_token: Option<String>,
}

impl TokenResponse {
    /// Convert `expires_in` (relative) to an absolute `expires_at`. RFC
    /// 6749 §6: on refresh the AS MAY issue a new refresh token. If it
    /// does (rotation), use it; if not, keep `fallback_refresh` — the
    /// one that just succeeded.
    pub(crate) fn into_stored_token(self, fallback_refresh: Option<RefreshToken>) -> StoredToken {
        let expires_at = self
            .expires_in
            .map(|secs| coulisse_core::u64_to_i64(coulisse_core::now_secs() + secs));
        StoredToken {
            access_token: AccessToken::new(self.access_token),
            expires_at,
            refresh_token: self
                .refresh_token
                .map(RefreshToken::new)
                .or(fallback_refresh),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"test-hmac-key-32-bytes-padding!!";

    fn key() -> StateKey {
        StateKey::from_base64(&B64.encode(KEY)).unwrap()
    }

    fn state_for(user_id: UserId) -> StateToken {
        StateToken {
            code_verifier: None,
            server: "github".to_string(),
            user_id,
        }
    }

    #[test]
    fn round_trip_valid_token() {
        let user_id = UserId::new();
        let token = state_for(user_id).encrypt(&key()).unwrap();
        let state = StateToken::decrypt(&key(), &token).unwrap();
        assert_eq!(state.server, "github");
        assert_eq!(state.user_id, user_id);
        assert!(
            state.code_verifier.is_none(),
            "no PKCE expected without a verifier"
        );
    }

    /// PKCE verifier round-trip: a state minted with a verifier must
    /// decrypt back to the same verifier. This is the path the callback
    /// handler takes — without it, the token exchange would lose the
    /// verifier mid-flow and fail.
    #[test]
    fn round_trip_state_carries_pkce_verifier() {
        let state = StateToken {
            code_verifier: Some(CodeVerifier::new("the-verifier-xyz")),
            ..state_for(UserId::new())
        };
        let token = state.encrypt(&key()).unwrap();
        let state = StateToken::decrypt(&key(), &token).unwrap();
        assert_eq!(
            state.code_verifier.as_ref().map(CodeVerifier::expose),
            Some("the-verifier-xyz")
        );
    }

    /// Any modification to the ciphertext breaks the GCM tag — this is
    /// exactly the property we needed (and didn't have under HMAC, where
    /// the payload was visible in cleartext).
    #[test]
    fn appended_bytes_rejected() {
        let token = state_for(UserId::new()).encrypt(&key()).unwrap();
        let tampered = format!("{token}AB");
        assert!(matches!(
            StateToken::decrypt(&key(), &tampered),
            Err(McpError::StateInvalid { .. })
        ));
    }

    /// Substituting a single base64 character (the LLM's "freshen the
    /// exp field" failure mode) yields an invalid token. Under the old
    /// HMAC scheme this would also fail — but only because the model
    /// can't sign; here the payload itself is unreadable, removing the
    /// affordance.
    #[test]
    fn substituted_char_rejected() {
        let token = state_for(UserId::new()).encrypt(&key()).unwrap();
        // Flip one character in the middle of the token.
        let mut chars: Vec<char> = token.chars().collect();
        let mid = chars.len() / 2;
        chars[mid] = if chars[mid] == 'A' { 'B' } else { 'A' };
        let tampered: String = chars.into_iter().collect();
        assert!(matches!(
            StateToken::decrypt(&key(), &tampered),
            Err(McpError::StateInvalid { .. })
        ));
    }

    /// Wrong key — same payload encrypted under a different secret must
    /// not decrypt with this one.
    #[test]
    fn wrong_key_rejected() {
        let other_key =
            StateKey::from_base64(&B64.encode(b"other-test-key-32-bytes-padding!")).unwrap();
        let token = state_for(UserId::new()).encrypt(&other_key).unwrap();
        assert!(matches!(
            StateToken::decrypt(&key(), &token),
            Err(McpError::StateInvalid { .. })
        ));
    }

    /// Malformed (not base64url at all) input must produce `StateInvalid`,
    /// not panic.
    #[test]
    fn garbage_input_rejected() {
        assert!(matches!(
            StateToken::decrypt(&key(), "!!!not base64!!!"),
            Err(McpError::StateInvalid { .. })
        ));
    }

    /// Payload too short to contain a nonce.
    #[test]
    fn short_input_rejected() {
        let tiny = B64URL.encode(b"abc");
        assert!(matches!(
            StateToken::decrypt(&key(), &tiny),
            Err(McpError::StateInvalid { .. })
        ));
    }

    /// Each call must produce a different ciphertext (fresh nonce) even
    /// for identical input — guarantees the URL isn't replayable across
    /// users who happen to share `(server, user_id, exp)`.
    #[test]
    fn each_token_uses_a_fresh_nonce() {
        let user_id = UserId::new();
        let a = state_for(user_id).encrypt(&key()).unwrap();
        let b = state_for(user_id).encrypt(&key()).unwrap();
        assert_ne!(a, b);
    }

    /// A key of the wrong length is rejected up front, not at the first
    /// encrypt.
    #[test]
    fn short_key_rejected() {
        assert!(matches!(
            StateKey::from_base64(&B64.encode([0u8; 16])),
            Err(McpError::StateKeyInvalid { .. })
        ));
    }

    /// `PkcePair::generate` must produce a verifier whose SHA-256
    /// base64url-encodes to the returned challenge. RFC 7636 §4.2.
    #[test]
    fn pkce_pair_challenge_is_sha256_of_verifier() {
        let pair = PkcePair::generate();
        let expected = B64URL.encode(Sha256::digest(pair.verifier.expose().as_bytes()));
        assert_eq!(pair.challenge.as_str(), expected);
        // Verifier must satisfy RFC 7636 length requirement: 43–128 chars.
        assert!(
            (43..=128).contains(&pair.verifier.expose().len()),
            "verifier length out of RFC 7636 range: {}",
            pair.verifier.expose().len()
        );
    }
}
