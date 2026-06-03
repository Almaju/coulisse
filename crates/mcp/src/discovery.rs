//! OAuth Authorization Server Metadata discovery for MCP servers.
//!
//! Two-step flow per RFC 9728 + RFC 8414:
//!
//! 1. Fetch `<mcp_origin>/.well-known/oauth-protected-resource` (RFC 9728
//!    Protected Resource Metadata). If present, it lists one or more
//!    `authorization_servers` — the issuer URLs of the actual auth servers.
//! 2. Fetch `<auth_server>/.well-known/oauth-authorization-server` for the
//!    first listed issuer (RFC 8414 Authorization Server Metadata).
//!
//! Many real-world MCP servers (Todoist, Atlassian) host the MCP endpoint
//! on a different origin than their OAuth authorization server — Todoist's
//! MCP is at `ai.todoist.net` but auth happens on `todoist.com`. Skipping
//! step 1 and hitting `<mcp_origin>/.well-known/oauth-authorization-server`
//! returns 404 on those servers, which is what the pre-fix behaviour did.
//!
//! Fallback: if step 1 fails (404, missing field), try the AS metadata at
//! the MCP origin directly. Some simple servers ARE their own auth server
//! and skip publishing protected-resource metadata.
//!
//! No persistent caching here — the result is folded into the
//! `mcp_oauth_clients` vault row after DCR runs, so subsequent users skip
//! both discovery and registration.

use serde::{Deserialize, Serialize};

use crate::error::McpError;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct AuthMetadata {
    pub(crate) authorization_endpoint: String,
    /// RFC 7591 dynamic client registration endpoint. Required for
    /// `oauth: { mode: discover }` — Coulisse cannot register itself
    /// without it. Static-credential providers can omit this field.
    #[serde(default)]
    pub(crate) registration_endpoint: Option<String>,
    /// Scopes the provider advertises. Used as the fallback when the YAML
    /// `oauth.scopes` override is empty.
    #[serde(default)]
    pub(crate) scopes_supported: Vec<String>,
    pub(crate) token_endpoint: String,
    /// Client authentication methods the token endpoint accepts (RFC
    /// 8414 §2). Empty means the AS didn't publish the list; per RFC
    /// 8414 §2, `client_secret_basic` is the implicit default in that
    /// case. Coulisse prefers `"none"` (public client + PKCE) when
    /// advertised — that's what `mcp-remote` uses, and Todoist's MCP
    /// in particular only seems to accept tokens issued to "none"
    /// clients despite the AS itself accepting confidential ones.
    #[serde(default)]
    pub(crate) token_endpoint_auth_methods_supported: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ProtectedResourceMetadata {
    #[serde(default)]
    authorization_servers: Vec<String>,
    /// Scopes the *resource* (the MCP endpoint) accepts. This is a strict
    /// subset of what the auth server advertises — e.g. Todoist's AS lists
    /// admin/billing scopes the MCP endpoint refuses. When present, this
    /// list is what we should request.
    #[serde(default)]
    scopes_supported: Vec<String>,
}

/// Fetch and parse OAuth metadata for the MCP server.
///
/// `mcp_url` is the full MCP endpoint URL (`https://ai.todoist.net/mcp`).
/// First attempts RFC 9728 protected-resource discovery to find the
/// authorization server issuer + the resource-specific scopes; falls back
/// to assuming the MCP origin is itself the authorization server.
///
/// When the protected-resource metadata declares its own `scopes_supported`,
/// that list **replaces** the one returned by the AS — Todoist's AS lists
/// admin scopes (`dev:app_console`, `billing:*`) the MCP endpoint won't
/// grant, and requesting them yields `invalid_scope` at the consent screen.
///
/// # Errors
///
/// Returns `McpError::Discovery` if the URL is malformed, the request
/// fails, or the response is not valid metadata.
pub(crate) async fn fetch(mcp_url: &str) -> Result<AuthMetadata, McpError> {
    let mcp_origin = origin_of(mcp_url)?;
    let prm = fetch_protected_resource_metadata(&mcp_origin).await;
    let as_issuer = prm
        .as_ref()
        .and_then(|p| p.authorization_servers.first().cloned())
        .unwrap_or_else(|| mcp_origin.clone());
    let mut metadata = fetch_authorization_server_metadata(&as_issuer).await?;
    if let Some(p) = prm
        && !p.scopes_supported.is_empty()
    {
        metadata.scopes_supported = p.scopes_supported;
    }
    Ok(metadata)
}

/// RFC 9728: fetch the MCP origin's protected-resource metadata. Returns
/// `None` if the endpoint is absent or unparseable — callers fall back to
/// assuming the MCP origin doubles as the auth server.
async fn fetch_protected_resource_metadata(mcp_origin: &str) -> Option<ProtectedResourceMetadata> {
    let url = format!("{mcp_origin}/.well-known/oauth-protected-resource");
    let response = reqwest::get(&url).await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json::<ProtectedResourceMetadata>().await.ok()
}

async fn fetch_authorization_server_metadata(issuer: &str) -> Result<AuthMetadata, McpError> {
    let issuer_trimmed = issuer.trim_end_matches('/');
    let url = format!("{issuer_trimmed}/.well-known/oauth-authorization-server");
    let response = reqwest::get(&url)
        .await
        .map_err(|source| McpError::Discovery {
            url: url.clone(),
            source: Box::new(source),
        })?;
    if !response.status().is_success() {
        return Err(McpError::DiscoveryStatus {
            status: response.status().as_u16(),
            url,
        });
    }
    response
        .json::<AuthMetadata>()
        .await
        .map_err(|source| McpError::Discovery {
            url,
            source: Box::new(source),
        })
}

/// Strip path + query + fragment from a URL, returning `scheme://authority`.
/// Used as the well-known origin per RFC 8414 §3.
fn origin_of(url: &str) -> Result<String, McpError> {
    let parsed = reqwest::Url::parse(url).map_err(|_| McpError::DiscoveryInvalidUrl {
        url: url.to_string(),
    })?;
    let scheme = parsed.scheme();
    let authority = parsed
        .host_str()
        .ok_or_else(|| McpError::DiscoveryInvalidUrl {
            url: url.to_string(),
        })?;
    let port_suffix = parsed.port().map_or(String::new(), |p| format!(":{p}"));
    Ok(format!("{scheme}://{authority}{port_suffix}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_strips_path() {
        assert_eq!(
            origin_of("https://ai.todoist.net/mcp").unwrap(),
            "https://ai.todoist.net"
        );
    }

    #[test]
    fn origin_preserves_explicit_port() {
        assert_eq!(
            origin_of("http://localhost:9999/mcp/v1").unwrap(),
            "http://localhost:9999"
        );
    }

    #[test]
    fn origin_strips_query_and_fragment() {
        assert_eq!(
            origin_of("https://example.com/mcp?token=x#frag").unwrap(),
            "https://example.com"
        );
    }

    #[test]
    fn origin_rejects_malformed_url() {
        assert!(matches!(
            origin_of("not a url"),
            Err(McpError::DiscoveryInvalidUrl { .. })
        ));
    }

    /// Real shape served by `https://ai.todoist.net/.well-known/oauth-protected-resource`.
    /// If Todoist tweaks the field name or wrapping, this catches it before users do.
    /// Both the auth-server pointer and the resource-specific scope list must round-trip.
    #[test]
    fn parses_todoist_protected_resource_metadata() {
        let body = r#"{
            "resource": "https://ai.todoist.net/mcp",
            "authorization_servers": ["https://todoist.com"],
            "bearer_methods_supported": ["header"],
            "scopes_supported": ["data:read_write"]
        }"#;
        let prm: ProtectedResourceMetadata = serde_json::from_str(body).unwrap();
        assert_eq!(prm.authorization_servers, vec!["https://todoist.com"]);
        assert_eq!(prm.scopes_supported, vec!["data:read_write"]);
    }

    /// Servers that don't publish protected-resource metadata (Coulisse's
    /// pre-RFC-9728 assumption: MCP origin IS the auth server) must still
    /// work via the fallback. Parsing an empty document should yield no
    /// issuer and no scopes.
    #[test]
    fn empty_authorization_servers_falls_back() {
        let body = r#"{"authorization_servers": []}"#;
        let prm: ProtectedResourceMetadata = serde_json::from_str(body).unwrap();
        assert!(prm.authorization_servers.is_empty());
        assert!(prm.scopes_supported.is_empty());
    }
}
