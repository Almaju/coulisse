mod conversation;
mod users;

pub use conversation::ConversationPage;
pub use users::UsersPage;

use std::time::{SystemTime, UNIX_EPOCH};

/// Render a unix-seconds timestamp as a short relative label (e.g. "3m ago").
/// Good enough for an admin overview; precision doesn't matter.
pub(crate) fn relative_time(ts: u64) -> String {
    if ts == 0 {
        return "—".into();
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let diff = now.saturating_sub(ts);
    if diff < 60 {
        format!("{diff}s ago")
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86_400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86_400)
    }
}
