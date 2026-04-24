use crate::TokenCount;

#[derive(Clone, Copy, Debug)]
pub struct MemoryConfig {
    pub context_budget: TokenCount,
    pub memory_budget_fraction: f32,
    pub recall_k: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            context_budget: TokenCount(8_000),
            memory_budget_fraction: 0.1,
            recall_k: 5,
        }
    }
}
