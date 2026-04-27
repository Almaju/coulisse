use std::collections::{HashMap, HashSet};

use crate::ExperimentConfig;
use crate::store::DynamicExperimentRow;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source {
    Dynamic,
    Override,
    Yaml,
}

#[derive(Clone, Debug)]
pub struct MergedExperiment {
    pub config: ExperimentConfig,
    pub source: Source,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MergeReport {
    pub dynamic_count: usize,
    pub override_count: usize,
    pub tombstone_count: usize,
    pub yaml_count: usize,
}

pub fn merge(
    yaml: &[ExperimentConfig],
    db: &[DynamicExperimentRow],
) -> (Vec<MergedExperiment>, MergeReport) {
    let db_by_name: HashMap<&str, &DynamicExperimentRow> =
        db.iter().map(|r| (r.name.as_str(), r)).collect();

    let mut merged: Vec<MergedExperiment> = Vec::with_capacity(yaml.len() + db.len());
    let mut report = MergeReport::default();

    for cfg in yaml {
        match db_by_name.get(cfg.name.as_str()) {
            None => {
                merged.push(MergedExperiment {
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
                    merged.push(MergedExperiment {
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
            merged.push(MergedExperiment {
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
pub struct AdminExperiment {
    pub config: Option<ExperimentConfig>,
    pub name: String,
    pub source: AdminSource,
    pub yaml_backed: bool,
}

pub fn admin_view(yaml: &[ExperimentConfig], db: &[DynamicExperimentRow]) -> Vec<AdminExperiment> {
    let db_by_name: HashMap<&str, &DynamicExperimentRow> =
        db.iter().map(|r| (r.name.as_str(), r)).collect();
    let yaml_by_name: HashMap<&str, &ExperimentConfig> =
        yaml.iter().map(|c| (c.name.as_str(), c)).collect();

    let mut out: Vec<AdminExperiment> = Vec::with_capacity(yaml.len() + db.len());

    for cfg in yaml {
        let row = db_by_name.get(cfg.name.as_str());
        match row {
            None => out.push(AdminExperiment {
                config: Some(cfg.clone()),
                name: cfg.name.clone(),
                source: AdminSource::Yaml,
                yaml_backed: true,
            }),
            Some(r) if r.disabled => out.push(AdminExperiment {
                config: None,
                name: cfg.name.clone(),
                source: AdminSource::Tombstoned,
                yaml_backed: true,
            }),
            Some(r) => out.push(AdminExperiment {
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
            out.push(AdminExperiment {
                config: None,
                name: row.name.clone(),
                source: AdminSource::Tombstoned,
                yaml_backed: false,
            });
        } else if let Some(cfg) = &row.config {
            out.push(AdminExperiment {
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
