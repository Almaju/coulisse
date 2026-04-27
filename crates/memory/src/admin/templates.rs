use askama::Template;

use super::MemoryRow;
use super::views::{AgentConversationRow, ConversationRow, MessageRow};

#[derive(Template)]
#[template(path = "agent_recent_conversations.html")]
pub(super) struct AgentRecentConversationsFragment {
    pub conversations: Vec<AgentConversationRow>,
}

#[derive(Template)]
#[template(path = "conversations.html")]
pub(super) struct ConversationsPage {
    pub conversations: Vec<ConversationRow>,
}

#[derive(Template)]
#[template(path = "conversation.html")]
pub(super) struct ConversationPage {
    pub memories: Vec<MemoryRow>,
    pub messages: Vec<MessageRow>,
    pub user_id: String,
}
