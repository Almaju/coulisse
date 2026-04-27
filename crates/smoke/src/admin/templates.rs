use askama::Template;

use super::views::{RunDetailView, RunRow, SmokeTestRow};

#[derive(Template)]
#[template(path = "smoke.html")]
pub(super) struct SmokePage {
    pub tests: Vec<SmokeTestRow>,
}

#[derive(Template)]
#[template(path = "smoke_test_detail.html")]
pub(super) struct SmokeTestDetailPage {
    pub recent_runs: Vec<RunRow>,
    pub test: SmokeTestRow,
}

#[derive(Template)]
#[template(path = "smoke_run.html")]
pub(super) struct SmokeRunPage {
    pub run: RunDetailView,
}

#[derive(Template)]
#[template(path = "smoke_edit.html")]
pub(super) struct SmokeTestEditPage {
    pub action: String,
    pub is_new: bool,
    pub method: &'static str,
    pub name: String,
    pub yaml: String,
}
