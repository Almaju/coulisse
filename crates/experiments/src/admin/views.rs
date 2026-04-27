use std::time::{SystemTime, UNIX_EPOCH};

use crate::merge::{AdminExperiment, AdminSource};
use crate::{ExperimentConfig, Strategy};

pub(super) struct SourceLabel(pub &'static str);

impl SourceLabel {
    pub(super) fn from_admin(source: AdminSource) -> Self {
        Self(match source {
            AdminSource::Dynamic => "dynamic",
            AdminSource::Override => "override",
            AdminSource::Tombstoned => "tombstoned",
            AdminSource::Yaml => "yaml",
        })
    }

    pub(super) fn as_str(&self) -> &'static str {
        self.0
    }
}

pub(super) struct ExperimentRow {
    pub epsilon: Option<f32>,
    pub means_url: Option<String>,
    pub metric: Option<String>,
    pub min_samples: Option<u32>,
    pub name: String,
    pub purpose: Option<String>,
    pub sampling_rate: Option<f32>,
    pub source: SourceLabel,
    pub sticky_by_user: bool,
    pub strategy: &'static str,
    pub tombstoned: bool,
    pub variants: Vec<VariantRow>,
    pub yaml_backed: bool,
}

pub(super) struct VariantRow {
    pub agent: String,
    pub is_primary: bool,
    pub share: String,
    pub weight: f32,
}

impl ExperimentRow {
    /// Build the display row from an admin merge entry. Bandit experiments
    /// embed an htmx URL pointing at the judges admin router for live mean
    /// scores; tombstones get a stripped-down view.
    pub(super) fn from_admin(row: &AdminExperiment) -> Self {
        let label = SourceLabel::from_admin(row.source);
        match &row.config {
            Some(exp) => Self::from_config(exp, label, row.yaml_backed),
            None => Self {
                epsilon: None,
                means_url: None,
                metric: None,
                min_samples: None,
                name: row.name.clone(),
                purpose: None,
                sampling_rate: None,
                source: label,
                sticky_by_user: false,
                strategy: "",
                tombstoned: true,
                variants: Vec::new(),
                yaml_backed: row.yaml_backed,
            },
        }
    }

    fn from_config(exp: &ExperimentConfig, source: SourceLabel, yaml_backed: bool) -> Self {
        let total: f32 = exp.variants.iter().map(|v| v.weight).sum();
        let primary = exp.primary.as_deref();
        let means_url = match exp.strategy {
            Strategy::Bandit => exp.metric.as_deref().and_then(|metric| {
                metric.split_once('.').map(|(judge, criterion)| {
                    let window = exp.bandit_window_seconds.unwrap_or(7 * 24 * 60 * 60);
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let since = now.saturating_sub(window);
                    format!(
                        "/admin/scores/means?judge={}&criterion={}&since={}",
                        urlencode(judge),
                        urlencode(criterion),
                        since,
                    )
                })
            }),
            _ => None,
        };
        let variants = exp
            .variants
            .iter()
            .map(|v| {
                let pct = if total > 0.0 {
                    100.0 * v.weight / total
                } else {
                    0.0
                };
                VariantRow {
                    agent: v.agent.clone(),
                    is_primary: Some(v.agent.as_str()) == primary,
                    share: format!("{pct:.0}%"),
                    weight: v.weight,
                }
            })
            .collect();
        Self {
            epsilon: exp.epsilon,
            means_url,
            metric: exp.metric.clone(),
            min_samples: exp.min_samples,
            name: exp.name.clone(),
            purpose: exp.purpose.clone(),
            sampling_rate: exp.sampling_rate,
            source,
            sticky_by_user: exp.sticky_by_user,
            strategy: match exp.strategy {
                Strategy::Bandit => "bandit",
                Strategy::Shadow => "shadow",
                Strategy::Split => "split",
            },
            tombstoned: false,
            variants,
            yaml_backed,
        }
    }
}

/// Minimal percent-encoder for query string values. Judge names and
/// criteria are alphanumeric in practice but the encoder is robust to any
/// future relaxation of the rules.
fn urlencode(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}
