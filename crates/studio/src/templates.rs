use askama::Template;

use crate::views::{EventRow, MemoryRow, MessageRow, ScoresPanel, UserRow};

#[derive(Template)]
#[template(path = "users.html")]
pub struct UsersPage {
    pub users: Vec<UserRow>,
}

#[derive(Template)]
#[template(path = "conversation.html")]
pub struct ConversationPage {
    pub memories: Vec<MemoryRow>,
    pub messages: Vec<MessageRow>,
    pub scores: ScoresPanel,
    pub user_id: String,
}

#[derive(Template)]
#[template(path = "events.html")]
pub struct EventsFragment {
    pub rows: Vec<EventRow>,
}
