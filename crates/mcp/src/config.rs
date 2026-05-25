use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
pub struct McpOAuthConfig {
    pub authorization_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    pub token_url: String,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
pub struct McpServerConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<McpOAuthConfig>,
    #[serde(flatten)]
    pub transport: McpTransport,
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
