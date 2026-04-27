use askama::Template;

use super::views::ExperimentRow;

#[derive(Template)]
#[template(path = "experiments.html")]
pub(super) struct ExperimentsPage {
    pub experiments: Vec<ExperimentRow>,
}

#[derive(Template)]
#[template(path = "experiment_edit.html")]
pub(super) struct ExperimentEditPage {
    pub action: String,
    pub is_new: bool,
    pub method: &'static str,
    pub name: String,
    pub yaml: String,
}
