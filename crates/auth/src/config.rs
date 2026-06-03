use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Two-scope auth configuration. Each scope is optional: unset means the
/// corresponding surface (`/v1/*` for `proxy`, `/admin/*` for `admin`) is
/// served unauthenticated. Both scopes accept the same shape, so a single
/// pair of credentials can guard both by repeating the block.
#[derive(Clone, Debug, Default, Deserialize, schemars::JsonSchema, Serialize)]
#[schemars(rename = "AuthConfig")]
pub struct Config {
    #[serde(default)]
    pub admin: Option<ScopeConfig>,
    /// Bearer token that guards the `/mcp-admin` MCP server endpoint.
    /// Distinct from `auth.admin` (HTTP Basic / OIDC studio) — this is a
    /// shared secret for IDE clients (Claude Code, Cursor) that use the
    /// Model Context Protocol to inspect and manage a deployed Coulisse
    /// instance.
    #[serde(default)]
    pub mcp_admin: Option<McpAdminConfig>,
    /// Shared secret that guards `POST /mcp/{server}/connect-link`.
    /// Required when any MCP server declares an `oauth:` block.
    #[serde(default)]
    pub mcp_consumer_secret: Option<String>,
    #[serde(default)]
    pub proxy: Option<ScopeConfig>,
}

/// Bearer-token auth for the MCP admin endpoint.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
pub struct McpAdminConfig {
    pub token: String,
}

/// One scope's auth method. Exactly one of `basic`, `oidc`, or `tokens`
/// must be set when the scope block is present — they are mutually
/// exclusive so the server never has to choose between competing schemes.
/// `tokens` is valid on the `proxy` scope only.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
pub struct ScopeConfig {
    #[serde(default)]
    pub basic: Option<BasicConfig>,
    /// How the per-user identity for `/v1/*` requests is derived. Only
    /// meaningful on the `proxy` scope — the `admin` surface has no
    /// per-user partitioning, so setting anything but the default here is
    /// rejected at startup.
    #[serde(default)]
    pub identity: IdentityMode,
    #[serde(default)]
    pub oidc: Option<OidcConfig>,
    /// Self-issued API tokens (`sk-coulisse-…`). Write `tokens: {}` to turn
    /// the scheme on; tokens themselves are minted at runtime via the studio
    /// or `coulisse token create`. Each token binds the request to its own
    /// principal, so it implies credential-bound identity regardless of the
    /// `identity` field.
    #[serde(default)]
    pub tokens: Option<TokensConfig>,
}

/// Marker for the self-issued-token auth scheme. Empty today — its presence
/// is the switch — but a struct so future per-scheme options (custom header,
/// default budget) have a home without a YAML break.
#[derive(Clone, Debug, Default, Deserialize, schemars::JsonSchema, Serialize)]
pub struct TokensConfig {}

/// Where the user identifier that partitions memory, recall, MCP sessions,
/// and rate limits comes from.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, schemars::JsonSchema, PartialEq, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum IdentityMode {
    /// Derive the identity from the authenticated principal — the Basic
    /// username or the OIDC `sub` claim. A request body that claims a
    /// different `safety_identifier` is rejected. Required for adversarial
    /// multi-tenant serving, where clients cannot be trusted to declare
    /// their own identity honestly.
    FromCredential,
    /// Trust the `safety_identifier` (or deprecated `user`) field in the
    /// request body. The default: correct for single-user setups and for
    /// trusted first-party backends that set the identifier on behalf of
    /// their own authenticated users.
    #[default]
    FromRequest,
}

/// Static HTTP Basic credentials. Appropriate for local dev or a
/// single-operator deployment. Browsers prompt via the native login
/// dialog; no session state.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
pub struct BasicConfig {
    pub password: String,
    #[serde(default = "default_username")]
    pub username: String,
}

/// OIDC (`OpenID` Connect) login. Validated against any compliant `IdP` —
/// Authentik, Keycloak, Auth0, Google, Microsoft, Okta. Access control
/// (who may use the surface) is delegated to the `IdP`'s application
/// bindings, not configured here.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
pub struct OidcConfig {
    pub client_id: String,
    /// Optional for public clients that use PKCE only. Authentik's default
    /// "confidential" client type requires a secret.
    #[serde(default)]
    pub client_secret: Option<String>,
    /// OIDC issuer URL. For Authentik, typically
    /// `https://authentik.example.com/application/o/<app-slug>/`.
    pub issuer_url: String,
    /// Absolute URL the `IdP` will redirect to after login. Must be
    /// whitelisted in the `IdP`'s client config and match a route served by
    /// Coulisse inside the protected scope.
    pub redirect_url: String,
    /// Additional `OAuth2` scopes beyond the implicit `openid`. Defaults to
    /// `profile` and `email`.
    #[serde(default = "default_oidc_scopes")]
    pub scopes: Vec<String>,
}

impl Config {
    /// Validate that any present scope declares exactly one auth method
    /// and that all required fields are non-empty. Cli calls this at
    /// startup as part of the cross-feature validation pass.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying operation fails.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if let Some(scope) = &self.admin {
            scope.validate("admin")?;
            if scope.identity == IdentityMode::FromCredential {
                return Err(ConfigError::IdentityOnAdminScope);
            }
            if scope.tokens.is_some() {
                return Err(ConfigError::TokensOnAdminScope);
            }
        }
        if let Some(mcp_admin) = &self.mcp_admin
            && mcp_admin.token.trim().is_empty()
        {
            return Err(ConfigError::BlankMcpAdminToken);
        }
        if let Some(scope) = &self.proxy {
            scope.validate("proxy")?;
        }
        if let Some(secret) = &self.mcp_consumer_secret
            && secret.trim().is_empty()
        {
            return Err(ConfigError::BlankMcpConsumerSecret);
        }
        Ok(())
    }
}

impl ScopeConfig {
    fn validate(&self, scope: &'static str) -> Result<(), ConfigError> {
        let declared = usize::from(self.basic.is_some())
            + usize::from(self.oidc.is_some())
            + usize::from(self.tokens.is_some());
        match declared {
            0 => return Err(ConfigError::ScopeWithoutAuth(scope)),
            1 => {}
            _ => return Err(ConfigError::ScopeMultipleAuthMethods(scope)),
        }
        if let Some(oidc) = &self.oidc {
            if oidc.client_id.is_empty() {
                return Err(ConfigError::BlankOidcField {
                    scope,
                    field: "client_id",
                });
            }
            if oidc.issuer_url.is_empty() {
                return Err(ConfigError::BlankOidcField {
                    scope,
                    field: "issuer_url",
                });
            }
            if oidc.redirect_url.is_empty() {
                return Err(ConfigError::BlankOidcField {
                    scope,
                    field: "redirect_url",
                });
            }
        }
        if let Some(basic) = &self.basic {
            if basic.password.is_empty() {
                return Err(ConfigError::BlankBasicField {
                    scope,
                    field: "password",
                });
            }
            if basic.username.is_empty() {
                return Err(ConfigError::BlankBasicField {
                    scope,
                    field: "username",
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("auth.{scope}.basic.{field} must be non-empty")]
    BlankBasicField {
        field: &'static str,
        scope: &'static str,
    },
    #[error("auth.mcp_admin.token must be non-empty when set")]
    BlankMcpAdminToken,
    #[error("auth.mcp_consumer_secret must be non-empty when set")]
    BlankMcpConsumerSecret,
    #[error("auth.{scope}.oidc.{field} must be non-empty")]
    BlankOidcField {
        field: &'static str,
        scope: &'static str,
    },
    #[error(
        "auth.admin.identity: `from_credential` is only valid on the proxy scope — the admin surface has no per-user partitioning"
    )]
    IdentityOnAdminScope,
    #[error(
        "auth.{0} block must declare exactly one of `basic`, `oidc`, or `tokens` (remove the extras)"
    )]
    ScopeMultipleAuthMethods(&'static str),
    #[error(
        "auth.{0} block must declare one of `basic`, `oidc`, or `tokens` (or remove the block to disable auth)"
    )]
    ScopeWithoutAuth(&'static str),
    #[error(
        "auth.admin.tokens: self-issued tokens are only valid on the proxy scope — the admin surface has no per-user partitioning"
    )]
    TokensOnAdminScope,
}

fn default_username() -> String {
    "admin".to_string()
}

fn default_oidc_scopes() -> Vec<String> {
    vec!["email".to_string(), "profile".to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basic_scope(identity: IdentityMode) -> ScopeConfig {
        ScopeConfig {
            basic: Some(BasicConfig {
                password: "pw".to_string(),
                username: "gateway".to_string(),
            }),
            identity,
            oidc: None,
            tokens: None,
        }
    }

    #[test]
    fn identity_defaults_to_from_request() {
        assert_eq!(IdentityMode::default(), IdentityMode::FromRequest);
    }

    #[test]
    fn from_credential_on_proxy_is_valid() {
        let config = Config {
            proxy: Some(basic_scope(IdentityMode::FromCredential)),
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn from_credential_on_admin_is_rejected() {
        let config = Config {
            admin: Some(basic_scope(IdentityMode::FromCredential)),
            ..Default::default()
        };
        assert!(matches!(
            config.validate(),
            Err(ConfigError::IdentityOnAdminScope)
        ));
    }
}
