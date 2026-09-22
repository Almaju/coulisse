use std::sync::Arc;

use coulisse_core::{AgentResolver, BoxFuture, ScoreLookup, UserId};

use crate::ExperimentRouter;

/// Composes `ExperimentRouter` with an optional `ScoreLookup` to satisfy
/// the `AgentResolver` trait. Agents holds an `Arc<dyn AgentResolver>` —
/// the resolver implementation lives here so agents itself never sees
/// `ExperimentRouter` or any experiment types.
pub struct ExperimentResolver {
    router: Arc<ExperimentRouter>,
    /// Required for bandit-strategy resolution (which needs recent mean
    /// scores at call time). When `None`, bandit experiments fall back to
    /// forced exploration.
    scores: Option<Arc<dyn ScoreLookup>>,
}

impl ExperimentResolver {
    #[must_use]
    pub fn new(router: Arc<ExperimentRouter>, scores: Option<Arc<dyn ScoreLookup>>) -> Self {
        Self { router, scores }
    }
}

impl AgentResolver for ExperimentResolver {
    fn purpose(&self, name: &str) -> Option<String> {
        self.router.get(name).and_then(|exp| exp.purpose.clone())
    }

    fn resolve<'a>(&'a self, name: &'a str, user_id: UserId) -> BoxFuture<'a, String> {
        Box::pin(async move {
            let scores = match (self.scores.as_ref(), self.router.bandit_query(name)) {
                (Some(store), Some(query)) => {
                    store.mean_scores_by_agent(query).await.unwrap_or_default()
                }
                _ => Vec::new(),
            };
            let resolved = self.router.resolve_with_scores(name, user_id, &scores);
            resolved.agent.into_owned()
        })
    }
}
