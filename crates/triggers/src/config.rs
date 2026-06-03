use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
pub struct TriggerConfig {
    /// Name of the agent to invoke when the trigger fires.
    pub agent: String,
    #[serde(flatten)]
    pub kind: TriggerKind,
    /// Stable identifier used in logs and admin views.
    pub name: String,
    /// Initial user message passed to the agent on each fire. Static for
    /// now; templating from trigger payload arrives with the webhook
    /// trigger.
    pub prompt: String,
}

/// Trigger discriminator. `#[serde(tag = "type")]` keeps each variant's
/// fields flat at the top level of a YAML entry so users write
/// `type: cron` and `schedule: "..."` as siblings, not nested.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
#[serde(rename_all = "lowercase", tag = "type")]
pub enum TriggerKind {
    /// Fires once when Coulisse boots, then never again. Useful for
    /// "wake up and decide what to do" prompts that should run on every
    /// `coulisse start` — e.g. asking the orchestrator agent to read the
    /// queue's leftovers and decide whether a standup is warranted.
    Boot {},
    Cron {
        /// 5-field POSIX cron (`min hour day-of-month month day-of-week`)
        /// or 6-field with leading seconds (`sec min hour …`). Parsed via
        /// the `cron` crate; 5-field expressions are normalised to 6-field
        /// with a leading `0` seconds before parsing.
        schedule: String,
    },
    Webhook {
        /// HTTP path Coulisse exposes for this trigger. Must start with
        /// `/hooks/` to keep webhook routes namespaced away from the
        /// proxy (`/v1/*`), studio (`/admin/*`), and OAuth callbacks
        /// (`/mcp/*`). External bridges (Slack, GitHub, anything
        /// HTTP-capable) POST JSON to this path to fire the trigger.
        path: String,
    },
}
