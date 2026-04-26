use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct McpToolAccess {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,
    pub server: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum McpServerConfig {
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
