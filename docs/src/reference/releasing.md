# Releasing

Coulisse follows [Semantic Versioning](https://semver.org). Pre-1.0, minor
bumps may include breaking changes to the YAML schema, HTTP surface, or CLI;
patch bumps will not.

## Cutting a release

1. **Bump the version** in the workspace `Cargo.toml`:

   ```toml
   [workspace.package]
   version = "0.2.0"
   ```

   All workspace crates inherit this via `version.workspace = true`, so this is
   the only place to edit.

2. **Update `CHANGELOG.md`** — rename the `## [Unreleased]` section to
   `## [0.2.0] - YYYY-MM-DD` and start a fresh `## [Unreleased]` block above it.

3. **Commit, tag, push:**

   ```bash
   git commit -am "Release v0.2.0"
   git tag v0.2.0
   git push && git push --tags
   ```

The `v*.*.*` tag triggers two workflows:

- `release.yml` (cargo-dist) — builds binaries and installers for macOS
  (x86 + ARM), Linux GNU (x86 + ARM), and Windows MSVC, then publishes them as
  a GitHub Release with auto-generated notes.
- `docker.yml` — builds a multi-arch image and pushes to
  `ghcr.io/almaju/coulisse` tagged `latest`, `0.2`, and `0.2.0`.

## Hotfixes

For patch releases on the latest minor, branch from the previous tag, fix
forward, then tag `v0.2.1` from that branch. The same workflow handles it.
