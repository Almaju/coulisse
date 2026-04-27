use askama::Template;

use super::views::{EventRow, RecentToolCallRow, ToolCallRow, ToolDetailRow, ToolListRow};

#[derive(Template)]
#[template(path = "events.html")]
pub(super) struct EventsFragment {
    pub rows: Vec<EventRow>,
}

#[derive(Template)]
#[template(path = "tool_calls.html")]
pub(super) struct ToolCallsFragment {
    pub rows: Vec<ToolCallRow>,
}

#[derive(Template)]
#[template(path = "tool_detail.html")]
pub(super) struct ToolDetailPage {
    pub recent_calls: Vec<RecentToolCallRow>,
    pub tool: ToolDetailRow,
}

#[derive(Template)]
#[template(path = "tools.html")]
pub(super) struct ToolsPage {
    pub tools: Vec<ToolListRow>,
}
