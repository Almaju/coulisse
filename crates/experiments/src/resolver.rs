use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use coulisse_core::{AgentResolver, ScoreLookup, UserId};

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
    pub fn new(router: Arc<ExperimentRouter>, scores: Option<Arc<dyn ScoreLookup>>) -> Self {
        Self { router, scores }
    }
}

impl AgentResolver for ExperimentResolver {
    fn resolve<'a>(
        &'a self,
        name: &'a str,
        user_id: UserId,
    ) -> Pin<Box<dyn Future<Output = String> + Send + 'a>> {
        Box::pin(async move {
            let scores = if let (Some(store), Some((judge, criterion, since))) =
                (self.scores.as_ref(), self.router.bandit_query(name))
            {
                store
                    .mean_scores_by_agent(&judge, &criterion, since)
                    .await
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let resolved = self.router.resolve_with_scores(name, user_id, &scores);
            resolved.agent.into_owned()
        })
    }

    fn purpose(&self, name: &str) -> Option<String> {
        self.router.get(name).and_then(|exp| exp.purpose.clone())
    }
}
