use askama::Template;

use super::MemoryRow;
use super::views::{MessageRow, UserRow};

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
    pub user_id: String,
}
