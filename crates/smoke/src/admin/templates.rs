use askama::Template;

use super::views::{RunDetailView, RunRow, SmokeTestRow};

#[derive(Template)]
#[template(path = "smoke.html")]
pub struct SmokePage {
    pub tests: Vec<SmokeTestRow>,
}

#[derive(Template)]
#[template(path = "smoke_test_detail.html")]
pub struct SmokeTestDetailPage {
    pub recent_runs: Vec<RunRow>,
    pub test: SmokeTestRow,
}

#[derive(Template)]
#[template(path = "smoke_run.html")]
pub struct SmokeRunPage {
    pub run: RunDetailView,
}
