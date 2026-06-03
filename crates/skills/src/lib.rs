//! On-disk skill catalog. A skill is a directory containing a `SKILL.md`:
//! YAML frontmatter (`name`, `description`) followed by a markdown body of
//! instructions. The catalog is loaded once at boot and held in memory —
//! the body and every bundled resource file are read up front, so the
//! runtime serves them without touching disk on the request path.
//!
//! Skills mirror the Claude Code / Codex experience: the name and
//! description are advertised to the model cheaply (one tool per skill),
//! and the full body is delivered only when the model calls that tool.
//! Bundled files are fetched on demand via [`SkillCatalog::read_file`],
//! sandboxed to the skill's own directory because only files discovered
//! under it at load time are ever held.

mod config;

use std::collections::BTreeMap;
use std::path::Path;

pub use config::SkillsConfig;
use coulisse_core::{SkillCatalog, SkillInfo, SkillReadError};
use serde::Deserialize;

/// Filename that marks a directory as a skill and carries its frontmatter.
const MANIFEST: &str = "SKILL.md";

/// One loaded skill: its advertised metadata, instruction body, and any
/// bundled resource files (keyed by path relative to the skill directory,
/// using `/` separators). The manifest itself is not stored among `files`
/// — its body lives in `body`.
#[derive(Clone, Debug)]
pub struct Skill {
    pub description: String,
    pub name: String,
    body: String,
    files: BTreeMap<String, String>,
}

/// In-memory catalog of every skill under the configured directory.
pub struct Skills {
    skills: BTreeMap<String, Skill>,
}

impl Skills {
    /// Load every skill under `config.dir`. A missing directory yields an
    /// empty catalog rather than an error, so the default `./skills`
    /// convention works whether or not the folder exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory exists but cannot be read, or if a
    /// `SKILL.md` manifest cannot be read or has malformed frontmatter.
    pub fn load(config: &SkillsConfig) -> Result<Self, SkillsError> {
        let mut skills = BTreeMap::new();
        if !config.dir.exists() {
            return Ok(Self { skills });
        }
        let entries = std::fs::read_dir(&config.dir).map_err(|source| SkillsError::Io {
            path: config.dir.display().to_string(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| SkillsError::Io {
                path: config.dir.display().to_string(),
                source,
            })?;
            let dir = entry.path();
            if !dir.is_dir() || !dir.join(MANIFEST).is_file() {
                continue;
            }
            let dir_name = dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("skill")
                .to_string();
            let skill = Skill::load(&dir, &dir_name)?;
            skills.insert(skill.name.clone(), skill);
        }
        Ok(Self { skills })
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.skills.len()
    }
}

impl SkillCatalog for Skills {
    fn body(&self, name: &str) -> Option<String> {
        self.skills.get(name).map(|s| s.body.clone())
    }

    fn list(&self) -> Vec<SkillInfo> {
        self.skills
            .values()
            .map(|s| SkillInfo {
                description: s.description.clone(),
                name: s.name.clone(),
            })
            .collect()
    }

    fn read_file(&self, skill: &str, path: &str) -> Result<String, SkillReadError> {
        let entry = self
            .skills
            .get(skill)
            .ok_or_else(|| SkillReadError::new(format!("no skill named '{skill}'")))?;
        let key = normalize(path);
        entry
            .files
            .get(&key)
            .cloned()
            .ok_or_else(|| SkillReadError::new(format!("skill '{skill}' has no file '{path}'")))
    }
}

impl Skill {
    fn load(dir: &Path, dir_name: &str) -> Result<Self, SkillsError> {
        let manifest = dir.join(MANIFEST);
        let raw = std::fs::read_to_string(&manifest).map_err(|source| SkillsError::Io {
            path: manifest.display().to_string(),
            source,
        })?;
        let (front, body) = parse_manifest(&raw, dir_name)?;
        let mut files = BTreeMap::new();
        collect_files(dir, dir, &mut files);
        files.remove(MANIFEST);
        Ok(Self {
            body,
            description: front.description,
            files,
            name: front.name,
        })
    }
}

/// Frontmatter fields. `name` defaults to the directory name and
/// `description` to empty, so a bare `SKILL.md` with no frontmatter still
/// loads.
#[derive(Deserialize)]
struct Frontmatter {
    #[serde(default)]
    description: String,
    name: Option<String>,
}

/// Split a `SKILL.md` into frontmatter and body. The frontmatter is an
/// optional leading `---` … `---` YAML block; without it the whole file is
/// the body and the skill is named after its directory.
fn parse_manifest(raw: &str, dir_name: &str) -> Result<(LoadedFront, String), SkillsError> {
    let trimmed = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    if let Some(rest) = trimmed.strip_prefix("---") {
        // The opening fence may be `---\n`; the body starts after the
        // closing `\n---` line.
        let rest = rest.trim_start_matches(['\r', '\n']);
        if let Some(end) = rest.find("\n---") {
            let yaml = &rest[..end];
            let after = &rest[end + "\n---".len()..];
            let body = after.trim_start_matches(['\r', '\n', '-']).to_string();
            let front: Frontmatter =
                serde_yaml::from_str(yaml).map_err(|source| SkillsError::Frontmatter { source })?;
            return Ok((
                LoadedFront::resolve(front, dir_name),
                body.trim().to_string(),
            ));
        }
    }
    Ok((
        LoadedFront {
            description: String::new(),
            name: dir_name.to_string(),
        },
        trimmed.trim().to_string(),
    ))
}

struct LoadedFront {
    description: String,
    name: String,
}

impl LoadedFront {
    fn resolve(front: Frontmatter, dir_name: &str) -> Self {
        Self {
            description: front.description,
            name: front
                .name
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| dir_name.to_string()),
        }
    }
}

/// Recursively read every UTF-8 text file under `dir` into `out`, keyed by
/// path relative to `root` with `/` separators. Non-UTF-8 files are
/// skipped — skills are instruction and resource text, and a binary blob
/// could never be served as a tool result anyway.
fn collect_files(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, out);
        } else if let (Ok(rel), Ok(contents)) =
            (path.strip_prefix(root), std::fs::read_to_string(&path))
        {
            out.insert(rel.to_string_lossy().replace('\\', "/"), contents);
        }
    }
}

/// Normalize a requested resource path to match the keys in `files`:
/// drop a leading `./` or `/` and unify separators.
fn normalize(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_string()
}

#[derive(Debug, thiserror::Error)]
pub enum SkillsError {
    #[error("failed to parse skill frontmatter: {source}")]
    Frontmatter { source: serde_yaml::Error },
    #[error("failed to read '{path}': {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_skill(root: &Path, name: &str, manifest: &str) {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(MANIFEST), manifest).unwrap();
    }

    #[test]
    fn loads_frontmatter_and_body() {
        let tmp = TempDir::new().unwrap();
        write_skill(
            tmp.path(),
            "resume-review",
            "---\nname: resume-review\ndescription: Review a resume\n---\nDo the thing.",
        );
        let skills = Skills::load(&SkillsConfig {
            dir: tmp.path().to_path_buf(),
        })
        .unwrap();
        assert_eq!(skills.len(), 1);
        let info = skills.list();
        assert_eq!(info[0].name, "resume-review");
        assert_eq!(info[0].description, "Review a resume");
        assert_eq!(
            skills.body("resume-review").as_deref(),
            Some("Do the thing.")
        );
    }

    #[test]
    fn missing_dir_is_empty_not_error() {
        let skills = Skills::load(&SkillsConfig {
            dir: Path::new("/nonexistent/skills/dir").to_path_buf(),
        })
        .unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn name_defaults_to_directory() {
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "salary", "No frontmatter here, just a body.");
        let skills = Skills::load(&SkillsConfig {
            dir: tmp.path().to_path_buf(),
        })
        .unwrap();
        assert_eq!(
            skills.body("salary").as_deref(),
            Some("No frontmatter here, just a body.")
        );
    }

    #[test]
    fn bundled_files_are_readable_and_sandboxed() {
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "coder", "---\nname: coder\n---\nbody");
        let dir = tmp.path().join("coder");
        fs::create_dir_all(dir.join("refs")).unwrap();
        fs::write(dir.join("refs").join("style.md"), "use tabs").unwrap();
        let skills = Skills::load(&SkillsConfig {
            dir: tmp.path().to_path_buf(),
        })
        .unwrap();
        assert_eq!(
            skills.read_file("coder", "refs/style.md").unwrap(),
            "use tabs"
        );
        assert_eq!(
            skills.read_file("coder", "./refs/style.md").unwrap(),
            "use tabs"
        );
        assert!(skills.read_file("coder", "../../etc/passwd").is_err());
        assert!(skills.read_file("coder", MANIFEST).is_err());
    }
}
