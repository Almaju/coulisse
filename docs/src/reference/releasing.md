# Releasing

Coulisse follows [Semantic Versioning](https://semver.org). Pre-1.0, minor
bumps may include breaking changes to the YAML schema, HTTP surface, or CLI;
patch bumps will not.

## Cutting a release

Releases are cut automatically when `main` advances to a new version. You never
tag by hand — bumping the version *is* the release trigger.

1. **Bump the version** in the workspace `Cargo.toml`:

   ```toml
   [workspace.package]
   version = "0.3.0"
   ```

   All workspace crates inherit this via `version.workspace = true`, so this is
   the only place to edit.

2. **Update `CHANGELOG.md`** — rename the `## [Unreleased]` section to
   `## [0.3.0] - YYYY-MM-DD` and start a fresh `## [Unreleased]` block above it.
   cargo-dist reads this section for the GitHub Release notes.

3. **Merge to `main`** (directly or via PR). That's it.

When `main` advances, `auto-tag.yml` reads the workspace version and, if no
`v<version>` tag exists yet, creates and pushes it. Commits that don't change
the version (docs, refactors, dependency bumps) match an existing tag and cut
nothing — so a release happens exactly when, and only when, you bump the
version.

The pushed `v*.*.*` tag then triggers two workflows:

- `release.yml` (cargo-dist) — builds binaries and installers for macOS
  (x86 + ARM), Linux GNU (x86 + ARM), and Windows MSVC, then publishes them as
  a GitHub Release with auto-generated notes. This Release is what
  [`coulisse update`](./cli.md#coulisse-update) pulls.
- `docker.yml` — builds a multi-arch image and pushes to
  `ghcr.io/almaju/coulisse` tagged `latest`, `0.3`, and `0.3.0`.

### One-time setup: `RELEASE_PAT`

`auto-tag.yml` pushes the tag with a `RELEASE_PAT` repository secret instead of
the default `GITHUB_TOKEN`. This is required: tags pushed by `GITHUB_TOKEN` do
**not** trigger other workflows (GitHub's recursion guard), which would leave
`release.yml` and `docker.yml` asleep and produce a tag with no artifacts.

Create it once:

1. Generate a fine-grained personal access token scoped to this repository with
   **Contents: Read and write** permission
   (Settings → Developer settings → Personal access tokens → Fine-grained
   tokens).
2. Add it under the repo's **Settings → Secrets and variables → Actions** as a
   new secret named `RELEASE_PAT`.

Without this secret the tag push fails and no release is cut.

## Hotfixes

For a patch release on the latest minor, bump the version to e.g. `0.3.1` and
merge to `main` — `auto-tag.yml` handles it like any other bump. To patch an
older minor, branch from the previous tag, bump and fix forward there, then
tag `v0.3.1` manually from that branch (the automation only watches `main`).
