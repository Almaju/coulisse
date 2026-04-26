use std::collections::HashMap;

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct McpToolAccess {
    #[serde(default)]
    pub only: Option<Vec<String>>,
    pub server: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum McpServerConfig {
    Http {
        url: String,
    },
    Stdio {
        #[serde(default)]
        args: Vec<String>,
        command: String,
        #[serde(default)]
        env: HashMap<String, String>,
    },
}
