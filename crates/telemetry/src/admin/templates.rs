use askama::Template;

use super::views::{EventRow, RecentToolCallRow, ToolCallRow, ToolDetailRow, ToolListRow};

#[derive(Template)]
#[template(path = "events.html")]
pub struct EventsFragment {
    pub rows: Vec<EventRow>,
}

#[derive(Template)]
#[template(path = "tool_calls.html")]
pub struct ToolCallsFragment {
    pub rows: Vec<ToolCallRow>,
}

#[derive(Template)]
#[template(path = "tool_detail.html")]
pub struct ToolDetailPage {
    pub recent_calls: Vec<RecentToolCallRow>,
    pub tool: ToolDetailRow,
}

#[derive(Template)]
#[template(path = "tools.html")]
pub struct ToolsPage {
    pub tools: Vec<ToolListRow>,
}
