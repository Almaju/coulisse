//! Startup banner. Prints the proxy/admin URLs and a per-feature
//! summary of the live configuration. Uses OSC 8 hyperlinks for
//! clickable URLs in supported terminals (iTerm2, modern Terminal.app,
//! VS Code, WezTerm) and falls back to plain text when stdout isn't a
//! TTY.

use std::io::IsTerminal;
use std::net::SocketAddr;

use agents::AgentConfig;
use auth::Auth;
use experiments::{ExperimentConfig, Strategy};
use judges::JudgeConfig;
use memory::ExtractorConfig;

pub struct Banner<'a> {
    pub addr: SocketAddr,
    pub agents: &'a [AgentConfig],
    pub auth: &'a Auth,
    pub experiments: &'a [ExperimentConfig],
    pub extractor: Option<&'a ExtractorConfig>,
    pub judges: &'a [JudgeConfig],
    pub memory_summary: &'a str,
}

impl Banner<'_> {
    pub fn print(&self) {
        let s = Style::detect();
        let url = self.display_url();

        println!();
        println!(
            "  {bold}coulisse{reset} {dim}{version}{reset}",
            bold = s.bold,
            reset = s.reset,
            dim = s.dim,
            version = env!("CARGO_PKG_VERSION"),
        );
        println!();
        println!(
            "  {bold}Proxy{reset}   →  {link}",
            bold = s.bold,
            reset = s.reset,
            link = s.link(&format!("{url}/v1")),
        );
        println!(
            "  {bold}Admin{reset}   →  {link}",
            bold = s.bold,
            reset = s.reset,
            link = s.link(&format!("{url}/admin")),
        );
        println!();

        let label = |name: &str| format!("{}{:<11}{}", s.bold, name, s.reset);
        println!("  {}{}", label("Memory"), self.memory_summary);
        println!(
            "  {}proxy: {} {dim}·{reset} admin: {}",
            label("Auth"),
            self.auth.proxy_summary(),
            self.auth.admin_summary(),
            dim = s.dim,
            reset = s.reset,
        );
        match self.extractor {
            Some(cfg) => println!(
                "  {}{} / {} {dim}(dedup={}, max={}){reset}",
                label("Extractor"),
                cfg.provider,
                cfg.model,
                cfg.dedup_threshold,
                cfg.max_facts_per_turn,
                dim = s.dim,
                reset = s.reset,
            ),
            None => println!(
                "  {}{dim}disabled (memory grows only via explicit API calls){reset}",
                label("Extractor"),
                dim = s.dim,
                reset = s.reset,
            ),
        }
        println!();

        self.print_agents(&s);
        self.print_judges(&s);
        self.print_experiments(&s);
    }

    fn display_url(&self) -> String {
        let host = if self.addr.ip().is_unspecified() {
            "localhost".to_string()
        } else {
            self.addr.ip().to_string()
        };
        format!("http://{host}:{}", self.addr.port())
    }

    fn print_agents(&self, s: &Style) {
        section_header(s, "Agents", self.agents.len());
        if self.agents.is_empty() {
            println!("    {}none configured{}", s.dim, s.reset);
            println!();
            return;
        }
        let w = self.agents.iter().map(|a| a.name.len()).max().unwrap_or(0);
        for agent in self.agents {
            let judges = if agent.judges.is_empty() {
                String::new()
            } else {
                format!("  {}judges: {}{}", s.dim, agent.judges.join(", "), s.reset)
            };
            println!(
                "    {bold}{name:<w$}{reset}  {dim}{provider} / {model}{reset}{judges}",
                bold = s.bold,
                reset = s.reset,
                name = agent.name,
                dim = s.dim,
                provider = agent.provider.as_str(),
                model = agent.model,
                judges = judges,
            );
        }
        println!();
    }

    fn print_judges(&self, s: &Style) {
        section_header(s, "Judges", self.judges.len());
        if self.judges.is_empty() {
            println!("    {}none configured{}", s.dim, s.reset);
            println!();
            return;
        }
        let w = self.judges.iter().map(|j| j.name.len()).max().unwrap_or(0);
        for judge in self.judges {
            let criteria: Vec<&str> = judge.rubrics.keys().map(String::as_str).collect();
            println!(
                "    {bold}{name:<w$}{reset}  {dim}{provider} / {model}  sampling={rate}{reset}  {criteria}",
                bold = s.bold,
                reset = s.reset,
                name = judge.name,
                dim = s.dim,
                provider = judge.provider,
                model = judge.model,
                rate = judge.sampling_rate,
                criteria = criteria.join(", "),
            );
        }
        println!();
    }

    fn print_experiments(&self, s: &Style) {
        section_header(s, "Experiments", self.experiments.len());
        if self.experiments.is_empty() {
            println!("    {}none configured{}", s.dim, s.reset);
            println!();
            return;
        }
        let w = self
            .experiments
            .iter()
            .map(|e| e.name.len())
            .max()
            .unwrap_or(0);
        for exp in self.experiments {
            let variants: Vec<String> = exp
                .variants
                .iter()
                .map(|v| format!("{}@{}", v.agent, v.weight))
                .collect();
            let strategy = match exp.strategy {
                Strategy::Bandit => "bandit",
                Strategy::Shadow => "shadow",
                Strategy::Split => "split",
            };
            println!(
                "    {bold}{name:<w$}{reset}  {dim}{strategy}, sticky={sticky}{reset}  [{variants}]",
                bold = s.bold,
                reset = s.reset,
                name = exp.name,
                dim = s.dim,
                strategy = strategy,
                sticky = exp.sticky_by_user,
                variants = variants.join(", "),
            );
        }
        println!();
    }
}

fn section_header(s: &Style, name: &str, count: usize) {
    println!(
        "  {bold}{name}{reset} {dim}({count}){reset}",
        bold = s.bold,
        reset = s.reset,
        dim = s.dim,
    );
}

struct Style {
    bold: &'static str,
    cyan: &'static str,
    dim: &'static str,
    hyperlinks: bool,
    reset: &'static str,
}

impl Style {
    fn detect() -> Self {
        if std::io::stdout().is_terminal() {
            Self {
                bold: "\x1b[1m",
                cyan: "\x1b[36m",
                dim: "\x1b[2m",
                hyperlinks: true,
                reset: "\x1b[0m",
            }
        } else {
            Self {
                bold: "",
                cyan: "",
                dim: "",
                hyperlinks: false,
                reset: "",
            }
        }
    }

    fn link(&self, url: &str) -> String {
        if self.hyperlinks {
            format!(
                "\x1b]8;;{url}\x1b\\{cyan}{url}{reset}\x1b]8;;\x1b\\",
                cyan = self.cyan,
                reset = self.reset,
            )
        } else {
            url.to_string()
        }
    }
}
