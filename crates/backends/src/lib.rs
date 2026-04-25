//! Per-provider LLM client wrappers.
//!
//! `backends` is the only crate that depends on Rig. It owns the
//! `ProviderKind` enum (the YAML name of a provider), `ProviderConfig`
//! (its API key), and the `Backend` enum that wraps one Rig client per
//! provider. Higher-level orchestration (agent loops, tool dispatch,
//! streaming) lives in `agents`; this crate is intentionally a thin
//! adapter so swapping a provider library only touches one crate.

use std::collections::HashMap;

use rig::providers::{anthropic, cohere, deepseek, gemini, groq, openai};
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Anthropic,
    Cohere,
    Deepseek,
    Gemini,
    Groq,
    Openai,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Cohere => "cohere",
            Self::Deepseek => "deepseek",
            Self::Gemini => "gemini",
            Self::Groq => "groq",
            Self::Openai => "openai",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "anthropic" => Some(Self::Anthropic),
            "cohere" => Some(Self::Cohere),
            "deepseek" => Some(Self::Deepseek),
            "gemini" => Some(Self::Gemini),
            "groq" => Some(Self::Groq),
            "openai" => Some(Self::Openai),
            _ => None,
        }
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProviderConfig {
    pub api_key: String,
}

/// One Rig client per supported provider. Variants are public so callers
/// (currently `agents`) can match and dispatch to provider-specific
/// completion paths — the multi-turn loop is generic over
/// `CompletionClient` and the variants give it a concrete client.
pub enum Backend {
    Anthropic(anthropic::Client),
    Cohere(cohere::Client),
    Deepseek(deepseek::Client),
    Gemini(gemini::Client),
    Groq(groq::Client),
    Openai(openai::Client),
}

impl Backend {
    pub fn new(kind: ProviderKind, api_key: &str) -> Result<Self, ClientInitError> {
        let result = match kind {
            ProviderKind::Anthropic => anthropic::Client::new(api_key).map(Backend::Anthropic),
            ProviderKind::Cohere => cohere::Client::new(api_key).map(Backend::Cohere),
            ProviderKind::Deepseek => deepseek::Client::new(api_key).map(Backend::Deepseek),
            ProviderKind::Gemini => gemini::Client::new(api_key).map(Backend::Gemini),
            ProviderKind::Groq => groq::Client::new(api_key).map(Backend::Groq),
            ProviderKind::Openai => openai::Client::new(api_key).map(Backend::Openai),
        };
        result.map_err(|source| ClientInitError {
            provider: kind,
            source,
        })
    }
}

/// Lookup table over the configured providers. Holds one `Backend` per
/// `ProviderKind` declared in YAML. `agents` consults this when running
/// an agent's turn.
pub struct Backends {
    by_kind: HashMap<ProviderKind, Backend>,
}

impl Backends {
    pub fn new(providers: HashMap<ProviderKind, ProviderConfig>) -> Result<Self, ClientInitError> {
        let mut by_kind = HashMap::with_capacity(providers.len());
        for (kind, provider) in providers {
            by_kind.insert(kind, Backend::new(kind, &provider.api_key)?);
        }
        Ok(Self { by_kind })
    }

    pub fn get(&self, kind: ProviderKind) -> Option<&Backend> {
        self.by_kind.get(&kind)
    }

    pub fn contains(&self, kind: ProviderKind) -> bool {
        self.by_kind.contains_key(&kind)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("failed to initialize {provider} client: {source}")]
pub struct ClientInitError {
    pub provider: ProviderKind,
    #[source]
    pub source: rig::http_client::Error,
}
