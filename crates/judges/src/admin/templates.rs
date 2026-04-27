use askama::Template;

use super::views::{
    AgentCriterionMatrix, JudgeDetailRow, JudgeListRow, ScoreRow, ScoreRowMean, ScoresPanel,
};

#[derive(Template)]
#[template(path = "scores.html")]
pub(super) struct ScoresFragment {
    pub scores: ScoresPanel,
}

#[derive(Template)]
#[template(path = "scores_means.html")]
pub(super) struct ScoresMeansFragment {
    pub rows: Vec<ScoreRowMean>,
}

#[derive(Template)]
#[template(path = "judge_detail.html")]
pub(super) struct JudgeDetailPage {
    pub judge: JudgeDetailRow,
    pub matrix: AgentCriterionMatrix,
    pub recent_scores: Vec<ScoreRow>,
}

#[derive(Template)]
#[template(path = "judges.html")]
pub(super) struct JudgesPage {
    pub judges: Vec<JudgeListRow>,
}

#[derive(Template)]
#[template(path = "judge_edit.html")]
pub(super) struct JudgeEditPage {
    pub action: String,
    pub is_new: bool,
    pub method: &'static str,
    pub name: String,
    pub yaml: String,
}
