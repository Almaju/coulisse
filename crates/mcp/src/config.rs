use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// OAuth configuration for an MCP server. Two flavours, picked by the
/// `mode` discriminator in YAML:
///
/// - `discover` (the default) — MCP-spec OAuth 2.1 with discovery +
///   Dynamic Client Registration (DCR). Coulisse fetches the provider's
///   authorization-server metadata and registers itself as a client on
///   first authorization. No credentials in YAML. This is what modern MCP
///   servers (Todoist, Atlassian, Linear, …) want, and what you get for
///   free by just declaring a `url:` — you never write `oauth:` at all.
/// - `static` — classic OAuth 2.0 with pre-registered app credentials, for
///   the rare provider that doesn't support DCR. You register Coulisse as a
///   client at the provider's console and paste the `client_id` /
///   `client_secret` here.
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

/// `oauth:` is a map whose `mode` defaults to `discover` when omitted, so
/// the common cases are:
///
/// - **omit `oauth:` entirely** — a `url:` server gets discover-mode OAuth
///   automatically. The 99% case; nothing to write.
/// - **`oauth: { scopes: [...] }`** — discover with an explicit scope list
///   (only needed when the server doesn't publish its own).
/// - **`oauth: { mode: static, ... }`** — explicit credentials for a
///   provider without Dynamic Client Registration.
///
/// To turn OAuth off for a non-auth HTTP MCP, write `oauth: false` on the
/// server entry itself (handled in [`McpServerConfig`]'s deserializer).
impl<'de> Deserialize<'de> for McpOAuthConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let value = serde_json::Value::deserialize(deserializer)?;
        let map = value.as_object().ok_or_else(|| {
            D::Error::custom(
                "oauth: must be a map (omit it for the default discover flow, \
                 or write `oauth: false` to disable auth)",
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<McpOAuthConfig>,
    #[serde(flatten)]
    pub transport: McpTransport,
}

/// Custom deserialization so users can drop `transport:` entirely and just
/// declare what their server is:
///
/// ```yaml
/// # Remote MCP — paste the URL, done. OAuth (discover mode) is automatic.
/// todoist:
///   url: https://ai.todoist.net/mcp
///
/// # Local MCP — a stdio child process.
/// hello:
///   command: uvx
///   args: [hello-mcp-server]
///
/// # Non-auth HTTP MCP — opt out of the automatic OAuth.
/// calculator:
///   url: http://localhost:8080
///   oauth: false
/// ```
///
/// Inference rules:
/// 1. `url:` + path contains `/sse` → `transport: sse`.
/// 2. `url:` otherwise → `transport: http`.
/// 3. `command:` (with optional `args:`/`env:`) → `transport: stdio`.
/// 4. An explicit `transport:` key overrides 1–3 (use it for an SSE
///    endpoint whose path doesn't contain `/sse`).
impl<'de> Deserialize<'de> for McpServerConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            /// Kept as raw JSON so the same field can carry a map
            /// (`oauth: { mode: ... }`) or the boolean `oauth: false`
            /// opt-out from the URL-auto-discover default.
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
        }
        let raw = Raw::deserialize(deserializer)?;
        use serde::de::Error;
        let transport = match (raw.transport.as_deref(), raw.url, raw.command) {
            // Explicit transport tag — useful for an SSE endpoint whose path
            // doesn't carry `/sse`, or just to be explicit.
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
        // Resolve `oauth:` with a zero-config default: URL-based servers get
        // discover-mode OAuth unless the user explicitly opts out. Same UX
        // ChatGPT uses — paste a URL, OAuth happens. Localhost is *not*
        // carved out: running a local OAuth-protected MCP is rare, and on
        // the off chance someone does, the discover flow fails loudly with
        // discovery errors rather than silently skipping auth.
        //
        // - `oauth: false` — explicit opt-out.
        // - `oauth: { ... }` — explicit discover/static config.
        // - URL transport + omitted `oauth:` — defaults to discover.
        // - Stdio transport + omitted `oauth:` — stays `None`. Stdio MCPs
        //   don't speak OAuth; the auto-default only applies to URL transports.
        let transport_is_url = matches!(
            &transport,
            McpTransport::Http { .. } | McpTransport::Sse { .. }
        );
        let oauth = match raw.oauth {
            Some(serde_json::Value::Bool(false)) => None,
            Some(v) => Some(McpOAuthConfig::deserialize(v).map_err(D::Error::custom)?),
            None if transport_is_url => Some(McpOAuthConfig::Discover { scopes: Vec::new() }),
            None => None,
        };
        Ok(Self { oauth, transport })
    }
}

/// Returns true if the URL's path has an `sse` segment (case-insensitive),
/// like `https://mcp.atlassian.com/v1/sse` or `https://example.com/sse/`.
/// Used to infer the older MCP-over-SSE transport from a bare `url:`.
fn url_path_includes_sse(url: &str) -> bool {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path_and_rest = after_scheme.split_once('/').map_or("", |(_, p)| p);
    let path = path_and_rest
        .split(['?', '#'])
        .next()
        .unwrap_or(path_and_rest);
    path.split('/').any(|seg| seg.eq_ignore_ascii_case("sse"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    /// doesn't apply to them.
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

    /// Unknown mode surfaces an error rather than silently falling back to
    /// discover.
    #[test]
    fn oauth_unknown_mode_errors_clearly() {
        let yaml = "url: https://example.com\noauth:\n  mode: device\n";
        let err = serde_yaml::from_str::<McpServerConfig>(yaml).unwrap_err();
        assert!(
            format!("{err}").contains("unknown oauth mode"),
            "expected mode error, got: {err}"
        );
    }

    /// A bare string `oauth:` is no longer accepted — guide the user to the
    /// map form (or to just omitting it).
    #[test]
    fn oauth_string_is_rejected_with_guidance() {
        let yaml = "url: https://example.com\noauth: discover\n";
        let err = serde_yaml::from_str::<McpServerConfig>(yaml).unwrap_err();
        assert!(
            format!("{err}").contains("oauth: must be a map"),
            "expected map-guidance error, got: {err}"
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
    /// user can declare an Atlassian-style config without knowing about
    /// the transport distinction.
    #[test]
    fn url_with_sse_path_infers_sse_transport() {
        let yaml = "url: https://mcp.atlassian.com/v1/sse\n";
        let cfg: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(cfg.transport, McpTransport::Sse { .. }));
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

    /// Explicit `transport: http` keeps working — handy to be explicit, or
    /// to force SSE on a path that doesn't carry `/sse`.
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

    /// `url_path_includes_sse` must look at PATH segments, not substrings,
    /// so a hostname like `sse.example.com` doesn't trigger it.
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
