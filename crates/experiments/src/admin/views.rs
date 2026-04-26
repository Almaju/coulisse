use std::time::{SystemTime, UNIX_EPOCH};

use crate::{ExperimentConfig, Strategy};

pub struct ExperimentRow {
    pub epsilon: Option<f32>,
    pub means_url: Option<String>,
    pub metric: Option<String>,
    pub min_samples: Option<u32>,
    pub name: String,
    pub purpose: Option<String>,
    pub sampling_rate: Option<f32>,
    pub sticky_by_user: bool,
    pub strategy: &'static str,
    pub variants: Vec<VariantRow>,
}

pub struct VariantRow {
    pub agent: String,
    pub is_primary: bool,
    pub share: String,
    pub weight: f32,
}

impl ExperimentRow {
    /// Build the display row from a config. Bandit experiments embed an
    /// htmx URL pointing at the judges admin router for live mean scores;
    /// non-bandit experiments leave that slot empty.
    pub fn build(exp: &ExperimentConfig) -> Self {
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
            sticky_by_user: exp.sticky_by_user,
            strategy: match exp.strategy {
                Strategy::Bandit => "bandit",
                Strategy::Shadow => "shadow",
                Strategy::Split => "split",
            },
            variants,
        }
    }
}

/// Minimal percent-encoder for query string values. Judge names and
/// criteria are alphanumeric in practice but the encoder is robust to any
/// future relaxation of the rules.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
