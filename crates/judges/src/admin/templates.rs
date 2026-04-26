use askama::Template;

use super::views::{ScoreRowMean, ScoresPanel};

#[derive(Template)]
#[template(path = "scores.html")]
pub struct ScoresFragment {
    pub scores: ScoresPanel,
}

#[derive(Template)]
#[template(path = "scores_means.html")]
pub struct ScoresMeansFragment {
    pub rows: Vec<ScoreRowMean>,
}
