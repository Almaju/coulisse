use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
pub struct SidecarConfig {
    /// Command-line arguments. Each is passed as a separate argv entry —
    /// no shell expansion. Quote inside the YAML if you need spaces.
    #[serde(default)]
    pub args: Vec<String>,
    /// Executable to spawn. Absolute path or anything on `PATH`.
    pub command: String,
    /// Working directory. Defaults to Coulisse's current working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Environment variables added on top of Coulisse's own env. `${VAR}`
    /// placeholders are expanded the same way the rest of `coulisse.yaml`
    /// expands them, so you can pass through secrets without inlining.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    /// Stable identifier for logs.
    pub name: String,
    #[serde(default)]
    pub restart: RestartPolicy,
}

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, schemars::JsonSchema, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    /// Always restart, regardless of exit status. Useful for long-running
    /// services that are expected to stay up forever.
    Always,
    /// Never restart. The sidecar runs once; if it exits (for any reason),
    /// it stays exited.
    Never,
    /// Restart only on non-zero exit or signal-kill. Clean shutdowns
    /// (exit 0) are respected. This is the default.
    #[default]
    OnFailure,
}
