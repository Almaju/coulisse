use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::store::AgentCriterionCell;
use crate::{JudgeConfig, Score};

pub struct ScoreRow {
    pub created_at: String,
    pub criterion: String,
    pub judge_name: String,
    pub reasoning: String,
    pub score: String,
}

pub struct CriterionAverageRow {
    pub average: String,
    pub count: u32,
    pub criterion: String,
    pub judge_name: String,
}

pub struct ScoresPanel {
    pub averages: Vec<CriterionAverageRow>,
    pub recent: Vec<ScoreRow>,
}

impl ScoresPanel {
    pub fn build(scores: Vec<Score>) -> Self {
        let averages = average_by_criterion(&scores);
        // Most recent first, top 5 — same posture as the legacy SPA so
        // operators recognize the surface.
        let mut recent: Vec<ScoreRow> = scores
            .into_iter()
            .rev()
            .take(5)
            .map(|s| ScoreRow {
                created_at: relative_time(s.created_at),
                criterion: s.criterion,
                judge_name: s.judge_name,
                reasoning: s.reasoning,
                score: format!("{:.1}", s.score),
            })
            .collect();
        recent.shrink_to_fit();
        Self { averages, recent }
    }
}

pub struct ScoreRowMean {
    pub agent: String,
    pub mean: String,
    pub samples: u32,
}

pub struct AgentCriterionMatrix {
    pub criteria: Vec<String>,
    pub rows: Vec<MatrixRow>,
}

pub struct JudgeDetailRow {
    pub model: String,
    pub name: String,
    pub provider: String,
    pub rubrics: Vec<RubricRow>,
    pub sampling_rate: String,
}

impl JudgeDetailRow {
    pub fn from_config(config: &JudgeConfig) -> Self {
        let rubrics = config
            .rubrics
            .iter()
            .map(|(name, desc)| RubricRow {
                description: desc.clone(),
                name: name.clone(),
            })
            .collect();
        Self {
            model: config.model.clone(),
            name: config.name.clone(),
            provider: config.provider.clone(),
            rubrics,
            sampling_rate: format!("{:.0}%", config.sampling_rate * 100.0),
        }
    }
}

pub struct JudgeListRow {
    pub criteria_count: usize,
    pub model: String,
    pub name: String,
    pub provider: String,
    pub sampling_rate: String,
    pub score_count_7d: u32,
}

pub struct MatrixCell {
    pub color_class: &'static str,
    pub mean: String,
    pub samples: u32,
}

pub struct MatrixRow {
    pub agent_name: String,
    pub cells: Vec<MatrixCell>,
}

pub struct RubricRow {
    pub description: String,
    pub name: String,
}

impl ScoreRow {
    pub fn from_score(s: Score) -> Self {
        Self {
            created_at: relative_time(s.created_at),
            criterion: s.criterion,
            judge_name: s.judge_name,
            reasoning: s.reasoning,
            score: format!("{:.1}", s.score),
        }
    }
}

pub fn build_matrix(cells: Vec<AgentCriterionCell>) -> AgentCriterionMatrix {
    let mut criteria_set = BTreeSet::new();
    let mut by_agent: BTreeMap<String, Vec<&AgentCriterionCell>> = BTreeMap::new();
    for cell in &cells {
        criteria_set.insert(cell.criterion.clone());
        by_agent
            .entry(cell.agent_name.clone())
            .or_default()
            .push(cell);
    }
    let criteria: Vec<String> = criteria_set.into_iter().collect();
    let rows = by_agent
        .into_iter()
        .map(|(agent_name, agent_cells)| {
            let cell_map: HashMap<&str, &AgentCriterionCell> = agent_cells
                .into_iter()
                .map(|c| (c.criterion.as_str(), c))
                .collect();
            let cells = criteria
                .iter()
                .map(|crit| match cell_map.get(crit.as_str()) {
                    Some(c) => {
                        let color_class = if c.mean >= 7.0 {
                            "text-emerald-300"
                        } else if c.mean >= 4.0 {
                            "text-amber-300"
                        } else {
                            "text-rose-300"
                        };
                        MatrixCell {
                            color_class,
                            mean: format!("{:.1}", c.mean),
                            samples: c.samples,
                        }
                    }
                    None => MatrixCell {
                        color_class: "text-slate-500",
                        mean: "—".into(),
                        samples: 0,
                    },
                })
                .collect();
            MatrixRow { agent_name, cells }
        })
        .collect();
    AgentCriterionMatrix { criteria, rows }
}

fn average_by_criterion(scores: &[Score]) -> Vec<CriterionAverageRow> {
    let mut buckets: HashMap<(String, String), (f64, u32)> = HashMap::new();
    for s in scores {
        let entry = buckets
            .entry((s.judge_name.clone(), s.criterion.clone()))
            .or_insert((0.0, 0));
        entry.0 += s.score as f64;
        entry.1 += 1;
    }
    let mut out: Vec<CriterionAverageRow> = buckets
        .into_iter()
        .map(
            |((judge_name, criterion), (sum, count))| CriterionAverageRow {
                average: format!("{:.1}", sum / count as f64),
                count,
                criterion,
                judge_name,
            },
        )
        .collect();
    out.sort_by(|a, b| {
        a.judge_name
            .cmp(&b.judge_name)
            .then_with(|| a.criterion.cmp(&b.criterion))
    });
    out
}

fn relative_time(seconds: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(seconds);
    let diff = now.saturating_sub(seconds);
    if diff < 60 {
        return "just now".into();
    }
    if diff < 3600 {
        return format!("{}m ago", diff / 60);
    }
    if diff < 86_400 {
        return format!("{}h ago", diff / 3600);
    }
    format!("{}d ago", diff / 86_400)
}
