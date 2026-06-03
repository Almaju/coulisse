//! `coulisse skill` — install the Coulisse configuration skill for AI
//! coding assistants.

use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::path::PathBuf;

const SKILL_CLAUDE_CODE: &str = include_str!("../skills/coulisse.md");
const SKILL_CODEX: &str = include_str!("../skills/codex-instructions.md");

#[derive(Clone, Debug, clap::ValueEnum)]
pub enum Tool {
    /// Claude Code — installs `/coulisse` as a slash command.
    #[value(name = "claude-code", alias = "claude")]
    ClaudeCode,
    /// OpenAI Codex CLI — writes Coulisse instructions to `AGENTS.md`.
    #[value(name = "codex")]
    Codex,
}

impl fmt::Display for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tool::ClaudeCode => f.write_str("Claude Code"),
            Tool::Codex => f.write_str("Codex"),
        }
    }
}

pub struct Options {
    pub global: bool,
    pub tool: Option<Tool>,
}

#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("$HOME is not set; cannot resolve the global skill directory")]
    NoHomeDir,
    #[error("unknown tool '{0}'; supported: claude-code, codex")]
    UnknownTool(String),
    #[error("failed to create directory {path}: {source}")]
    CreateDir {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("failed to write skill file {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("failed to read selection: {0}")]
    Stdin(#[from] io::Error),
}

/// # Errors
///
/// Returns an error if the underlying operation fails.
pub fn run(opts: &Options) -> Result<(), SkillError> {
    let tool = match &opts.tool {
        Some(t) => t.clone(),
        None => prompt_tool()?,
    };
    install(&tool, opts.global)
}

fn prompt_tool() -> Result<Tool, SkillError> {
    let choices: &[(&str, Tool, &str)] = &[
        (
            "1",
            Tool::ClaudeCode,
            "Claude Code  — /coulisse slash command",
        ),
        ("2", Tool::Codex, "Codex        — AGENTS.md instructions"),
    ];

    println!("Which AI coding tool do you use?");
    for (n, _, label) in choices {
        println!("  {n}. {label}");
    }
    print!("Choice [1]: ");
    io::stdout().flush()?;

    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let input = line.trim();

    match if input.is_empty() { "1" } else { input } {
        "1" | "claude-code" | "claude" => Ok(Tool::ClaudeCode),
        "2" | "codex" => Ok(Tool::Codex),
        other => Err(SkillError::UnknownTool(other.to_string())),
    }
}

fn install(tool: &Tool, global: bool) -> Result<(), SkillError> {
    match tool {
        Tool::ClaudeCode => install_claude_code(global),
        Tool::Codex => install_codex(global),
    }
}

fn install_claude_code(global: bool) -> Result<(), SkillError> {
    let dir = if global {
        home()?.join(".claude").join("commands")
    } else {
        PathBuf::from(".claude").join("commands")
    };
    write_file(&dir.join("coulisse.md"), SKILL_CLAUDE_CODE)?;
    println!("invoke it in Claude Code with: /coulisse <your request>");
    Ok(())
}

fn install_codex(global: bool) -> Result<(), SkillError> {
    let path = if global {
        home()?.join(".codex").join("instructions.md")
    } else {
        PathBuf::from("AGENTS.md")
    };
    write_file(&path, SKILL_CODEX)?;
    Ok(())
}

fn write_file(path: &std::path::Path, content: &str) -> Result<(), SkillError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|source| SkillError::CreateDir {
            path: parent.display().to_string(),
            source,
        })?;
    }
    fs::write(path, content).map_err(|source| SkillError::Write {
        path: path.display().to_string(),
        source,
    })?;
    println!("installed: {}", path.display());
    Ok(())
}

fn home() -> Result<PathBuf, SkillError> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| SkillError::NoHomeDir)
}
