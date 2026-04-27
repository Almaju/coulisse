use std::collections::{HashMap, HashSet};

use crate::SmokeTestConfig;
use crate::store::DynamicSmokeRow;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source {
    Dynamic,
    Override,
    Yaml,
}

#[derive(Clone, Debug)]
pub struct MergedSmoke {
    pub config: SmokeTestConfig,
    pub source: Source,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MergeReport {
    pub dynamic_count: usize,
    pub override_count: usize,
    pub tombstone_count: usize,
    pub yaml_count: usize,
}

pub fn merge(yaml: &[SmokeTestConfig], db: &[DynamicSmokeRow]) -> (Vec<MergedSmoke>, MergeReport) {
    let db_by_name: HashMap<&str, &DynamicSmokeRow> =
        db.iter().map(|r| (r.name.as_str(), r)).collect();

    let mut merged: Vec<MergedSmoke> = Vec::with_capacity(yaml.len() + db.len());
    let mut report = MergeReport::default();

    for cfg in yaml {
        match db_by_name.get(cfg.name.as_str()) {
            None => {
                merged.push(MergedSmoke {
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
                    merged.push(MergedSmoke {
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
            merged.push(MergedSmoke {
                config: cfg.clone(),
                source: Source::Dynamic,
            });
            report.dynamic_count += 1;
        }
    }

    merged.sort_by(|a, b| a.config.name.cmp(&b.config.name));
    (merged, report)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdminSource {
    Dynamic,
    Override,
    Tombstoned,
    Yaml,
}

#[derive(Clone, Debug)]
pub struct AdminSmoke {
    pub config: Option<SmokeTestConfig>,
    pub name: String,
    pub source: AdminSource,
    pub yaml_backed: bool,
}

pub fn admin_view(yaml: &[SmokeTestConfig], db: &[DynamicSmokeRow]) -> Vec<AdminSmoke> {
    let db_by_name: HashMap<&str, &DynamicSmokeRow> =
        db.iter().map(|r| (r.name.as_str(), r)).collect();
    let yaml_by_name: HashMap<&str, &SmokeTestConfig> =
        yaml.iter().map(|c| (c.name.as_str(), c)).collect();

    let mut out: Vec<AdminSmoke> = Vec::with_capacity(yaml.len() + db.len());

    for cfg in yaml {
        let row = db_by_name.get(cfg.name.as_str());
        match row {
            None => out.push(AdminSmoke {
                config: Some(cfg.clone()),
                name: cfg.name.clone(),
                source: AdminSource::Yaml,
                yaml_backed: true,
            }),
            Some(r) if r.disabled => out.push(AdminSmoke {
                config: None,
                name: cfg.name.clone(),
                source: AdminSource::Tombstoned,
                yaml_backed: true,
            }),
            Some(r) => out.push(AdminSmoke {
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
            out.push(AdminSmoke {
                config: None,
                name: row.name.clone(),
                source: AdminSource::Tombstoned,
                yaml_backed: false,
            });
        } else if let Some(cfg) = &row.config {
            out.push(AdminSmoke {
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
