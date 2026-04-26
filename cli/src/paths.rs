//! State file locations resolved relative to the config file.
//!
//! Every running instance of coulisse keeps its PID and detached log
//! under a `.coulisse/` directory next to the YAML it was started with.
//! Mirroring the YAML location (rather than `~/.coulisse/<hash>/`) keeps
//! state co-located with the project — `cd && coulisse stop` just
//! works, and removing the project directory removes the state.

use std::path::{Path, PathBuf};

pub struct StatePaths {
    pub config: PathBuf,
    pub dir: PathBuf,
    pub log: PathBuf,
    pub pid: PathBuf,
}

impl StatePaths {
    pub fn for_config(config: impl AsRef<Path>) -> Self {
        let config = config.as_ref().to_path_buf();
        let parent = config
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let dir = parent.join(".coulisse");
        Self {
            log: dir.join("coulisse.log"),
            pid: dir.join("coulisse.pid"),
            config,
            dir,
        }
    }
}
