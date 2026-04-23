use crate::archive::ArchiveWriter;
use crate::error::{Result, WaylogError};
use crate::output::Output;
use crate::providers;
use crate::utils::path;
use std::time::Duration;
use tokio::time;

const WATCH_INTERVAL_SECS: u64 = 30;

pub async fn handle_watch(
    provider_name: Option<String>,
    archive_dir: Option<std::path::PathBuf>,
    output: &mut Output,
) -> Result<()> {
    if let Some(ref name) = provider_name {
        match providers::get_provider(name) {
            Ok(_) => {}
            Err(WaylogError::ProviderNotFound(invalid_name)) => {
                output.unknown_provider(&invalid_name)?;
                return Err(WaylogError::ProviderNotFound(name.clone()));
            }
            Err(error) => return Err(error),
        }
    }

    let archive_dir = archive_dir.unwrap_or(path::get_default_archive_dir()?);
    path::ensure_dir_exists(&archive_dir)?;
    let writer = ArchiveWriter::new(archive_dir.clone());

    output.info(format!(
        "Watching local session data and syncing to {} every {} seconds",
        archive_dir.display(),
        WATCH_INTERVAL_SECS
    ))?;
    output.info("Press Ctrl+C to stop watching.")?;

    let watched_providers = if let Some(name) = provider_name {
        vec![providers::get_provider(&name)?]
    } else {
        providers::all_providers()
    };

    let mut interval = time::interval(Duration::from_secs(WATCH_INTERVAL_SECS));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let mut written = 0usize;
                let mut unchanged = 0usize;

                for provider in &watched_providers {
                    if !provider.has_local_data() {
                        continue;
                    }

                    let session_paths = provider.get_all_host_sessions().await?;
                    for session_path in session_paths {
                        let session = match provider.parse_session(&session_path).await {
                            Ok(session) => session,
                            Err(error) => {
                                tracing::warn!(
                                    "Skipping unreadable {} session {}: {}",
                                    provider.name(),
                                    session_path.display(),
                                    error
                                );
                                continue;
                            }
                        };
                        if session.messages.is_empty() {
                            continue;
                        }

                        let result = match writer
                            .export_session(&session, &session_path, provider.raw_extension())
                            .await
                        {
                            Ok(result) => result,
                            Err(error) => {
                                tracing::warn!(
                                    "Skipping failed archive export for {} session {}: {}",
                                    provider.name(),
                                    session_path.display(),
                                    error
                                );
                                continue;
                            }
                        };
                        if result.written {
                            written += 1;
                        } else {
                            unchanged += 1;
                        }
                    }
                }

                if written > 0 {
                    output.info(format!(
                        "Watch sync complete: {} updated, {} unchanged",
                        written, unchanged
                    ))?;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                output.info("Stopped watching local session data.")?;
                break;
            }
        }
    }

    Ok(())
}
