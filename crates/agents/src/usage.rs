#[derive(Clone, Copy, Debug, Default)]
pub struct Usage {
    pub cache_creation_input_tokens: u64,
    pub cached_input_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

impl From<rig::completion::Usage> for Usage {
    fn from(u: rig::completion::Usage) -> Self {
        Self {
            cache_creation_input_tokens: u.cache_creation_input_tokens,
            cached_input_tokens: u.cached_input_tokens,
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            total_tokens: u.total_tokens,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Completion {
    pub text: String,
    pub usage: Usage,
}
