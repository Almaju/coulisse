//! `coulisse update` — fetch the latest release from GitHub and
//! replace the running binary in place.
//!
//! Releases are produced by cargo-dist (see
//! `.github/workflows/release.yml`); the `self_update` crate auto-
//! detects the host target triple and matches it against the asset
//! names cargo-dist publishes.

use self_update::cargo_crate_version;

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error(transparent)]
    SelfUpdate(#[from] self_update::errors::Error),
}

/// # Errors
///
/// Returns an error if the underlying operation fails.
pub fn run() -> Result<(), UpdateError> {
    let status = self_update::backends::github::Update::configure()
        .repo_owner("Almaju")
        .repo_name("coulisse")
        .bin_name("coulisse")
        .show_download_progress(true)
        .current_version(cargo_crate_version!())
        .build()?
        .update()?;

    if status.updated() {
        println!("updated to {}", status.version());
    } else {
        println!("already on the latest version ({})", status.version());
    }
    Ok(())
}
