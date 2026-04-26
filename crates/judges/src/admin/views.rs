use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::Score;

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
