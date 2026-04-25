mod conversation;
mod users;

pub use conversation::ConversationPage;
pub use users::UsersPage;

/// Render a unix-seconds timestamp as a short relative label (e.g. "3m ago").
/// Good enough for a studio overview; precision doesn't matter.
///
/// `std::time::SystemTime` is unimplemented on `wasm32-unknown-unknown` and
/// panics at runtime if called — we read the clock through `js_sys::Date`
/// instead.
pub(crate) fn relative_time(ts: u64) -> String {
    if ts == 0 {
        return "—".into();
    }
    let now = (js_sys::Date::now() / 1000.0) as u64;
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
