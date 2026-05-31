use crate::error::{Result, WaylogError};
use crate::github::{GitHubPublishConfig, GitHubPublisher};
use crate::output::Output;
use crate::utils::path;
use dialoguer::{Input, Password};
use std::path::PathBuf;

pub async fn handle_publish(
    archive_dir: Option<PathBuf>,
    repo: Option<String>,
    repo_path: String,
    branch: Option<String>,
    token: Option<String>,
    github_token_env: String,
    message: Option<String>,
    output: &mut Output,
) -> Result<()> {
    let default_archive_dir = path::get_default_archive_dir()?;
    let archive_dir = match archive_dir {
        Some(path) => path,
        None => {
            let input: String = Input::new()
                .with_prompt("Archive directory")
                .default(default_archive_dir.display().to_string())
                .interact_text()
                .map_err(|error| WaylogError::Internal(error.to_string()))?;
            PathBuf::from(input)
        }
    };
    if !archive_dir.exists() {
        return Err(WaylogError::PathError(format!(
            "Archive directory does not exist: {}",
            archive_dir.display()
        )));
    }

    let repo = match repo {
        Some(repo) => repo,
        None => Input::new()
            .with_prompt("GitHub repository (OWNER/REPO)")
            .interact_text()
            .map_err(|error| WaylogError::Internal(error.to_string()))?,
    };

    let repo_path = if repo_path == "waylog" {
        Input::new()
            .with_prompt("Repository path")
            .default(repo_path)
            .interact_text()
            .map_err(|error| WaylogError::Internal(error.to_string()))?
    } else {
        repo_path
    };

    let branch = match branch {
        Some(branch) => Some(branch),
        None => {
            let input: String = Input::new()
                .with_prompt("Branch (leave empty for default branch)")
                .allow_empty(true)
                .interact_text()
                .map_err(|error| WaylogError::Internal(error.to_string()))?;
            if input.trim().is_empty() {
                None
            } else {
                Some(input)
            }
        }
    };

    let message = match message {
        Some(message) => Some(message),
        None => {
            let input: String = Input::new()
                .with_prompt("Commit message (leave empty for automatic message)")
                .allow_empty(true)
                .interact_text()
                .map_err(|error| WaylogError::Internal(error.to_string()))?;
            if input.trim().is_empty() {
                None
            } else {
                Some(input)
            }
        }
    };

    let token = resolve_github_token(token, &github_token_env)?;

    output.info(format!(
        "Publishing {} to GitHub repo {}",
        archive_dir.display(),
        repo
    ))?;

    let publisher = GitHubPublisher::new(GitHubPublishConfig {
        archive_dir,
        repo,
        repo_path,
        branch,
        token,
        message,
    })?;

    let result = publisher.publish().await?;
    output.info(format!(
        "GitHub publish complete: {} uploaded, {} unchanged",
        result.uploaded, result.skipped
    ))?;
    Ok(())
}

fn resolve_github_token(token: Option<String>, github_token_env: &str) -> Result<String> {
    if let Some(token) = token {
        return Ok(token);
    }

    match std::env::var(github_token_env) {
        Ok(token) => Ok(token),
        Err(_) => Password::new()
            .with_prompt(format!(
                "GitHub token ({} not set in env)",
                github_token_env
            ))
            .interact()
            .map_err(|error| WaylogError::Internal(error.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_github_token_prefers_direct_token() {
        let token = resolve_github_token(Some("ghp_direct".to_string()), "MISSING_ENV").unwrap();
        assert_eq!(token, "ghp_direct");
    }

    #[test]
    fn test_resolve_github_token_falls_back_to_env() {
        let key = "WAYLOG_TEST_GITHUB_TOKEN";
        unsafe {
            std::env::set_var(key, "ghp_from_env");
        }

        let token = resolve_github_token(None, key).unwrap();
        assert_eq!(token, "ghp_from_env");

        unsafe {
            std::env::remove_var(key);
        }
    }
}
