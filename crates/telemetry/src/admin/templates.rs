use askama::Template;

use super::views::{EventRow, ToolCallRow};

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
