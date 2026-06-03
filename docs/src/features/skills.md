# Skills

A skill is a reusable bundle of instructions you can hand an agent on demand — the same idea as skills in Claude Code or Codex. You write a folder with a `SKILL.md` describing how to do something ("review a resume", "negotiate a salary", "triage a bug report"), and any agent that opts in can pull those instructions in exactly when they're relevant.

> Not to be confused with the `coulisse skill` CLI command, which installs Coulisse itself as a skill into your coding assistant. This page is about the `skills:` config section — a primitive alongside `mcp`, tools, and `subagents` that your own agents use.

The point is *progressive disclosure*. An agent's preamble is always in context and costs tokens on every turn. A skill is different: only its one-line description sits in the model's tool list, cheaply advertising that the skill exists. The full body is delivered only when the model decides to use it. You can ship a dozen detailed playbooks without bloating every request.

## Writing a skill

A skill is a directory containing a `SKILL.md`. The file is optional YAML frontmatter followed by a markdown body:

```
skills/
  resume-review/
    SKILL.md
    rubric.md
```

```markdown
---
name: resume-review
description: Review a candidate resume against a role and produce structured feedback.
---

Score the resume on clarity, relevance, and impact. For the scoring rubric and
weights, read the `rubric.md` file bundled with this skill.

Return: a one-line verdict, three strengths, three gaps, and a hire/no-hire lean.
```

Frontmatter fields:

- **name** — how the skill is addressed in YAML and exposed to the model as a tool. Optional; defaults to the directory name. Use a tool-safe name (letters, digits, `_`, `-`).
- **description** — the one-line summary the model sees in its tool list. This is what it uses to decide whether to reach for the skill, so write it for the caller, not for yourself.

No frontmatter is fine too — a bare `SKILL.md` becomes a skill named after its directory with an empty description.

## Bundled resource files

Anything else in the skill's directory is a bundled resource the skill body can point at — a rubric, a template, a checklist, a reference doc. The model fetches them on demand through a built-in `skill_file` tool (one extra level of progressive disclosure: the body loads on use, a referenced file loads only when the model follows the pointer).

Resource access is sandboxed: only files discovered under the skill's own directory at load time are reachable. A skill cannot read outside its folder.

## Enabling skills

By default Coulisse scans `./skills` — dropping a folder there is all it takes. Point elsewhere with the top-level block:

```yaml
skills:
  dir: ./playbooks
```

A missing directory is not an error; it simply yields no skills.

Agents opt in by name, the same way they opt into MCP tools and subagents:

```yaml
agents:
  - name: recruiter
    provider: anthropic
    model: claude-sonnet-4-6
    skills: [resume-review, salary-negotiation]
```

Names that don't match a loaded skill are ignored. An agent with no `skills:` array gets none.

## What the model sees

When an agent has at least one usable skill, its tool list gains:

- **one tool per listed skill** — named after the skill, described by its `description`. Calling it returns the skill's full `SKILL.md` body.
- **`skill_file`** — reads a bundled resource by `skill` name and `path` (relative to that skill's directory).

A typical flow: the model reads a skill's description, decides it's relevant, calls the skill tool to load the instructions, then follows any pointers to bundled files via `skill_file`.

## Skills vs. MCP tools

Skills carry *instructions*; MCP servers carry *capabilities and side effects*. A skill tells an agent how to do something; it does not run code, touch the network, or mutate state. If a skill's procedure needs to execute something — score a document with a script, hit an API, write a file — that step belongs in an MCP tool the skill's body tells the model to call. Keeping the boundary here is deliberate: skills stay pure, inspectable text, and anything with effects goes through MCP where it's configured and observed.
