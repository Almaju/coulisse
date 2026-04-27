use serde::Deserialize;
use thiserror::Error;

/// Two-scope auth configuration. Each scope is optional: unset means the
/// corresponding surface (`/v1/*` for `proxy`, `/admin/*` for `admin`) is
/// served unauthenticated. Both scopes accept the same shape, so a single
/// pair of credentials can guard both by repeating the block.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub admin: Option<ScopeConfig>,
    #[serde(default)]
    pub proxy: Option<ScopeConfig>,
}

/// One scope's auth method. Exactly one of `basic` or `oidc` must be set
/// when the scope block is present — they are mutually exclusive so the
/// server never has to choose between two competing session schemes.
#[derive(Clone, Debug, Deserialize)]
pub struct ScopeConfig {
    #[serde(default)]
    pub basic: Option<BasicConfig>,
    #[serde(default)]
    pub oidc: Option<OidcConfig>,
}

/// Static HTTP Basic credentials. Appropriate for local dev or a
/// single-operator deployment. Browsers prompt via the native login
/// dialog; no session state.
#[derive(Clone, Debug, Deserialize)]
pub struct BasicConfig {
    pub password: String,
    #[serde(default = "default_username")]
    pub username: String,
}

/// OIDC (`OpenID` Connect) login. Validated against any compliant `IdP` —
/// Authentik, Keycloak, Auth0, Google, Microsoft, Okta. Access control
/// (who may use the surface) is delegated to the `IdP`'s application
/// bindings, not configured here.
#[derive(Clone, Debug, Deserialize)]
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
        }
        if let Some(scope) = &self.proxy {
            scope.validate("proxy")?;
        }
        Ok(())
    }
}

impl ScopeConfig {
    fn validate(&self, scope: &'static str) -> Result<(), ConfigError> {
        match (&self.basic, &self.oidc) {
            (None, None) => Err(ConfigError::ScopeWithoutAuth(scope)),
            (Some(_), Some(_)) => Err(ConfigError::ScopeBothAuthMethods(scope)),
            (Some(basic), None) => {
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
                Ok(())
            }
            (None, Some(oidc)) => {
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
                Ok(())
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("auth.{scope}.basic.{field} must be non-empty")]
    BlankBasicField {
        field: &'static str,
        scope: &'static str,
    },
    #[error("auth.{scope}.oidc.{field} must be non-empty")]
    BlankOidcField {
        field: &'static str,
        scope: &'static str,
    },
    #[error("auth.{0} block must declare exactly one of `basic` or `oidc`, not both (remove one)")]
    ScopeBothAuthMethods(&'static str),
    #[error(
        "auth.{0} block must declare one of `basic` or `oidc` (or remove the block to disable auth)"
    )]
    ScopeWithoutAuth(&'static str),
}

fn default_username() -> String {
    "admin".to_string()
}

fn default_oidc_scopes() -> Vec<String> {
    vec!["email".to_string(), "profile".to_string()]
}
