use crate::error::Result;
use crate::{providers, session, synchronizer::Synchronizer};
use std::sync::Arc;
use tokio::process::Child;
use tokio::task::JoinHandle;
use tracing;

/// Perform cleanup and final sync
///
/// This function handles:
/// - Stopping the file watcher
/// - Performing final sync of chat messages
/// - Saving session state
///
/// Errors during cleanup are logged but don't prevent the function from completing.
pub(crate) async fn cleanup_and_sync(
    watcher_handle: &JoinHandle<()>,
    _child: &mut Child,
    tracker: &Arc<session::SessionTracker>,
    provider: &Arc<dyn providers::base::Provider>,
    project_path: &std::path::Path,
    archive_dir: Option<&std::path::Path>,
    _exit_status: Option<std::process::ExitStatus>,
) -> Result<()> {
    // Stop the file watcher
    watcher_handle.abort();
    // Wait a bit for the watcher to stop (non-blocking, ignore result)
    // Note: JoinHandle is not Copy, so we can't await the reference directly
    // Just abort is sufficient, the task will be cleaned up

    // Do a final sync
    tracing::info!("Session ended, performing final sync...");

    let synchronizer = Synchronizer::new(
        provider.clone(),
        project_path.to_path_buf(),
        tracker.clone(),
        archive_dir.map(|path| path.to_path_buf()),
    );
    if let Err(error) = synchronizer.sync_all(false).await {
        tracing::error!("Failed final sync: {}", error);
    }

    // Save final state - errors are logged but don't stop cleanup
    if let Err(e) = tracker.save_state().await {
        tracing::warn!("Failed to save state: {}", e);
    }

    Ok(())
}
