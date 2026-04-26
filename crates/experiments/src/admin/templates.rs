use askama::Template;

use super::views::ExperimentRow;

#[derive(Template)]
#[template(path = "experiments.html")]
pub struct ExperimentsPage {
    pub experiments: Vec<ExperimentRow>,
}
