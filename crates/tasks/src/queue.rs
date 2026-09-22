use coulisse_core::migrate::{self, SchemaMigrator};
use coulisse_core::{
    BoxFuture, TaskId, TaskQueue, TaskQueueError, TaskState, TaskStatus, TaskStatusError,
    TaskSubmission, TaskSummary, UserId, i64_to_u64, now_secs, u64_to_i64,
};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::error::TaskError;

struct Schema;

impl SchemaMigrator for Schema {
    const NAME: &'static str = "tasks";
    const SCHEMA: &'static str = include_str!("../migrations/schema.sql");
    const VERSIONS: &'static [&'static str] = &["0.1.0"];
}

#[derive(Clone, Debug)]
pub struct Task {
    pub agent: String,
    pub created_at: u64,
    pub error: Option<String>,
    pub finished_at: Option<u64>,
    pub id: TaskId,
    pub prompt: String,
    pub result: Option<String>,
    pub started_at: Option<u64>,
    pub state: TaskState,
    pub user_id: UserId,
}

/// Persistent queue for fire-and-forget agent runs. Workers in `cli` pull
/// `next_runnable` in a loop and write the outcome back via `mark_done` or
/// `mark_errored`. Submitters (the `dispatch_task` tool, cron triggers,
/// webhook triggers) use `submit` — the trait method — and forget the
/// result; the model that submitted gets back only a `TaskId` to refer to
/// the task later.
pub struct Tasks {
    pool: SqlitePool,
}

impl Tasks {
    /// Apply the `tasks` schema to `pool` and return a queue handle.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying migration fails.
    pub async fn open(pool: SqlitePool) -> Result<Self, TaskError> {
        migrate::run(&pool, &Schema).await?;
        Ok(Self { pool })
    }

    /// Insert a new task in the `queued` state.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying database write fails.
    pub async fn enqueue(&self, submission: TaskSubmission<'_>) -> Result<TaskId, TaskError> {
        let id = TaskId::new();
        let now = u64_to_i64(now_secs());
        sqlx::query(
            "INSERT INTO tasks (agent, created_at, id, prompt, state, user_id) \
             VALUES (?, ?, ?, ?, 'queued', ?)",
        )
        .bind(submission.agent)
        .bind(now)
        .bind(id.0.to_string())
        .bind(submission.prompt)
        .bind(submission.user_id.0.to_string())
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Look up a task by id. Returns `None` if no such task exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying database read fails.
    pub async fn get(&self, id: TaskId) -> Result<Option<Task>, TaskError> {
        let row = sqlx::query_as::<_, TaskRow>(
            "SELECT agent, created_at, error, finished_at, id, prompt, result, \
                    started_at, state, user_id \
             FROM tasks WHERE id = ?",
        )
        .bind(id.0.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(TaskRow::into_task).transpose()
    }

    /// Transition `id` to `done` with the agent's final reply as `result`.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying database write fails.
    pub async fn mark_done(&self, id: TaskId, result: &str) -> Result<(), TaskError> {
        let now = u64_to_i64(now_secs());
        sqlx::query("UPDATE tasks SET state = 'done', finished_at = ?, result = ? WHERE id = ?")
            .bind(now)
            .bind(result)
            .bind(id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Transition `id` to `errored` with the displayed failure reason.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying database write fails.
    pub async fn mark_errored(&self, id: TaskId, error: &str) -> Result<(), TaskError> {
        let now = u64_to_i64(now_secs());
        sqlx::query("UPDATE tasks SET state = 'errored', finished_at = ?, error = ? WHERE id = ?")
            .bind(now)
            .bind(error)
            .bind(id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Atomically claim the oldest queued task and transition it to
    /// `running`. Returns `None` when no task is ready.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying database operation fails.
    pub async fn next_runnable(&self) -> Result<Option<Task>, TaskError> {
        let mut tx = self.pool.begin().await?;
        let Some(row) = sqlx::query_as::<_, TaskRow>(
            "SELECT agent, created_at, error, finished_at, id, prompt, result, \
                    started_at, state, user_id \
             FROM tasks WHERE state = 'queued' ORDER BY created_at LIMIT 1",
        )
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(None);
        };
        let now = u64_to_i64(now_secs());
        let rows = sqlx::query(
            "UPDATE tasks SET state = 'running', started_at = ? \
             WHERE id = ? AND state = 'queued'",
        )
        .bind(now)
        .bind(&row.id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        if rows == 0 {
            // Another worker raced us; let the caller try again.
            return Ok(None);
        }
        let mut task = row.into_task()?;
        task.state = TaskState::Running;
        task.started_at = Some(i64_to_u64(now));
        Ok(Some(task))
    }

    /// Most recent tasks, newest first. Used by the `/admin/live` page to
    /// show what's queued, running, and finished across all users.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying database read fails.
    pub async fn recent(&self, limit: u32) -> Result<Vec<Task>, TaskError> {
        let rows = sqlx::query_as::<_, TaskRow>(
            "SELECT agent, created_at, error, finished_at, id, prompt, result, \
                    started_at, state, user_id \
             FROM tasks ORDER BY created_at DESC LIMIT ?",
        )
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TaskRow::into_task).collect()
    }
}

impl TaskQueue for Tasks {
    fn submit<'a>(
        &'a self,
        submission: TaskSubmission<'a>,
    ) -> BoxFuture<'a, Result<TaskId, TaskQueueError>> {
        Box::pin(async move {
            self.enqueue(submission)
                .await
                .map_err(|e| TaskQueueError::new(e.to_string()))
        })
    }
}

impl TaskStatus for Tasks {
    fn reap_stale_running<'a>(
        &'a self,
        started_before_secs: u64,
        reason: &'a str,
    ) -> BoxFuture<'a, Result<u64, TaskStatusError>> {
        Box::pin(async move {
            let now = u64_to_i64(now_secs());
            let cutoff = u64_to_i64(started_before_secs);
            sqlx::query(
                "UPDATE tasks SET state = 'errored', finished_at = ?, error = ? \
                 WHERE state = 'running' AND (started_at IS NULL OR started_at < ?)",
            )
            .bind(now)
            .bind(reason)
            .bind(cutoff)
            .execute(&self.pool)
            .await
            .map(|r| r.rows_affected())
            .map_err(|e| TaskStatusError::new(e.to_string()))
        })
    }

    fn recent(&self, limit: u32) -> BoxFuture<'_, Result<Vec<TaskSummary>, TaskStatusError>> {
        Box::pin(async move {
            let rows = self
                .recent(limit)
                .await
                .map_err(|e| TaskStatusError::new(e.to_string()))?;
            Ok(rows.into_iter().map(TaskSummary::from).collect())
        })
    }
}

impl From<Task> for TaskSummary {
    fn from(task: Task) -> Self {
        Self {
            agent: task.agent,
            created_at: task.created_at,
            error: task.error,
            finished_at: task.finished_at,
            id: task.id,
            prompt: task.prompt,
            result: task.result,
            started_at: task.started_at,
            state: task.state,
        }
    }
}

#[derive(sqlx::FromRow)]
struct TaskRow {
    agent: String,
    created_at: i64,
    error: Option<String>,
    finished_at: Option<i64>,
    id: String,
    prompt: String,
    result: Option<String>,
    started_at: Option<i64>,
    state: String,
    user_id: String,
}

impl TaskRow {
    fn into_task(self) -> Result<Task, TaskError> {
        let state = self
            .state
            .parse::<TaskState>()
            .map_err(|source| TaskError::UnknownState {
                id: self.id.clone(),
                source,
            })?;
        let task_uuid = Uuid::parse_str(&self.id).map_err(|source| TaskError::InvalidId {
            id: self.id.clone(),
            source,
        })?;
        let user_uuid =
            Uuid::parse_str(&self.user_id).map_err(|source| TaskError::InvalidUserId {
                id: self.id.clone(),
                source,
                user_id: self.user_id.clone(),
            })?;
        Ok(Task {
            agent: self.agent,
            created_at: i64_to_u64(self.created_at),
            error: self.error,
            finished_at: self.finished_at.map(i64_to_u64),
            id: TaskId(task_uuid),
            prompt: self.prompt,
            result: self.result,
            started_at: self.started_at.map(i64_to_u64),
            state,
            user_id: UserId(user_uuid),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqliteConnectOptions;
    use std::str::FromStr;

    fn submission<'a>(agent: &'a str, prompt: &'a str, user_id: UserId) -> TaskSubmission<'a> {
        TaskSubmission {
            agent,
            prompt,
            user_id,
        }
    }

    async fn queue() -> Tasks {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:").unwrap();
        let pool = SqlitePool::connect_with(opts).await.unwrap();
        Tasks::open(pool).await.unwrap()
    }

    #[tokio::test]
    async fn enqueue_and_pick() {
        let q = queue().await;
        let user = UserId::new();
        let id = q
            .enqueue(submission("pm", "do the thing", user))
            .await
            .unwrap();

        let picked = q.next_runnable().await.unwrap().unwrap();
        assert_eq!(picked.id, id);
        assert_eq!(picked.agent, "pm");
        assert_eq!(picked.prompt, "do the thing");
        assert_eq!(picked.state, TaskState::Running);
        assert!(picked.started_at.is_some());

        // Same task shouldn't be picked again.
        assert!(q.next_runnable().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn mark_done_persists_result() {
        let q = queue().await;
        let id = q
            .enqueue(submission("pm", "p", UserId::new()))
            .await
            .unwrap();
        let _ = q.next_runnable().await.unwrap().unwrap();
        q.mark_done(id, "all good").await.unwrap();

        let t = q.get(id).await.unwrap().unwrap();
        assert_eq!(t.state, TaskState::Done);
        assert_eq!(t.result.as_deref(), Some("all good"));
        assert!(t.finished_at.is_some());
        assert!(t.error.is_none());
    }

    #[tokio::test]
    async fn mark_errored_persists_reason() {
        let q = queue().await;
        let id = q
            .enqueue(submission("pm", "p", UserId::new()))
            .await
            .unwrap();
        let _ = q.next_runnable().await.unwrap().unwrap();
        q.mark_errored(id, "provider timeout").await.unwrap();

        let t = q.get(id).await.unwrap().unwrap();
        assert_eq!(t.state, TaskState::Errored);
        assert_eq!(t.error.as_deref(), Some("provider timeout"));
        assert!(t.result.is_none());
    }

    #[tokio::test]
    async fn fifo_order() {
        let q = queue().await;
        let user = UserId::new();
        let first = q.enqueue(submission("a", "1", user)).await.unwrap();
        let second = q.enqueue(submission("b", "2", user)).await.unwrap();

        let p1 = q.next_runnable().await.unwrap().unwrap();
        let p2 = q.next_runnable().await.unwrap().unwrap();
        assert_eq!(p1.id, first);
        assert_eq!(p2.id, second);
    }

    #[tokio::test]
    async fn empty_queue_returns_none() {
        let q = queue().await;
        assert!(q.next_runnable().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn submit_via_trait_returns_task_id() {
        let q = queue().await;
        let id = TaskQueue::submit(&q, submission("pm", "prompt", UserId::new()))
            .await
            .unwrap();
        let t = q.get(id).await.unwrap().unwrap();
        assert_eq!(t.state, TaskState::Queued);
    }

    #[tokio::test]
    async fn task_status_recent_returns_summaries() {
        let q = queue().await;
        let user = UserId::new();
        q.enqueue(submission("pm", "1", user)).await.unwrap();
        q.enqueue(submission("coder", "2", user)).await.unwrap();

        let summaries = TaskStatus::recent(&q, 10).await.unwrap();
        assert_eq!(summaries.len(), 2);
        let agents: Vec<_> = summaries.iter().map(|s| s.agent.as_str()).collect();
        assert!(agents.contains(&"pm"));
        assert!(agents.contains(&"coder"));
        assert!(summaries.iter().all(|s| s.state == TaskState::Queued));
    }

    #[tokio::test]
    async fn reap_stale_running_marks_old_running_errored() {
        let q = queue().await;
        let id = q
            .enqueue(submission("pm", "p", UserId::new()))
            .await
            .unwrap();
        let _ = q.next_runnable().await.unwrap().unwrap();

        let touched = TaskStatus::reap_stale_running(&q, u64::MAX, "process restarted")
            .await
            .unwrap();
        assert_eq!(touched, 1);

        let t = q.get(id).await.unwrap().unwrap();
        assert_eq!(t.state, TaskState::Errored);
        assert_eq!(t.error.as_deref(), Some("process restarted"));
    }

    #[tokio::test]
    async fn reap_stale_running_leaves_recent_running_alone() {
        let q = queue().await;
        q.enqueue(submission("pm", "p", UserId::new()))
            .await
            .unwrap();
        let _ = q.next_runnable().await.unwrap().unwrap();

        // started_before_secs = 0 → only tasks that started before epoch are reaped.
        let touched = TaskStatus::reap_stale_running(&q, 0, "should not fire")
            .await
            .unwrap();
        assert_eq!(touched, 0);
    }
}
