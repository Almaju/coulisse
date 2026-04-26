use askama::Template;

use super::views::{
    AgentCriterionMatrix, JudgeDetailRow, JudgeListRow, ScoreRow, ScoreRowMean, ScoresPanel,
};

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

#[derive(Template)]
#[template(path = "judge_detail.html")]
pub struct JudgeDetailPage {
    pub judge: JudgeDetailRow,
    pub matrix: AgentCriterionMatrix,
    pub recent_scores: Vec<ScoreRow>,
}

#[derive(Template)]
#[template(path = "judges.html")]
pub struct JudgesPage {
    pub judges: Vec<JudgeListRow>,
}

#[derive(Template)]
#[template(path = "judge_edit.html")]
pub struct JudgeEditPage {
    pub action: String,
    pub is_new: bool,
    pub method: &'static str,
    pub name: String,
    pub yaml: String,
}
