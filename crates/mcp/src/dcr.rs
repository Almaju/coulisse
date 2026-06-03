//! RFC 7591 Dynamic Client Registration.
//!
//! Run once per `oauth: discover` MCP server, the first time any user
//! triggers a connect link for it. Coulisse POSTs its own metadata to the
//! provider's `registration_endpoint`, the provider responds with a
//! `client_id` (and sometimes `client_secret`), and we cache the result in
//! `mcp_oauth_clients` for every subsequent user.
//!
//! Coulisse-wide registration — one row per server, reused across users.
//! The `client_id` identifies the Coulisse instance, not the end user; each
//! user's access token is bound to their own provider account via the
//! authorization-code dance, not via separate client registrations.

use serde::{Deserialize, Serialize};

use crate::discovery::AuthMetadata;
use crate::error::McpError;

#[derive(Debug)]
pub(crate) struct ClientRegistration {
    pub(crate) client_id: String,
    pub(crate) client_secret: Option<String>,
    /// Scopes the AS told this client it can request, parsed from RFC
    /// 7591 §3.2.1's space-separated `scope` field. Atlassian-style
    /// servers that don't publish `scopes_supported` in their AS
    /// metadata still echo a default scope set here, which is how
    /// `mcp-remote` learns what to ask for. `None` means the response
    /// omitted `scope` entirely.
    pub(crate) scopes: Option<Vec<String>>,
}

#[derive(Serialize)]
struct RegistrationRequest<'a> {
    client_name: &'static str,
    /// Public-facing URL the provider can show on the consent screen.
    /// Some providers (Todoist's MCP layer) appear to gate token
    /// acceptance on this field matching a recognised client identity.
    client_uri: &'static str,
    grant_types: &'static [&'static str],
    redirect_uris: [&'a str; 1],
    response_types: &'static [&'static str],
    token_endpoint_auth_method: &'static str,
}

#[derive(Deserialize)]
struct RegistrationResponse {
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
    /// RFC 7591 §3.2.1: "String containing a space-separated list of
    /// scope values (as described in Section 3.3 of [RFC6749]) that the
    /// client can use when requesting access tokens." Optional.
    #[serde(default)]
    scope: Option<String>,
}

/// Register Coulisse as an OAuth client at the discovered registration
/// endpoint, asking only for what the per-user authorization-code flow
/// needs. Returns the credentials the provider issued; the caller stores
/// them in the vault keyed by server name.
///
/// # Errors
///
/// Returns `McpError::DcrUnsupported` if the metadata doesn't advertise a
/// registration endpoint (no DCR possible — the server must use
/// `oauth: static` instead), or `McpError::DynamicClientRegistration` if
/// the HTTP exchange fails.
pub(crate) async fn register(
    server_name: &str,
    metadata: &AuthMetadata,
    redirect_uri: &str,
) -> Result<ClientRegistration, McpError> {
    let endpoint =
        metadata
            .registration_endpoint
            .as_deref()
            .ok_or_else(|| McpError::DcrUnsupported {
                server: server_name.to_string(),
            })?;

    // Prefer registering as a public client (PKCE-only, no secret) when the
    // AS supports `"none"` as a token endpoint auth method. That's what
    // `mcp-remote` does and it's the MCP OAuth 2.1 recommended pattern for
    // local clients like Coulisse. We learned the hard way that some MCP
    // resource servers (Todoist's MCP in particular) accept the OAuth
    // dance for both public and confidential clients but only honour
    // tokens issued to public ones. When the AS doesn't advertise `none`,
    // fall back to `client_secret_post` so providers that strictly require
    // a secret still work.
    let auth_method = if metadata
        .token_endpoint_auth_methods_supported
        .iter()
        .any(|m| m == "none")
        || metadata.token_endpoint_auth_methods_supported.is_empty()
    {
        "none"
    } else {
        "client_secret_post"
    };
    let body = RegistrationRequest {
        client_name: "Coulisse",
        client_uri: "https://github.com/Almaju/coulisse",
        grant_types: &["authorization_code", "refresh_token"],
        redirect_uris: [redirect_uri],
        response_types: &["code"],
        token_endpoint_auth_method: auth_method,
    };

    let client = reqwest::Client::new();
    let response = client
        .post(endpoint)
        .json(&body)
        .send()
        .await
        .map_err(|source| McpError::DynamicClientRegistration {
            server: server_name.to_string(),
            source: Box::new(source),
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(McpError::DynamicClientRegistration {
            server: server_name.to_string(),
            source: format!("HTTP {status}: {body}").into(),
        });
    }

    let parsed: RegistrationResponse =
        response
            .json()
            .await
            .map_err(|source| McpError::DynamicClientRegistration {
                server: server_name.to_string(),
                source: Box::new(source),
            })?;

    let scopes = parsed
        .scope
        .map(|s| s.split_whitespace().map(str::to_string).collect());

    Ok(ClientRegistration {
        client_id: parsed.client_id,
        client_secret: parsed.client_secret,
        scopes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7591 §3.2.1: registration response carries scopes the client
    /// may request. Atlassian's `cf.mcp.atlassian.com/v1/register` echoes
    /// the granted scope set here even though `scopes_supported` on the
    /// AS metadata is empty. Parsing this is what lets Coulisse skip
    /// manual `oauth.scopes` config for spec-compliant servers.
    #[test]
    fn parses_space_separated_scopes_from_registration_response() {
        let body = r#"{
            "client_id": "abc",
            "client_secret": null,
            "scope": "read:jira-work write:jira-work offline_access"
        }"#;
        let parsed: RegistrationResponse = serde_json::from_str(body).unwrap();
        assert_eq!(
            parsed.scope.as_deref(),
            Some("read:jira-work write:jira-work offline_access")
        );
    }

    /// Response without a `scope` field must round-trip without error
    /// (Todoist and many providers omit it).
    #[test]
    fn missing_scope_field_is_none() {
        let body = r#"{"client_id":"abc"}"#;
        let parsed: RegistrationResponse = serde_json::from_str(body).unwrap();
        assert!(parsed.scope.is_none());
        assert!(parsed.client_secret.is_none());
    }

    /// Mirrors the selection rule in `register()` — kept in sync so a
    /// future change to the logic is forced through the tests. Public-
    /// client (PKCE-only) is preferred when the AS supports it, matching
    /// what mcp-remote does and what made Todoist's MCP accept the
    /// resulting tokens.
    fn pick_auth_method(supported: &[&str]) -> &'static str {
        if supported.iter().any(|m| *m == "none") || supported.is_empty() {
            "none"
        } else {
            "client_secret_post"
        }
    }

    #[test]
    fn prefers_none_when_advertised() {
        assert_eq!(
            pick_auth_method(&["client_secret_basic", "client_secret_post", "none"]),
            "none"
        );
        assert_eq!(pick_auth_method(&["none"]), "none");
    }

    /// When the AS publishes a list but doesn't include `none`, fall
    /// back to `client_secret_post` so providers that strictly require
    /// confidential clients still work.
    #[test]
    fn falls_back_to_client_secret_post_when_none_not_advertised() {
        assert_eq!(
            pick_auth_method(&["client_secret_basic", "client_secret_post"]),
            "client_secret_post"
        );
    }

    /// Empty list means the AS didn't advertise the field. Pre-RFC-8414
    /// servers fall here. Coulisse treats this the MCP-aligned way —
    /// register as public — because every modern MCP server supports
    /// it, and the failure mode for confidential registration on a
    /// server that prefers public clients is far worse (silent 401s
    /// from the resource server, hard to debug) than the reverse.
    #[test]
    fn empty_list_defaults_to_none() {
        assert_eq!(pick_auth_method(&[]), "none");
    }
}
