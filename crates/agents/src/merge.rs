use std::collections::{HashMap, HashSet};

use crate::AgentConfig;
use crate::store::DynamicRow;

/// Where the resolved version of an agent came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source {
    /// Created via admin/HTTP, no YAML entry of this name.
    Dynamic,
    /// DB row shadowing a YAML entry of the same name.
    Override,
    /// Pure YAML entry, no DB row.
    Yaml,
}

#[derive(Clone, Debug)]
pub struct MergedAgent {
    pub config: AgentConfig,
    pub source: Source,
}

/// Counts of each row class produced by a merge. Used by startup logging
/// and the admin overview.
#[derive(Clone, Copy, Debug, Default)]
pub struct MergeReport {
    pub dynamic_count: usize,
    pub override_count: usize,
    pub tombstone_count: usize,
    pub yaml_count: usize,
}

/// Resolve YAML and DB into the effective agent list.
///
/// Rule: for each name, a DB row wins over a YAML entry. Tombstones drop
/// the YAML entry from the result. Pure YAML entries pass through. The
/// returned list is sorted alphabetically by name.
pub fn merge(yaml: &[AgentConfig], db: &[DynamicRow]) -> (Vec<MergedAgent>, MergeReport) {
    let db_by_name: HashMap<&str, &DynamicRow> = db.iter().map(|r| (r.name.as_str(), r)).collect();

    let mut merged: Vec<MergedAgent> = Vec::with_capacity(yaml.len() + db.len());
    let mut report = MergeReport::default();

    for cfg in yaml {
        match db_by_name.get(cfg.name.as_str()) {
            None => {
                merged.push(MergedAgent {
                    config: cfg.clone(),
                    source: Source::Yaml,
                });
                report.yaml_count += 1;
            }
            Some(row) if row.disabled => {
                report.tombstone_count += 1;
            }
            Some(row) => {
                if let Some(db_cfg) = &row.config {
                    merged.push(MergedAgent {
                        config: db_cfg.clone(),
                        source: Source::Override,
                    });
                    report.override_count += 1;
                }
            }
        }
    }

    let yaml_names: HashSet<&str> = yaml.iter().map(|c| c.name.as_str()).collect();
    for row in db {
        if yaml_names.contains(row.name.as_str()) {
            continue;
        }
        if row.disabled {
            report.tombstone_count += 1;
            continue;
        }
        if let Some(cfg) = &row.config {
            merged.push(MergedAgent {
                config: cfg.clone(),
                source: Source::Dynamic,
            });
            report.dynamic_count += 1;
        }
    }

    merged.sort_by(|a, b| a.config.name.cmp(&b.config.name));
    (merged, report)
}

/// Source label for the admin UI. Wider than [`Source`] because the admin
/// also surfaces tombstoned rows so operators can re-enable them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdminSource {
    /// DB row, active, no YAML entry of the same name.
    Dynamic,
    /// DB row, active, shadows a YAML entry of the same name.
    Override,
    /// DB row, disabled (tombstone). May or may not have a backing YAML
    /// entry; check `AdminAgent::yaml_backed`.
    Tombstoned,
    /// YAML entry, no DB row.
    Yaml,
}

/// One row in the admin agents list. Tombstones appear here (with
/// `config = None`) so operators can re-enable them.
#[derive(Clone, Debug)]
pub struct AdminAgent {
    /// Effective config — `Some` for everything except tombstones. For
    /// `Override` this is the DB version (what the runtime uses); for
    /// `Yaml` it's the YAML version.
    pub config: Option<AgentConfig>,
    pub name: String,
    pub source: AdminSource,
    /// True when YAML declares this name. Drives the admin UX — a
    /// tombstone with `yaml_backed = true` means "remove tombstone →
    /// YAML reasserts," whereas `yaml_backed = false` means "remove
    /// row → it's gone."
    pub yaml_backed: bool,
}

/// Build the row list for the admin UI. Includes tombstones. Sorted by
/// name. The runtime still reads from `merge` — this is a separate, wider
/// view for operators.
pub fn admin_view(yaml: &[AgentConfig], db: &[DynamicRow]) -> Vec<AdminAgent> {
    let db_by_name: HashMap<&str, &DynamicRow> = db.iter().map(|r| (r.name.as_str(), r)).collect();
    let yaml_by_name: HashMap<&str, &AgentConfig> =
        yaml.iter().map(|c| (c.name.as_str(), c)).collect();

    let mut out: Vec<AdminAgent> = Vec::with_capacity(yaml.len() + db.len());

    for cfg in yaml {
        let row = db_by_name.get(cfg.name.as_str());
        match row {
            None => out.push(AdminAgent {
                config: Some(cfg.clone()),
                name: cfg.name.clone(),
                source: AdminSource::Yaml,
                yaml_backed: true,
            }),
            Some(r) if r.disabled => out.push(AdminAgent {
                config: None,
                name: cfg.name.clone(),
                source: AdminSource::Tombstoned,
                yaml_backed: true,
            }),
            Some(r) => out.push(AdminAgent {
                config: r.config.clone(),
                name: cfg.name.clone(),
                source: AdminSource::Override,
                yaml_backed: true,
            }),
        }
    }

    for row in db {
        if yaml_by_name.contains_key(row.name.as_str()) {
            continue;
        }
        if row.disabled {
            out.push(AdminAgent {
                config: None,
                name: row.name.clone(),
                source: AdminSource::Tombstoned,
                yaml_backed: false,
            });
        } else if let Some(cfg) = &row.config {
            out.push(AdminAgent {
                config: Some(cfg.clone()),
                name: row.name.clone(),
                source: AdminSource::Dynamic,
                yaml_backed: false,
            });
        }
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use providers::ProviderKind;

    fn cfg(name: &str, model: &str) -> AgentConfig {
        AgentConfig {
            judges: vec![],
            mcp_tools: vec![],
            model: model.into(),
            name: name.into(),
            preamble: String::new(),
            provider: ProviderKind::Openai,
            purpose: None,
            subagents: vec![],
        }
    }

    fn active(name: &str, model: &str) -> DynamicRow {
        DynamicRow {
            config: Some(cfg(name, model)),
            created_at: 0,
            disabled: false,
            name: name.into(),
            updated_at: 0,
        }
    }

    fn tombstone(name: &str) -> DynamicRow {
        DynamicRow {
            config: None,
            created_at: 0,
            disabled: true,
            name: name.into(),
            updated_at: 0,
        }
    }

    #[test]
    fn yaml_only_passes_through() {
        let (out, report) = merge(&[cfg("alice", "gpt-4")], &[]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, Source::Yaml);
        assert_eq!(out[0].config.model, "gpt-4");
        assert_eq!(report.yaml_count, 1);
        assert_eq!(report.override_count, 0);
        assert_eq!(report.dynamic_count, 0);
        assert_eq!(report.tombstone_count, 0);
    }

    #[test]
    fn db_only_appears_as_dynamic() {
        let (out, report) = merge(&[], &[active("bob", "gpt-5")]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, Source::Dynamic);
        assert_eq!(report.dynamic_count, 1);
    }

    #[test]
    fn db_active_shadows_yaml() {
        let (out, report) = merge(
            &[cfg("alice", "yaml-model")],
            &[active("alice", "db-model")],
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, Source::Override);
        assert_eq!(out[0].config.model, "db-model");
        assert_eq!(report.override_count, 1);
        assert_eq!(report.yaml_count, 0);
    }

    #[test]
    fn tombstone_drops_yaml_entry() {
        let (out, report) = merge(&[cfg("alice", "yaml-model")], &[tombstone("alice")]);
        assert!(out.is_empty());
        assert_eq!(report.tombstone_count, 1);
    }

    #[test]
    fn orphan_tombstone_is_counted_but_invisible() {
        let (out, report) = merge(&[], &[tombstone("ghost")]);
        assert!(out.is_empty());
        assert_eq!(report.tombstone_count, 1);
    }

    #[test]
    fn output_is_sorted_alphabetically() {
        let yaml = vec![cfg("charlie", "m"), cfg("alice", "m")];
        let db = vec![active("bob", "m")];
        let (out, _) = merge(&yaml, &db);
        let names: Vec<&str> = out.iter().map(|m| m.config.name.as_str()).collect();
        assert_eq!(names, vec!["alice", "bob", "charlie"]);
    }

    #[test]
    fn admin_view_covers_all_states() {
        let yaml = vec![cfg("alice", "y"), cfg("bob", "y"), cfg("charlie", "y")];
        let db = vec![
            active("alice", "db"), // Override
            tombstone("bob"),      // Tombstoned with yaml_backed
            active("dave", "db"),  // Dynamic
            tombstone("ghost"),    // Tombstoned without yaml_backed
        ];
        let rows = admin_view(&yaml, &db);
        let by_name: std::collections::HashMap<_, _> =
            rows.iter().map(|r| (r.name.as_str(), r)).collect();

        let alice = by_name.get("alice").unwrap();
        assert_eq!(alice.source, AdminSource::Override);
        assert!(alice.yaml_backed);
        assert_eq!(alice.config.as_ref().unwrap().model, "db");

        let bob = by_name.get("bob").unwrap();
        assert_eq!(bob.source, AdminSource::Tombstoned);
        assert!(bob.yaml_backed);
        assert!(bob.config.is_none());

        let charlie = by_name.get("charlie").unwrap();
        assert_eq!(charlie.source, AdminSource::Yaml);
        assert!(charlie.yaml_backed);

        let dave = by_name.get("dave").unwrap();
        assert_eq!(dave.source, AdminSource::Dynamic);
        assert!(!dave.yaml_backed);

        let ghost = by_name.get("ghost").unwrap();
        assert_eq!(ghost.source, AdminSource::Tombstoned);
        assert!(!ghost.yaml_backed);
    }

    #[test]
    fn admin_view_is_sorted_by_name() {
        let yaml = vec![cfg("zeta", "y"), cfg("alpha", "y")];
        let db = vec![active("mike", "db"), tombstone("nova")];
        let rows = admin_view(&yaml, &db);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mike", "nova", "zeta"]);
    }

    #[test]
    fn mixed_scenario_counts_correctly() {
        let yaml = vec![cfg("alice", "y"), cfg("bob", "y"), cfg("charlie", "y")];
        let db = vec![
            active("alice", "db"), // override
            tombstone("bob"),      // tombstone
            active("dave", "db"),  // dynamic
            tombstone("ghost"),    // orphan tombstone
        ];
        let (out, report) = merge(&yaml, &db);

        let by_name: std::collections::HashMap<_, _> = out
            .iter()
            .map(|m| (m.config.name.as_str(), m.source))
            .collect();
        assert_eq!(by_name.get("alice"), Some(&Source::Override));
        assert_eq!(by_name.get("bob"), None);
        assert_eq!(by_name.get("charlie"), Some(&Source::Yaml));
        assert_eq!(by_name.get("dave"), Some(&Source::Dynamic));
        assert_eq!(out.len(), 3);

        assert_eq!(report.yaml_count, 1);
        assert_eq!(report.override_count, 1);
        assert_eq!(report.dynamic_count, 1);
        assert_eq!(report.tombstone_count, 2);
    }
}
