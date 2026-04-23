use crate::archive::ArchiveWriter;
use crate::error::{Result, WaylogError};
use crate::output::Output;
use crate::providers;
use crate::utils::path;

pub async fn handle_export(
    provider_name: Option<String>,
    archive_dir: Option<std::path::PathBuf>,
    force: bool,
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
    let writer = ArchiveWriter::new(archive_dir.clone());
    output.info(format!(
        "Exporting chat history to {}",
        archive_dir.display()
    ))?;

    let providers_to_export = if let Some(name) = provider_name {
        vec![providers::get_provider(&name)?]
    } else {
        providers::all_providers()
    };

    let mut written = 0usize;
    let mut skipped = 0usize;

    for provider in providers_to_export {
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
                    skipped += 1;
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
                    skipped += 1;
                    continue;
                }
            };

            if result.written || force {
                written += 1;
            } else {
                skipped += 1;
            }
        }
    }

    output.info(format!(
        "Archive export complete: {} written, {} unchanged",
        written, skipped
    ))?;
    Ok(())
}
