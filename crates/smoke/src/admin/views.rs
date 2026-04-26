use crate::config::SmokeTestConfig;
use crate::types::{RunStatus, StoredMessage, StoredRun, TurnRole};

pub struct SmokeTestRow {
    pub last_run: Option<RunRow>,
    pub max_turns: u32,
    pub name: String,
    pub persona_model: String,
    pub repetitions: u32,
    pub target: String,
}

pub struct RunRow {
    pub agent_resolved: String,
    pub experiment: Option<String>,
    pub id: String,
    pub started_at_label: String,
    pub status: String,
    pub status_class: &'static str,
    pub total_turns: u32,
}

pub struct TurnView {
    pub content: String,
    pub is_persona: bool,
    pub label: &'static str,
    pub turn_index: u32,
}

pub struct RunDetailView {
    pub agent_resolved: String,
    pub error: Option<String>,
    pub experiment: Option<String>,
    pub id: String,
    pub is_running: bool,
    pub started_at_label: String,
    pub status: String,
    pub status_class: &'static str,
    pub test_name: String,
    pub total_turns: u32,
    pub turns: Vec<TurnView>,
}

impl SmokeTestRow {
    pub fn build(config: &SmokeTestConfig, last_run: Option<&StoredRun>) -> Self {
        Self {
            last_run: last_run.map(RunRow::build),
            max_turns: config.max_turns,
            name: config.name.clone(),
            persona_model: format!("{} / {}", config.persona.provider, config.persona.model),
            repetitions: config.repetitions,
            target: config.target.clone(),
        }
    }
}

impl RunRow {
    pub fn build(run: &StoredRun) -> Self {
        let (status_class, status) = status_pill(run.status);
        Self {
            agent_resolved: run.agent_resolved.clone().unwrap_or_else(|| "—".into()),
            experiment: run.experiment.clone(),
            id: run.id.0.to_string(),
            started_at_label: relative_time(run.started_at),
            status,
            status_class,
            total_turns: run.total_turns,
        }
    }
}

impl RunDetailView {
    pub fn build(run: &StoredRun, messages: Vec<StoredMessage>) -> Self {
        let (status_class, status) = status_pill(run.status);
        let mut turns: Vec<TurnView> = messages
            .into_iter()
            .map(|m| TurnView {
                content: m.content,
                is_persona: m.role == TurnRole::Persona,
                label: match m.role {
                    TurnRole::Assistant => "Assistant",
                    TurnRole::Persona => "Persona",
                },
                turn_index: m.turn_index,
            })
            .collect();
        // Within a turn pair, persona comes before assistant. messages_for_run
        // orders by (turn_index ASC, role ASC) — "assistant" sorts before
        // "persona" alphabetically, so we re-sort here for the desired display.
        turns.sort_by(|a, b| {
            a.turn_index
                .cmp(&b.turn_index)
                .then_with(|| (!a.is_persona).cmp(&!b.is_persona))
        });
        Self {
            agent_resolved: run.agent_resolved.clone().unwrap_or_else(|| "—".into()),
            error: run.error.clone(),
            experiment: run.experiment.clone(),
            id: run.id.0.to_string(),
            is_running: run.status == RunStatus::Running,
            started_at_label: relative_time(run.started_at),
            status,
            status_class,
            test_name: run.test_name.clone(),
            total_turns: run.total_turns,
            turns,
        }
    }
}

fn status_pill(status: RunStatus) -> (&'static str, String) {
    let class = match status {
        RunStatus::Completed => "border-emerald-900/60 bg-emerald-950/60 text-emerald-300",
        RunStatus::Failed => "border-rose-900/60 bg-rose-950/60 text-rose-300",
        RunStatus::Running => "border-sky-900/60 bg-sky-950/60 text-sky-300",
    };
    (class, status.as_str().to_string())
}

/// Shorthand "5m ago" / "2h ago" / "3d ago" rendering. Avoids a chrono
/// dep — this is a single use and unix-seconds arithmetic is enough.
fn relative_time(unix_seconds: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let diff = now.saturating_sub(unix_seconds);
    if diff < 60 {
        return format!("{diff}s ago");
    }
    if diff < 3_600 {
        return format!("{}m ago", diff / 60);
    }
    if diff < 86_400 {
        return format!("{}h ago", diff / 3_600);
    }
    format!("{}d ago", diff / 86_400)
}
