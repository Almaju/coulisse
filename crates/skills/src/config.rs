use std::path::PathBuf;

use serde::Deserialize;

/// YAML slice under the top-level `skills:` key. Omit the block entirely
/// and Coulisse still scans the default `./skills` directory, so dropping a
/// `SKILL.md` folder there is all it takes to add a skill — no config
/// required. Point `dir` elsewhere to load skills from a different folder.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub struct SkillsConfig {
    /// Directory holding one subdirectory per skill, each with a `SKILL.md`.
    /// Relative paths resolve against the process working directory.
    /// Defaults to `./skills`. A missing directory is not an error — it
    /// simply yields no skills.
    #[serde(default = "default_dir")]
    pub dir: PathBuf,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self { dir: default_dir() }
    }
}

fn default_dir() -> PathBuf {
    PathBuf::from("./skills")
}
