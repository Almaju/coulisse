use serde::Deserialize;

/// Authentication for the studio UI and its JSON API. Exactly one of
/// `basic` or `oidc` must be set — they are mutually exclusive so the
/// server never has to choose between two competing session schemes.
#[derive(Clone, Debug, Deserialize)]
pub struct StudioConfig {
    #[serde(default)]
    pub basic: Option<StudioBasicConfig>,
    #[serde(default)]
    pub oidc: Option<StudioOidcConfig>,
}

/// Static HTTP Basic credentials. Appropriate for local dev or a
/// single-operator deployment. Browsers prompt via the native login
/// dialog; no session state.
#[derive(Clone, Debug, Deserialize)]
pub struct StudioBasicConfig {
    pub password: String,
    #[serde(default = "default_studio_username")]
    pub username: String,
}

/// OIDC (OpenID Connect) login. Validated against any compliant IdP —
/// Authentik, Keycloak, Auth0, Google, Microsoft, Okta. Access control
/// (who may use the studio) is delegated to the IdP's application
/// bindings, not configured here.
#[derive(Clone, Debug, Deserialize)]
pub struct StudioOidcConfig {
    pub client_id: String,
    /// Optional for public clients that use PKCE only. Authentik's default
    /// "confidential" client type requires a secret.
    #[serde(default)]
    pub client_secret: Option<String>,
    /// OIDC issuer URL. For Authentik, typically
    /// `https://authentik.example.com/application/o/<app-slug>/`.
    pub issuer_url: String,
    /// Absolute URL the IdP will redirect to after login. Must be
    /// whitelisted in the IdP's client config. The callback handler is
    /// served by Coulisse under this path; point it at a path inside
    /// `/studio/` (e.g. `https://coulisse.example.com/studio/auth/callback`).
    pub redirect_url: String,
    /// Additional OAuth2 scopes beyond the implicit `openid`. Defaults to
    /// `profile` and `email`; add `groups` if you want to surface group
    /// membership claims from Authentik (currently unused for authz, but
    /// available to future features).
    #[serde(default = "default_oidc_scopes")]
    pub scopes: Vec<String>,
}

fn default_studio_username() -> String {
    "admin".to_string()
}

fn default_oidc_scopes() -> Vec<String> {
    vec!["email".to_string(), "profile".to_string()]
}
