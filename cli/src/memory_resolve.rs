//! Resolution from the user-facing `MemoryYaml` shape into the explicit
//! [`MemoryConfig`] that the memory crate's `Store` consumes.
//!
//! Lives in cli (rather than the memory crate) because resolution looks at
//! the configured `providers:` map to auto-derive the embedder and the
//! extraction model — and feature crates don't depend on each other or on
//! the providers crate. cli is the orchestrator; cross-crate composition
//! lives here.
//!
//! Auto-derivation rules (when `user_state: true` with no overrides):
//! - **Embedder**: prefer `OpenAI`'s `text-embedding-3-small` if `openai` is
//!   configured; fall back to the offline hash embedder otherwise. Voyage
//!   needs an explicit override because there's no top-level provider key
//!   for it (it's embedding-only, not a completion provider).
//! - **Extractor model**: pick the first configured provider in a small
//!   priority order (anthropic → openai → gemini → groq → deepseek →
//!   cohere) and use its known cheap "haiku-tier" model.
//!
//! All resolution failures are reported at startup so misconfigurations are
//! loud, not silent.

use std::collections::HashMap;
use std::hash::BuildHasher;
use std::path::Path;

use memory::{
    BackendConfig, DEFAULT_SQLITE_FILENAME, EmbedderConfig, EmbedderYaml, ExtractorConfig,
    MemoryConfig, MemoryYaml, ProviderModel, UserStateConfig, default_dedup_threshold,
    default_extractor_max_facts, default_hash_dims, default_openai_embedding_model,
    default_recall_k, default_voyage_model,
};
use providers::{ProviderConfig, ProviderKind};
use thiserror::Error;

/// Resolve the user-facing YAML shape into the explicit runtime config.
///
/// `state_dir` is the project's `.coulisse/` directory (next to the config
/// file). The database always lands there — there is no path knob — so state
/// stays co-located with the project and `.coulisse/` is the one thing to
/// back up or volume-mount.
///
/// # Errors
///
/// Returns an error when `user_state: true` is requested but the providers
/// map can't supply a usable extraction model, or when an explicit
/// `learn_from`/`embed_with` references an unconfigured provider.
pub fn resolve_memory<S: BuildHasher>(
    yaml: &MemoryYaml,
    providers: &HashMap<ProviderKind, ProviderConfig, S>,
    state_dir: &Path,
) -> Result<MemoryConfig, MemoryResolveError> {
    let backend = BackendConfig::Sqlite {
        path: state_dir.join(DEFAULT_SQLITE_FILENAME),
    };

    let (enabled, overrides) = yaml.user_state.parts();
    if !enabled {
        return Ok(MemoryConfig {
            backend,
            embedder: EmbedderConfig::Hash {
                dims: default_hash_dims(),
            },
            extractor: None,
            recall_k: 0,
            ..MemoryConfig::default()
        });
    }

    let embedder = resolve_embedder(overrides.and_then(|c| c.embed_with.as_ref()), providers);
    let extractor = resolve_extractor(overrides, providers)?;
    let recall_k = overrides
        .and_then(|c| c.recall_k)
        .unwrap_or_else(default_recall_k);

    Ok(MemoryConfig {
        backend,
        embedder,
        extractor: Some(extractor),
        recall_k,
        ..MemoryConfig::default()
    })
}

fn resolve_embedder<S: BuildHasher>(
    yaml: Option<&EmbedderYaml>,
    providers: &HashMap<ProviderKind, ProviderConfig, S>,
) -> EmbedderConfig {
    match yaml {
        None => auto_pick_embedder(providers),
        Some(EmbedderYaml::Hash { dims }) => EmbedderConfig::Hash {
            dims: dims.unwrap_or_else(default_hash_dims),
        },
        Some(EmbedderYaml::Openai { api_key, model }) => EmbedderConfig::Openai {
            api_key: api_key.clone(),
            model: model.clone().unwrap_or_else(default_openai_embedding_model),
        },
        Some(EmbedderYaml::Voyage { api_key, model }) => EmbedderConfig::Voyage {
            api_key: api_key.clone(),
            model: model.clone().unwrap_or_else(default_voyage_model),
        },
    }
}

/// Pick a usable embedder from the configured providers without an
/// explicit override. Prefers `OpenAI` (if configured) for real semantic
/// embeddings; falls back to the offline hash embedder so Coulisse always
/// boots, even with only Anthropic configured.
fn auto_pick_embedder<S: BuildHasher>(
    providers: &HashMap<ProviderKind, ProviderConfig, S>,
) -> EmbedderConfig {
    if providers.contains_key(&ProviderKind::Openai) {
        EmbedderConfig::Openai {
            api_key: None,
            model: default_openai_embedding_model(),
        }
    } else {
        EmbedderConfig::Hash {
            dims: default_hash_dims(),
        }
    }
}

fn resolve_extractor<S: BuildHasher>(
    overrides: Option<&UserStateConfig>,
    providers: &HashMap<ProviderKind, ProviderConfig, S>,
) -> Result<ExtractorConfig, MemoryResolveError> {
    let (provider, model) = match overrides.and_then(|c| c.learn_from.as_ref()) {
        None => auto_pick_extractor(providers)?,
        Some(ProviderModel { provider, model }) => {
            let kind = ProviderKind::parse(provider).ok_or_else(|| {
                MemoryResolveError::LearnFromUnknownProvider {
                    provider: provider.clone(),
                }
            })?;
            if !providers.contains_key(&kind) {
                return Err(MemoryResolveError::LearnFromProviderNotConfigured { provider: kind });
            }
            (provider.clone(), model.clone())
        }
    };
    Ok(ExtractorConfig {
        dedup_threshold: overrides
            .and_then(|c| c.dedup_threshold)
            .unwrap_or_else(default_dedup_threshold),
        max_facts_per_turn: overrides
            .and_then(|c| c.max_facts_per_turn)
            .unwrap_or_else(default_extractor_max_facts),
        model,
        provider,
    })
}

/// Pick the first configured provider in a stable priority order and use
/// its known cheap "haiku-tier" model for extraction. Anthropic comes
/// first because it's the most common Coulisse setup.
fn auto_pick_extractor<S: BuildHasher>(
    providers: &HashMap<ProviderKind, ProviderConfig, S>,
) -> Result<(String, String), MemoryResolveError> {
    const PRIORITY: &[(ProviderKind, &str)] = &[
        (ProviderKind::Anthropic, "claude-haiku-4-5-20251001"),
        (ProviderKind::Openai, "gpt-4o-mini"),
        (ProviderKind::Gemini, "gemini-2.0-flash-lite"),
        (ProviderKind::Groq, "llama-3.1-8b-instant"),
        (ProviderKind::Deepseek, "deepseek-chat"),
        (ProviderKind::Cohere, "command-r"),
    ];
    for (kind, model) in PRIORITY {
        if providers.contains_key(kind) {
            return Ok((kind.as_str().to_string(), (*model).to_string()));
        }
    }
    Err(MemoryResolveError::NoExtractorProvider)
}

#[derive(Debug, Error)]
pub enum MemoryResolveError {
    #[error(
        "memory.user_state.learn_from references provider '{provider}' which is not declared under `providers:`"
    )]
    LearnFromProviderNotConfigured { provider: ProviderKind },
    #[error(
        "memory.user_state.learn_from has unknown provider '{provider}' (anthropic, cohere, deepseek, gemini, groq, openai)"
    )]
    LearnFromUnknownProvider { provider: String },
    #[error(
        "memory.user_state is enabled but no provider is configured to drive extraction; declare at least one provider under `providers:` or set `memory.user_state: false`"
    )]
    NoExtractorProvider,
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory::UserStateYaml;

    fn providers_with(kinds: &[ProviderKind]) -> HashMap<ProviderKind, ProviderConfig> {
        kinds
            .iter()
            .map(|k| {
                (
                    *k,
                    ProviderConfig {
                        api_key: "test".into(),
                    },
                )
            })
            .collect()
    }

    /// Resolve against a fixed `.coulisse` state dir so tests don't repeat it.
    fn resolve(
        yaml: &MemoryYaml,
        providers: &HashMap<ProviderKind, ProviderConfig>,
    ) -> Result<MemoryConfig, MemoryResolveError> {
        resolve_memory(yaml, providers, Path::new(".coulisse"))
    }

    #[test]
    fn omitted_storage_defaults_into_state_dir() {
        let yaml = MemoryYaml::default();
        let resolved = resolve(&yaml, &providers_with(&[])).unwrap();
        match resolved.backend {
            BackendConfig::Sqlite { path } => {
                assert_eq!(path, Path::new(".coulisse").join(DEFAULT_SQLITE_FILENAME));
            }
            BackendConfig::InMemory => panic!("expected sqlite backend, got in_memory"),
        }
    }

    #[test]
    fn user_state_off_disables_recall_and_extraction() {
        let yaml = MemoryYaml::default();
        let resolved = resolve(&yaml, &providers_with(&[ProviderKind::Anthropic])).unwrap();
        assert!(resolved.extractor.is_none());
        assert_eq!(resolved.recall_k, 0);
    }

    #[test]
    fn user_state_true_with_anthropic_picks_haiku_and_hash_embedder() {
        let yaml = MemoryYaml {
            user_state: UserStateYaml::OnOff(true),
        };
        let resolved = resolve(&yaml, &providers_with(&[ProviderKind::Anthropic])).unwrap();
        let extractor = resolved.extractor.expect("extractor should be set");
        assert_eq!(extractor.provider, "anthropic");
        assert!(extractor.model.contains("haiku"));
        assert!(matches!(resolved.embedder, EmbedderConfig::Hash { .. }));
        assert_eq!(resolved.recall_k, default_recall_k());
    }

    #[test]
    fn user_state_true_with_openai_picks_openai_embedder() {
        let yaml = MemoryYaml {
            user_state: UserStateYaml::OnOff(true),
        };
        let resolved = resolve(&yaml, &providers_with(&[ProviderKind::Openai])).unwrap();
        let extractor = resolved.extractor.expect("extractor should be set");
        assert_eq!(extractor.provider, "openai");
        assert!(matches!(resolved.embedder, EmbedderConfig::Openai { .. }));
    }

    #[test]
    fn user_state_true_without_providers_fails_loudly() {
        let yaml = MemoryYaml {
            user_state: UserStateYaml::OnOff(true),
        };
        let err = resolve(&yaml, &providers_with(&[])).unwrap_err();
        assert!(matches!(err, MemoryResolveError::NoExtractorProvider));
    }

    #[test]
    fn anthropic_priority_beats_openai() {
        let yaml = MemoryYaml {
            user_state: UserStateYaml::OnOff(true),
        };
        let resolved = resolve(
            &yaml,
            &providers_with(&[ProviderKind::Openai, ProviderKind::Anthropic]),
        )
        .unwrap();
        assert_eq!(resolved.extractor.unwrap().provider, "anthropic");
    }

    #[test]
    fn explicit_learn_from_referencing_unconfigured_provider_fails() {
        let yaml: MemoryYaml = serde_yaml::from_str(
            r"
user_state:
  learn_from:
    provider: gemini
    model: gemini-2.0-flash-lite
",
        )
        .unwrap();
        let err = resolve(&yaml, &providers_with(&[ProviderKind::Anthropic])).unwrap_err();
        assert!(matches!(
            err,
            MemoryResolveError::LearnFromProviderNotConfigured { .. }
        ));
    }

    /// `storage:` is gone — the DB always lives in the state dir. A config
    /// that still tries to set it is rejected by `deny_unknown_fields`.
    #[test]
    fn storage_field_is_rejected() {
        let yaml = "storage: ./alt.db\n";
        assert!(serde_yaml::from_str::<MemoryYaml>(yaml).is_err());
    }

    #[test]
    fn old_yaml_shape_is_rejected() {
        let yaml = r"
backend:
  kind: sqlite
  path: ./old.db
embedder:
  provider: hash
";
        assert!(serde_yaml::from_str::<MemoryYaml>(yaml).is_err());
    }
}
