use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// OAuth configuration for an MCP server. Two flavours, picked by the
/// `mode` discriminator in YAML:
///
/// - `static` — classic OAuth 2.0 with pre-registered app credentials. You
///   register Coulisse as a client at the provider's developer console and
///   paste the resulting `client_id` / `client_secret` here. Required for
///   providers that don't support Dynamic Client Registration.
/// - `discover` — MCP-spec OAuth 2.1 with discovery + Dynamic Client
///   Registration (DCR). Coulisse fetches the provider's authorization
///   server metadata from `<mcp_origin>/.well-known/oauth-authorization-server`
///   and registers itself as a client on first user authorization. No
///   credentials in YAML. This is what modern MCP servers (Todoist,
///   Atlassian, Linear, …) want.
///
/// Both variants drive the same per-user token flow: tokens are stored in
/// the vault keyed by `(server_name, user_id)`, and the `NotConnectedTool`
/// placeholder surfaces a per-user connect link to the LLM when a user
/// hasn't authorized yet.
#[derive(Clone, Debug, schemars::JsonSchema, Serialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum McpOAuthConfig {
    Discover {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        scopes: Vec<String>,
    },
    Static {
        authorization_url: String,
        client_id: String,
        client_secret: String,
        redirect_uri: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        scopes: Vec<String>,
        token_url: String,
    },
}

impl McpOAuthConfig {
    /// Scopes the user is asked to grant. Discover mode falls back to the
    /// provider's `scopes_supported` if this list is empty.
    #[must_use]
    pub fn scopes(&self) -> &[String] {
        match self {
            Self::Discover { scopes } | Self::Static { scopes, .. } => scopes,
        }
    }
}

/// `oauth:` accepts three shapes in YAML, ordered by how often you'd
/// actually write each one:
///
/// 1. **`oauth: discover`** — string shorthand. Discover-mode OAuth
///    (RFC 8414 + RFC 7591 DCR), no fields to fill in. The 99% case for
///    spec-compliant remote MCP servers.
/// 2. **`oauth: {}` / `oauth: { scopes: [...] }`** — bare map, no
///    `mode:` field. Same as `discover`, with optional scope override.
/// 3. **`oauth: { mode: static, ... }`** — explicit static credentials.
///    Required for providers that don't support Dynamic Client
///    Registration; needs `client_id` / `client_secret` /
///    `authorization_url` / `token_url` / `redirect_uri`.
///
/// `mode: discover` is the default — you only write `mode:` when
/// switching to `static`.
impl<'de> Deserialize<'de> for McpOAuthConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let value = serde_json::Value::deserialize(deserializer)?;
        if let Some(s) = value.as_str() {
            return match s {
                "discover" => Ok(Self::Discover { scopes: Vec::new() }),
                other => Err(D::Error::custom(format!(
                    "unknown oauth shorthand `{other}` (use `discover`, or a full \
                     `oauth: {{ mode: static, ... }}` block)"
                ))),
            };
        }
        let map = value.as_object().ok_or_else(|| {
            D::Error::custom(
                "oauth: must be either the shorthand `discover` or a map with `mode:`/`scopes:`",
            )
        })?;
        // mode defaults to "discover" when absent
        let mode = map
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("discover");
        match mode {
            "discover" => {
                let scopes = map
                    .get("scopes")
                    .cloned()
                    .map(serde_json::from_value::<Vec<String>>)
                    .transpose()
                    .map_err(D::Error::custom)?
                    .unwrap_or_default();
                Ok(Self::Discover { scopes })
            }
            "static" => {
                #[derive(Deserialize)]
                struct StaticRaw {
                    authorization_url: String,
                    client_id: String,
                    client_secret: String,
                    redirect_uri: String,
                    #[serde(default)]
                    scopes: Vec<String>,
                    token_url: String,
                }
                let StaticRaw {
                    authorization_url,
                    client_id,
                    client_secret,
                    redirect_uri,
                    scopes,
                    token_url,
                } = serde_json::from_value(serde_json::Value::Object(map.clone()))
                    .map_err(D::Error::custom)?;
                Ok(Self::Static {
                    authorization_url,
                    client_id,
                    client_secret,
                    redirect_uri,
                    scopes,
                    token_url,
                })
            }
            other => Err(D::Error::custom(format!(
                "unknown oauth mode `{other}` (expected `discover` or `static`)"
            ))),
        }
    }
}

#[derive(Clone, Debug, schemars::JsonSchema, Serialize)]
pub struct McpServerConfig {
    /// When `true`, skip Coulisse's `npx mcp-remote <URL>` → native
    /// HTTP/SSE rewrite for this server. Use when the upstream MCP
    /// server only accepts tokens from `mcp-remote`'s well-known
    /// (grandfathered) client_id — Todoist's MCP at the moment — so
    /// `mcp-remote` must continue to run as a stdio child rather than
    /// be replaced by Coulisse's native transport + DCR.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_rewrite: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<McpOAuthConfig>,
    #[serde(flatten)]
    pub transport: McpTransport,
}

/// Custom deserialization so users can drop `transport:` entirely and
/// just declare what their server is. The legacy explicit form keeps
/// working:
///
/// ```yaml
/// # Modern URL-only form — what we recommend (matches ChatGPT-style UX).
/// todoist:
///   url: https://ai.todoist.net/mcp
///   oauth:
///     mode: discover
///
/// # Stdio (local MCP servers).
/// hello:
///   command: uvx
///   args: [hello-mcp-server]
///
/// # Legacy explicit transport (still supported, but verbose).
/// legacy:
///   transport: http
///   url: https://internal.example.com/mcp
/// ```
///
/// The auto-detection rules:
/// 1. `url:` + path contains `/sse` → `transport: sse`.
/// 2. `url:` otherwise → `transport: http`.
/// 3. `command:` (with optional `args:`/`env:`) → `transport: stdio`.
/// 4. Explicit `transport:` overrides 1-3.
impl<'de> Deserialize<'de> for McpServerConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            no_rewrite: bool,
            /// We keep this as raw JSON so the same field can carry a
            /// string (`oauth: discover`), a map (`oauth: { mode: ... }`),
            /// or a boolean (`oauth: false` for an explicit opt-out from
            /// the URL-auto-discover default).
            #[serde(default)]
            oauth: Option<serde_json::Value>,
            #[serde(default)]
            transport: Option<String>,
            #[serde(default)]
            url: Option<String>,
            #[serde(default)]
            command: Option<String>,
            #[serde(default)]
            args: Vec<String>,
            #[serde(default)]
            env: HashMap<String, String>,
            /// Escape hatch for URL-based servers whose resource (the MCP
            /// endpoint) whitelists `mcp-remote`'s grandfathered
            /// `client_id` and refuses fresh DCR registrations. Set true
            /// and Coulisse internally routes through `npx mcp-remote
            /// @latest <url>` as a stdio child, which uses
            /// `mcp-remote`'s cached identity and its own OAuth flow.
            /// Confirmed need: Todoist's MCP at `ai.todoist.net/mcp`.
            #[serde(default)]
            use_mcp_remote: bool,
        }
        let raw = Raw::deserialize(deserializer)?;
        use serde::de::Error;
        let transport = match (raw.transport.as_deref(), raw.url, raw.command) {
            // Explicit transport tag — keeps the legacy form working.
            (Some("http"), Some(url), None) => McpTransport::Http { url },
            (Some("sse"), Some(url), None) => McpTransport::Sse { url },
            (Some("stdio"), None, Some(command)) => McpTransport::Stdio {
                args: raw.args,
                command,
                env: raw.env,
            },
            (Some("http" | "sse"), None, _) => {
                return Err(D::Error::custom("transport `http`/`sse` requires `url:`"));
            }
            (Some("stdio"), Some(_), _) => {
                return Err(D::Error::custom(
                    "transport `stdio` requires `command:`, not `url:`",
                ));
            }
            (Some(tag), _, _) => {
                return Err(D::Error::custom(format!(
                    "unknown transport `{tag}` (expected one of: http, sse, stdio)"
                )));
            }

            // No explicit transport — infer from the kind-discriminating fields.
            (None, Some(url), None) => {
                if url_path_includes_sse(&url) {
                    McpTransport::Sse { url }
                } else {
                    McpTransport::Http { url }
                }
            }
            (None, None, Some(command)) => McpTransport::Stdio {
                args: raw.args,
                command,
                env: raw.env,
            },
            (None, Some(_), Some(_)) => {
                return Err(D::Error::custom(
                    "specify either `url:` (http/sse) or `command:` (stdio), not both",
                ));
            }
            (None, None, None) => {
                return Err(D::Error::custom(
                    "MCP server config needs either `url:` (remote http/sse) or `command:` (local stdio)",
                ));
            }
        };
        // `use_mcp_remote: true` on a URL config rewrites internally to
        // the stdio mcp-remote shim. Lets users keep the clean URL
        // mental model in YAML while routing through mcp-remote for
        // servers that whitelist its grandfathered client_id.
        let (transport, force_no_oauth) = if raw.use_mcp_remote {
            match transport {
                McpTransport::Http { url } | McpTransport::Sse { url } => (
                    McpTransport::Stdio {
                        args: vec!["-y".to_string(), "mcp-remote@latest".to_string(), url],
                        command: "npx".to_string(),
                        env: HashMap::new(),
                    },
                    true,
                ),
                McpTransport::Stdio { .. } => {
                    return Err(D::Error::custom(
                        "use_mcp_remote: true only makes sense with `url:` — got a stdio config",
                    ));
                }
            }
        } else {
            (transport, false)
        };
        // Resolve `oauth:` with a zero-config default: URL-based servers
        // get `oauth: discover` unless the user explicitly opts out.
        // Same UX ChatGPT uses — paste a URL, OAuth happens. Localhost is
        // *not* carved out: it's rare to run a local OAuth-protected MCP,
        // but on the off chance someone does, the auto-discover flow will
        // just fail loudly with discovery errors rather than silently
        // skipping auth.
        //
        // - `oauth: false` (or `oauth: { ... }` map / `oauth: discover`) — explicit.
        // - URL transport + omitted `oauth:` — defaults to discover.
        // - Stdio transport + omitted `oauth:` — stays `None`. Stdio MCPs
        //   don't speak OAuth; the auto-default only applies to URL transports.
        let transport_is_url = matches!(
            &transport,
            McpTransport::Http { .. } | McpTransport::Sse { .. }
        );
        let oauth = if force_no_oauth {
            // mcp-remote handles its own OAuth via the browser callback
            // it runs locally — Coulisse must not try to layer its own
            // discover-mode flow on top.
            None
        } else {
            match raw.oauth {
                Some(serde_json::Value::Bool(false)) => None,
                Some(serde_json::Value::Bool(true)) => {
                    Some(McpOAuthConfig::Discover { scopes: Vec::new() })
                }
                Some(v) => Some(McpOAuthConfig::deserialize(v).map_err(D::Error::custom)?),
                None if transport_is_url => Some(McpOAuthConfig::Discover { scopes: Vec::new() }),
                None => None,
            }
        };
        // When use_mcp_remote: true rewrote us to stdio, the
        // normalize_mcp_remote_shim auto-rewrite would otherwise undo our
        // rewrite. Set no_rewrite so that path skips us.
        let no_rewrite = raw.no_rewrite || raw.use_mcp_remote;
        Ok(Self {
            no_rewrite,
            oauth,
            transport,
        })
    }
}

impl McpServerConfig {
    /// If this entry is the canonical `npx mcp-remote@... <URL>` shim
    /// that the MCP docs steer users toward, rewrite it in place to a
    /// native streamable-HTTP transport with `oauth: { mode: discover }`
    /// and return a warning that names the equivalent explicit YAML.
    ///
    /// Why: `mcp-remote` does its own OAuth flow — opens a browser, runs
    /// a local callback server, caches tokens under the OS user — none of
    /// which is per-Coulisse-user. Coulisse handles the same MCP-spec
    /// OAuth 2.1 + DCR flow natively and stores tokens per-user in the
    /// vault. Running both stacks fights itself (the user's report:
    /// boot fails with `EADDRINUSE` when `mcp-remote`'s callback server
    /// clashes with a previous run). Auto-rewriting the canonical shim
    /// shape makes the docs-style YAML "just work" without forcing users
    /// to know about `oauth: discover`.
    ///
    /// We only rewrite the simple `npx[/pnpm/bunx/yarn] -y? mcp-remote
    /// <URL>` shape. Anything fancier (custom env vars on the shim, …)
    /// goes through untouched — the user is doing something we don't
    /// understand and we'd rather not silently drop their config.
    pub fn normalize_mcp_remote_shim(&mut self) -> Option<String> {
        // Explicit opt-out — the user has good reason to keep
        // mcp-remote as a stdio child (e.g. Todoist's MCP whitelists
        // mcp-remote's client_id and refuses fresh DCR registrations).
        if self.no_rewrite {
            return None;
        }
        let McpTransport::Stdio {
            args, command, env, ..
        } = &self.transport
        else {
            return None;
        };
        if !env.is_empty() {
            return None;
        }
        if !is_node_package_runner(command) {
            return None;
        }
        if !args.iter().any(|a| is_mcp_remote_package(a)) {
            return None;
        }
        let url = args
            .iter()
            .find(|a| a.starts_with("http://") || a.starts_with("https://"))?
            .clone();

        // Pick the right wire protocol based on the URL shape. A `/sse`
        // path segment signals the older MCP-over-SSE protocol revision
        // (Atlassian's `mcp.atlassian.com/v1/sse`); everything else gets
        // the current streamable-HTTP transport. mcp-remote does
        // HTTP-first-then-SSE-fallback at runtime; we can decide
        // upfront from the URL because in practice the path segment is
        // a reliable signal and probing on every boot adds latency.
        let oauth_inserted = self.oauth.is_none();
        let (warning, transport) = if url_path_includes_sse(&url) {
            (
                format!(
                    "MCP server uses the `npx mcp-remote {url}` shim; rewritten to native \
                     SSE transport + oauth: discover so Coulisse can mint per-user tokens \
                     and skip the Node process. To silence this warning, write the \
                     equivalent explicitly:\n\
                     \n  transport: sse\n  url: {url}\n  oauth:\n    mode: discover\n"
                ),
                McpTransport::Sse { url },
            )
        } else {
            (
                format!(
                    "MCP server uses the `npx mcp-remote {url}` shim; rewritten to native \
                     HTTP transport + oauth: discover so Coulisse can mint per-user tokens \
                     instead of letting mcp-remote run its own browser-based OAuth flow at \
                     boot. To silence this warning, write the equivalent explicitly:\n\
                     \n  transport: http\n  url: {url}\n  oauth:\n    mode: discover\n"
                ),
                McpTransport::Http { url },
            )
        };
        self.transport = transport;
        if oauth_inserted {
            self.oauth = Some(McpOAuthConfig::Discover { scopes: vec![] });
        }
        Some(warning)
    }
}

/// Returns true if the URL's path has an `sse` segment (case-insensitive),
/// like `https://mcp.atlassian.com/v1/sse` or `https://example.com/sse/`.
/// Used to keep the `mcp-remote` stdio shim in place for SSE-only MCP
/// servers, which Coulisse's streamable-HTTP transport can't reach.
fn url_path_includes_sse(url: &str) -> bool {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path_and_rest = after_scheme.split_once('/').map_or("", |(_, p)| p);
    let path = path_and_rest
        .split(['?', '#'])
        .next()
        .unwrap_or(path_and_rest);
    path.split('/').any(|seg| seg.eq_ignore_ascii_case("sse"))
}

fn is_node_package_runner(command: &str) -> bool {
    // Strip a leading path so `/usr/local/bin/npx` still matches `npx`.
    let basename = command.rsplit('/').next().unwrap_or(command);
    matches!(basename, "bunx" | "npx" | "pnpm" | "yarn")
}

/// Match `mcp-remote`, `mcp-remote@latest`, `mcp-remote@1.2.3`, and the
/// scoped variants (`@scope/mcp-remote@…`). We do not match arbitrary
/// substrings — `not-mcp-remote` should not trigger this.
fn is_mcp_remote_package(arg: &str) -> bool {
    let bare_name = arg.split('@').next().unwrap_or("");
    if bare_name == "mcp-remote" || bare_name.ends_with("/mcp-remote") {
        return true;
    }
    // Scoped packages start with `@scope/...`; the bare-name split above
    // returns "" for those, so handle them explicitly.
    if arg.starts_with('@')
        && let Some(rest) = arg.split('/').nth(1)
    {
        let pkg = rest.split('@').next().unwrap_or("");
        if pkg == "mcp-remote" {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdio(command: &str, args: &[&str]) -> McpServerConfig {
        McpServerConfig {
            no_rewrite: false,
            oauth: None,
            transport: McpTransport::Stdio {
                args: args.iter().map(|s| (*s).to_string()).collect(),
                command: command.to_string(),
                env: HashMap::new(),
            },
        }
    }

    /// `use_mcp_remote: true` on a URL config rewrites internally to
    /// `npx mcp-remote@latest <url>` stdio so the user keeps the clean
    /// URL mental model while routing through mcp-remote's
    /// grandfathered identity. Concrete case: Todoist's MCP currently
    /// only honours tokens issued to mcp-remote's whitelisted client_id.
    #[test]
    fn use_mcp_remote_rewrites_url_to_stdio_shim() {
        let yaml = "url: https://ai.todoist.net/mcp\nuse_mcp_remote: true\n";
        let cfg: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        match &cfg.transport {
            McpTransport::Stdio { command, args, env } => {
                assert_eq!(command, "npx");
                assert_eq!(
                    args,
                    &["-y", "mcp-remote@latest", "https://ai.todoist.net/mcp"]
                );
                assert!(env.is_empty());
            }
            other => panic!("expected stdio after use_mcp_remote rewrite, got {other:?}"),
        }
        // OAuth must be cleared — mcp-remote does its own auth flow,
        // Coulisse must not try to layer discover mode on top.
        assert!(cfg.oauth.is_none());
        // no_rewrite must be implicitly set so the legacy
        // normalize_mcp_remote_shim pass doesn't undo our rewrite on
        // the next config-load.
        assert!(cfg.no_rewrite);
    }

    /// `use_mcp_remote: true` on a stdio config is nonsense — the user
    /// is asking us to turn their stdio config into a stdio config.
    /// Surface that with a clear error.
    #[test]
    fn use_mcp_remote_on_stdio_is_rejected() {
        let yaml = "command: foo\nuse_mcp_remote: true\n";
        let err = serde_yaml::from_str::<McpServerConfig>(yaml).unwrap_err();
        assert!(
            format!("{err}").contains("only makes sense with `url:`"),
            "expected the url-only error, got: {err}"
        );
    }

    /// Zero-config: a URL-based server with no `oauth:` field at all
    /// defaults to discover mode. This is the ChatGPT-style UX —
    /// paste a URL and OAuth happens.
    #[test]
    fn url_only_defaults_to_oauth_discover() {
        let yaml = "url: https://ai.todoist.net/mcp\n";
        let cfg: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        match cfg.oauth {
            Some(McpOAuthConfig::Discover { scopes }) => assert!(scopes.is_empty()),
            other => panic!("expected default-discover, got {other:?}"),
        }
    }

    /// Stdio servers don't speak OAuth, so the auto-discover default
    /// doesn't apply to them. Same as before.
    #[test]
    fn stdio_only_stays_without_oauth() {
        let yaml = "command: uvx\nargs: [hello-mcp-server]\n";
        let cfg: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.oauth.is_none());
    }

    /// `oauth: false` is the explicit opt-out for the rare case of a
    /// URL-based MCP that doesn't need authentication (e.g. a local
    /// calculator service or an internal non-auth tool).
    #[test]
    fn oauth_false_opts_out_of_default_discover() {
        let yaml = "url: http://localhost:8080\noauth: false\n";
        let cfg: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.oauth.is_none());
    }

    /// `oauth: true` is also accepted as a synonym for `oauth: discover`
    /// — handy for users who think in booleans.
    #[test]
    fn oauth_true_opts_into_discover() {
        let yaml = "url: https://example.com/mcp\noauth: true\n";
        let cfg: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(cfg.oauth, Some(McpOAuthConfig::Discover { .. })));
    }

    /// String shorthand: `oauth: discover` is the most common form and
    /// shouldn't require typing the `mode:` field.
    #[test]
    fn oauth_string_shorthand_discover() {
        let yaml = "url: https://ai.todoist.net/mcp\noauth: discover\n";
        let cfg: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(cfg.oauth, Some(McpOAuthConfig::Discover { .. })));
    }

    /// `oauth:` with just scopes (no `mode:`) — defaults to discover.
    /// Lets users add a scope override without writing the mode.
    #[test]
    fn oauth_map_without_mode_defaults_to_discover() {
        let yaml = "url: https://ai.todoist.net/mcp\noauth:\n  scopes: [data:read_write]\n";
        let cfg: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        match cfg.oauth {
            Some(McpOAuthConfig::Discover { scopes }) => {
                assert_eq!(scopes, vec!["data:read_write".to_string()]);
            }
            other => panic!("expected discover with scopes, got {other:?}"),
        }
    }

    /// Original explicit `mode: discover` keeps working (backwards compat).
    #[test]
    fn oauth_explicit_mode_discover_still_accepted() {
        let yaml = "url: https://ai.todoist.net/mcp\noauth:\n  mode: discover\n";
        let cfg: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(cfg.oauth, Some(McpOAuthConfig::Discover { .. })));
    }

    /// `mode: static` requires the full set of credential fields and
    /// parses unchanged. The discover-default doesn't accidentally turn
    /// a static block into discover.
    #[test]
    fn oauth_static_mode_with_credentials() {
        let yaml = r#"
url: https://example.com/mcp
oauth:
  mode: static
  authorization_url: https://auth.example.com/authorize
  token_url: https://auth.example.com/token
  client_id: my-client
  client_secret: my-secret
  redirect_uri: http://localhost:8423/cb
"#;
        let cfg: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(cfg.oauth, Some(McpOAuthConfig::Static { .. })));
    }

    /// Unknown shorthand surfaces a useful error message.
    #[test]
    fn oauth_unknown_shorthand_errors_clearly() {
        let yaml = "url: https://example.com\noauth: magic\n";
        let err = serde_yaml::from_str::<McpServerConfig>(yaml).unwrap_err();
        assert!(
            format!("{err}").contains("unknown oauth shorthand"),
            "expected shorthand error, got: {err}"
        );
    }

    /// Unknown explicit mode surfaces an error rather than silently
    /// falling back to discover.
    #[test]
    fn oauth_unknown_mode_errors_clearly() {
        let yaml = "url: https://example.com\noauth:\n  mode: device\n";
        let err = serde_yaml::from_str::<McpServerConfig>(yaml).unwrap_err();
        assert!(
            format!("{err}").contains("unknown oauth mode"),
            "expected mode error, got: {err}"
        );
    }

    /// URL-only form — the simplest config we want users to write.
    /// HTTPS URL without a `/sse` segment → `transport: http`.
    #[test]
    fn url_only_yaml_infers_http_transport() {
        let yaml = "url: https://example.com/mcp\n";
        let cfg: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        match cfg.transport {
            McpTransport::Http { url } => assert_eq!(url, "https://example.com/mcp"),
            other => panic!("expected http, got {other:?}"),
        }
    }

    /// URL with `/sse` segment → `transport: sse` automatically. The
    /// user can declare a Todoist-or-Atlassian style config without
    /// knowing about the transport distinction.
    #[test]
    fn url_with_sse_path_infers_sse_transport() {
        let yaml = "url: https://mcp.atlassian.com/v1/sse\n";
        let cfg: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(cfg.transport, McpTransport::Sse { .. }));
    }

    /// URL-only + `oauth: { mode: discover }` is the canonical shape
    /// for remote MCPs that need per-user OAuth (Todoist, Atlassian).
    /// What we want users to write.
    #[test]
    fn url_plus_oauth_discover_works() {
        let yaml = "url: https://ai.todoist.net/mcp\noauth:\n  mode: discover\n";
        let cfg: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(cfg.transport, McpTransport::Http { .. }));
        assert!(matches!(cfg.oauth, Some(McpOAuthConfig::Discover { .. })));
    }

    /// `command:` (with no `url:`) → stdio transport. Local MCP
    /// servers stay just as easy to declare.
    #[test]
    fn command_only_yaml_infers_stdio_transport() {
        let yaml = "command: uvx\nargs: [hello-mcp-server]\n";
        let cfg: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        match cfg.transport {
            McpTransport::Stdio { command, args, .. } => {
                assert_eq!(command, "uvx");
                assert_eq!(args, vec!["hello-mcp-server".to_string()]);
            }
            other => panic!("expected stdio, got {other:?}"),
        }
    }

    /// Explicit `transport: http` (legacy form) keeps working. We
    /// can't break existing YAMLs.
    #[test]
    fn explicit_transport_tag_still_accepted() {
        let yaml = "transport: http\nurl: https://example.com/mcp\n";
        let cfg: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(cfg.transport, McpTransport::Http { .. }));
    }

    /// You can't be both URL-based AND stdio — surface that as a
    /// clear deserialize error instead of silently picking one.
    #[test]
    fn url_and_command_together_is_rejected() {
        let yaml = "url: https://example.com\ncommand: uvx\n";
        let err = serde_yaml::from_str::<McpServerConfig>(yaml).unwrap_err();
        assert!(
            format!("{err}").contains("specify either `url:`"),
            "expected url/command conflict message, got: {err}"
        );
    }

    /// Empty config with neither url nor command → clear error.
    #[test]
    fn neither_url_nor_command_is_rejected() {
        let yaml = "oauth:\n  mode: discover\n";
        let err = serde_yaml::from_str::<McpServerConfig>(yaml).unwrap_err();
        assert!(
            format!("{err}").contains("`url:`") && format!("{err}").contains("`command:`"),
            "error should hint at the two valid forms, got: {err}"
        );
    }

    /// Misspelt or unknown `transport:` value — surface it.
    #[test]
    fn unknown_explicit_transport_is_rejected() {
        let yaml = "transport: websocket\nurl: ws://example.com\n";
        let err = serde_yaml::from_str::<McpServerConfig>(yaml).unwrap_err();
        assert!(
            format!("{err}").contains("unknown transport"),
            "error should call out the bad tag, got: {err}"
        );
    }

    /// Explicit `no_rewrite: true` keeps `mcp-remote` as a stdio child
    /// even though the args look like the canonical shim shape.
    /// Concrete case: Todoist's MCP currently only honours tokens
    /// issued to `mcp-remote`'s grandfathered client_id, so the user
    /// must opt out of Coulisse's native-OAuth rewrite for that server.
    #[test]
    fn no_rewrite_flag_skips_normalization() {
        let mut cfg = stdio(
            "npx",
            &["-y", "mcp-remote@latest", "https://ai.todoist.net/mcp"],
        );
        cfg.no_rewrite = true;
        assert!(cfg.normalize_mcp_remote_shim().is_none());
        // Transport must stay as stdio.
        assert!(matches!(cfg.transport, McpTransport::Stdio { .. }));
        assert!(cfg.oauth.is_none());
    }

    #[test]
    fn rewrites_canonical_mcp_remote_shim() {
        let mut cfg = stdio(
            "npx",
            &["-y", "mcp-remote@latest", "https://example.com/mcp"],
        );
        let warning = cfg.normalize_mcp_remote_shim().expect("should rewrite");
        assert!(warning.contains("https://example.com/mcp"));
        match cfg.transport {
            McpTransport::Http { url } => assert_eq!(url, "https://example.com/mcp"),
            other => panic!("expected http transport, got {other:?}"),
        }
        assert!(matches!(cfg.oauth, Some(McpOAuthConfig::Discover { .. })));
    }

    #[test]
    fn rewrites_without_version_pin() {
        let mut cfg = stdio("npx", &["-y", "mcp-remote", "https://example.com/mcp"]);
        cfg.normalize_mcp_remote_shim().expect("should rewrite");
        assert!(matches!(cfg.transport, McpTransport::Http { .. }));
    }

    #[test]
    fn rewrites_pnpm_runner() {
        let mut cfg = stdio("pnpm", &["dlx", "mcp-remote", "https://example.com/mcp"]);
        cfg.normalize_mcp_remote_shim()
            .expect("pnpm should be detected");
    }

    #[test]
    fn rewrites_absolute_path_runner() {
        let mut cfg = stdio(
            "/usr/local/bin/npx",
            &["-y", "mcp-remote", "https://example.com/mcp"],
        );
        cfg.normalize_mcp_remote_shim()
            .expect("basename match should work for absolute paths");
    }

    #[test]
    fn preserves_existing_static_oauth() {
        let mut cfg = stdio("npx", &["-y", "mcp-remote", "https://example.com/mcp"]);
        cfg.oauth = Some(McpOAuthConfig::Static {
            authorization_url: "https://auth.example.com/authorize".into(),
            client_id: "id".into(),
            client_secret: "secret".into(),
            redirect_uri: "https://coulisse.example.com/mcp/x/oauth/callback".into(),
            scopes: vec![],
            token_url: "https://auth.example.com/token".into(),
        });
        cfg.normalize_mcp_remote_shim().expect("rewrites transport");
        // Transport is now http but the user-supplied oauth: static is untouched.
        assert!(matches!(cfg.transport, McpTransport::Http { .. }));
        assert!(matches!(cfg.oauth, Some(McpOAuthConfig::Static { .. })));
    }

    #[test]
    fn does_not_touch_non_mcp_remote_stdio() {
        let mut cfg = stdio("python", &["-m", "my_mcp"]);
        assert!(cfg.normalize_mcp_remote_shim().is_none());

        let mut cfg = stdio("npx", &["-y", "some-other-package"]);
        assert!(cfg.normalize_mcp_remote_shim().is_none());

        let mut cfg = stdio("npx", &["-y", "not-mcp-remote", "https://example.com"]);
        assert!(cfg.normalize_mcp_remote_shim().is_none());
    }

    #[test]
    fn does_not_touch_already_http_transport() {
        let mut cfg = McpServerConfig {
            no_rewrite: false,
            oauth: None,
            transport: McpTransport::Http {
                url: "https://example.com/mcp".to_string(),
            },
        };
        assert!(cfg.normalize_mcp_remote_shim().is_none());
    }

    #[test]
    fn skips_shim_when_custom_env_is_present() {
        let mut cfg = McpServerConfig {
            no_rewrite: false,
            oauth: None,
            transport: McpTransport::Stdio {
                args: vec![
                    "-y".into(),
                    "mcp-remote".into(),
                    "https://example.com/mcp".into(),
                ],
                command: "npx".into(),
                env: HashMap::from([("DEBUG".to_string(), "1".to_string())]),
            },
        };
        assert!(
            cfg.normalize_mcp_remote_shim().is_none(),
            "custom env vars mean we don't fully understand the shim — leave it alone"
        );
    }

    #[test]
    fn detects_scoped_mcp_remote() {
        assert!(is_mcp_remote_package("mcp-remote"));
        assert!(is_mcp_remote_package("mcp-remote@latest"));
        assert!(is_mcp_remote_package("mcp-remote@1.2.3"));
        assert!(is_mcp_remote_package("@scope/mcp-remote"));
        assert!(is_mcp_remote_package("@scope/mcp-remote@1.0.0"));
        assert!(!is_mcp_remote_package("not-mcp-remote"));
        assert!(!is_mcp_remote_package("mcp-remoteX"));
        assert!(!is_mcp_remote_package("@scope/other"));
    }

    /// Atlassian's MCP at `/v1/sse` speaks the older MCP-over-SSE
    /// protocol. Coulisse rewrites the `npx mcp-remote` shim to its
    /// native SSE transport so we can mint per-user tokens via the vault
    /// instead of letting mcp-remote run its own OAuth flow. Before this
    /// change Coulisse rewrote to streamable-HTTP, which 404'd, and the
    /// fix was to leave the stdio shim alone — but native SSE is the
    /// better answer now that the SSE client transport exists.
    #[test]
    fn sse_only_url_rewrites_to_sse_transport() {
        let mut cfg = stdio(
            "npx",
            &[
                "-y",
                "mcp-remote@latest",
                "https://mcp.atlassian.com/v1/sse",
            ],
        );
        let warning = cfg
            .normalize_mcp_remote_shim()
            .expect("should rewrite SSE-only URLs to native SSE transport");
        assert!(
            warning.contains("SSE transport"),
            "warning should mention the SSE rewrite: {warning}"
        );
        match &cfg.transport {
            McpTransport::Sse { url } => {
                assert_eq!(url, "https://mcp.atlassian.com/v1/sse");
            }
            other => panic!("expected SSE transport, got {other:?}"),
        }
        assert!(matches!(cfg.oauth, Some(McpOAuthConfig::Discover { .. })));
    }

    /// Non-SSE paths still get rewritten — Todoist's `/mcp` path stays
    /// on the fast native-HTTP path with per-user vault tokens.
    #[test]
    fn non_sse_url_still_rewrites_normally() {
        let mut cfg = stdio(
            "npx",
            &["-y", "mcp-remote@latest", "https://ai.todoist.net/mcp"],
        );
        cfg.normalize_mcp_remote_shim().expect("should rewrite");
        assert!(matches!(cfg.transport, McpTransport::Http { .. }));
        assert!(matches!(cfg.oauth, Some(McpOAuthConfig::Discover { .. })));
    }

    /// `url_path_includes_sse` must look at PATH segments, not substrings,
    /// so a hostname like `sse.example.com` doesn't trigger the skip.
    #[test]
    fn url_path_includes_sse_matches_path_segments_only() {
        assert!(super::url_path_includes_sse(
            "https://mcp.atlassian.com/v1/sse"
        ));
        assert!(super::url_path_includes_sse("https://example.com/sse"));
        assert!(super::url_path_includes_sse("https://example.com/sse/"));
        assert!(super::url_path_includes_sse(
            "https://example.com/sse/extra"
        ));
        // Case-insensitive on the segment.
        assert!(super::url_path_includes_sse("https://example.com/SSE"));
        // Host containing "sse" is not a path segment.
        assert!(!super::url_path_includes_sse("https://sse.example.com/mcp"));
        // Substring in another path segment doesn't count.
        assert!(!super::url_path_includes_sse("https://example.com/parsse"));
        assert!(!super::url_path_includes_sse(
            "https://example.com/ssegment"
        ));
        // Query / fragment shouldn't match.
        assert!(!super::url_path_includes_sse(
            "https://example.com/mcp?type=sse"
        ));
    }
}

/// The actual transport variant. Kept as a separate enum so the parent
/// struct can carry `oauth` alongside without breaking existing YAML
/// (the `transport` discriminant remains a sibling key).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
#[serde(rename_all = "lowercase", tag = "transport")]
pub enum McpTransport {
    Http {
        url: String,
    },
    /// MCP-over-SSE (older protocol revision). The server hosts a
    /// long-lived `GET <url>` event-stream; its first event announces
    /// the POST endpoint for outgoing JSON-RPC messages. Used by
    /// servers that haven't moved to streamable-HTTP yet — most notably
    /// Atlassian's `https://mcp.atlassian.com/v1/sse`.
    Sse {
        url: String,
    },
    Stdio {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        command: String,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        env: HashMap<String, String>,
    },
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
pub struct McpToolAccess {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,
    pub server: String,
}
