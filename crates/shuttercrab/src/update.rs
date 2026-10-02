//! Shuttercrab updates itself from the repository's GitHub Releases,
//! through [Velopack](https://velopack.io).
//!
//! An installed copy checks at launch and every few hours, downloads a
//! newer release in the background, and offers "Restart to update" in the
//! tray menu. It never restarts on its own: the offer is hidden while a
//! recording runs, and an update that is downloaded but not restarted into
//! is applied at the next launch. A copy that was not installed by
//! Velopack, such as `cargo run`, never checks.

use futures::{FutureExt as _, future::BoxFuture};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use velopack::{UpdateCheck, UpdateInfo, UpdateManager, sources::AutoSource};

/// Where releases are published.
pub const REPO_URL: &str = "https://github.com/jakeklassen/shuttercrab";

/// Points an installed copy at another feed, such as a local folder of
/// packages, to try a release before publishing it.
const SOURCE_VARIABLE: &str = "SHUTTERCRAB_UPDATE_SOURCE";

/// How often a running copy looks for a new release. GitHub allows 60
/// unauthenticated API requests an hour per IP, so this stays well clear.
pub const CHECK_EVERY: Duration = Duration::from_secs(4 * 60 * 60);

/// Finds, downloads and applies updates. Tests pass a fake.
pub trait UpdateBackend: Send + Sync + 'static {
    /// Look for a newer release and download it. Resolves to its version
    /// once it is ready to apply, or `None` when there is nothing newer.
    fn fetch(&self) -> BoxFuture<'static, anyhow::Result<Option<String>>>;

    /// Start the updater, which waits for Shuttercrab to quit, applies the
    /// downloaded release and starts it. The caller then quits as usual.
    fn apply_after_exit(&self) -> anyhow::Result<()>;
}

/// Velopack's update manager reading GitHub Releases.
pub struct Velopack {
    manager: UpdateManager,
    downloaded: Arc<Mutex<Option<UpdateInfo>>>,
}

impl Velopack {
    /// `None` when this copy was not installed by Velopack.
    pub fn new() -> Option<Self> {
        let source = std::env::var(SOURCE_VARIABLE).unwrap_or_else(|_| REPO_URL.into());
        let manager = UpdateManager::new(AutoSource::new(&source), None, None).ok()?;
        Some(Self {
            manager,
            downloaded: Arc::default(),
        })
    }

    /// This copy's version, as Velopack installed it.
    pub fn version(&self) -> String {
        self.manager.get_current_version_as_string()
    }
}

impl UpdateBackend for Velopack {
    // Velopack's calls block on the network, so the future does too. The
    // caller polls it on GPUI's background executor.
    fn fetch(&self) -> BoxFuture<'static, anyhow::Result<Option<String>>> {
        let manager = self.manager.clone();
        let downloaded = self.downloaded.clone();
        async move {
            let UpdateCheck::UpdateAvailable(update) = manager.check_for_updates()? else {
                return Ok(None);
            };
            manager.download_updates(&update, None)?;
            let version = update.TargetFullRelease.Version.clone();
            *downloaded.lock().unwrap() = Some(*update);
            Ok(Some(version))
        }
        .boxed()
    }

    fn apply_after_exit(&self) -> anyhow::Result<()> {
        let update = self.downloaded.lock().unwrap().clone();
        let update = update.ok_or_else(|| anyhow::anyhow!("no update has been downloaded"))?;
        let no_args: [&str; 0] = [];
        self.manager
            .wait_exit_then_apply_updates(&update, true, true, no_args)?;
        Ok(())
    }
}
