use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use coulisse_core::{AgentResolver, ResolveRequest, ScoreLookup, ScoreQuery};

use crate::{ExperimentRouter, ResolveQuery};

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

    fn resolve<'a>(
        &'a self,
        request: ResolveRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = String> + Send + 'a>> {
        let ResolveRequest { name, user_id } = request;
        Box::pin(async move {
            let scores = if let (Some(store), Some((judge, criterion, since))) =
                (self.scores.as_ref(), self.router.bandit_query(name))
            {
                store
                    .mean_scores_by_agent(ScoreQuery {
                        criterion: &criterion,
                        judge: &judge,
                        since,
                    })
                    .await
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let resolved = self.router.resolve(ResolveQuery {
                name,
                scores: &scores,
                user_id,
            });
            resolved.agent.into_owned()
        })
    }
}
