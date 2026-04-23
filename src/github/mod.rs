use crate::error::{Result, WaylogError};
use base64::Engine;
use reqwest::{Method, StatusCode, Url};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const API_VERSION: &str = "2022-11-28";
const USER_AGENT: &str = "waylog-cli";

#[derive(Debug, Clone)]
pub struct GitHubPublishConfig {
    pub archive_dir: PathBuf,
    pub repo: String,
    pub repo_path: String,
    pub branch: Option<String>,
    pub token: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishResult {
    pub uploaded: usize,
    pub skipped: usize,
}

pub struct GitHubPublisher {
    client: reqwest::Client,
    archive_dir: PathBuf,
    owner: String,
    repo: String,
    repo_path: String,
    branch: Option<String>,
    token: String,
    message: Option<String>,
}

impl GitHubPublisher {
    pub fn new(config: GitHubPublishConfig) -> Result<Self> {
        let (owner, repo) = parse_repo(&config.repo)?;
        Ok(Self {
            client: reqwest::Client::new(),
            archive_dir: config.archive_dir,
            owner,
            repo,
            repo_path: normalize_repo_path(&config.repo_path),
            branch: config.branch,
            token: config.token,
            message: config.message,
        })
    }

    pub async fn publish(&self) -> Result<PublishResult> {
        let branch = match &self.branch {
            Some(branch) => branch.clone(),
            None => self.default_branch().await?,
        };

        let files = collect_archive_files(&self.archive_dir)?;
        if files.is_empty() {
            return Ok(PublishResult {
                uploaded: 0,
                skipped: 0,
            });
        }

        let head_ref = self.get_reference(&branch).await?;
        let base_commit = self.get_commit(&head_ref.object.sha).await?;

        let mut entries = Vec::new();
        for relative_path in &files {
            let absolute_path = self.archive_dir.join(relative_path);
            let content = std::fs::read(&absolute_path)?;
            let blob_sha = self.create_blob(&content).await?;
            entries.push(CreateTreeEntry {
                path: build_remote_path(&self.repo_path, relative_path),
                mode: "100644".to_string(),
                entry_type: "blob".to_string(),
                sha: blob_sha,
            });
        }

        let tree = self.create_tree(&base_commit.tree.sha, &entries).await?;
        if tree.sha == base_commit.tree.sha {
            return Ok(PublishResult {
                uploaded: 0,
                skipped: files.len(),
            });
        }

        let commit_message = self.message.clone().unwrap_or_else(|| {
            format!(
                "Update WayLog archive at {}",
                crate::utils::time::format_local_rfc3339(&chrono::Utc::now())
            )
        });
        let commit_sha = self
            .create_commit(&commit_message, &tree.sha, &head_ref.object.sha)
            .await?;
        self.update_reference(&branch, &commit_sha).await?;

        Ok(PublishResult {
            uploaded: files.len(),
            skipped: 0,
        })
    }

    async fn default_branch(&self) -> Result<String> {
        let url = self.repo_url()?;
        let response = self
            .send(Method::GET, url, None::<&serde_json::Value>)
            .await?;
        let repo: RepoResponse = response.json().await?;
        Ok(repo.default_branch)
    }

    async fn get_reference(&self, branch: &str) -> Result<GitReference> {
        let mut url = self.repo_url()?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| WaylogError::Internal("failed to build GitHub URL".to_string()))?;
            segments.extend(["git", "ref", "heads"]);
            segments.push(branch);
        }

        let response = self
            .send(Method::GET, url, None::<&serde_json::Value>)
            .await?;
        Ok(response.json().await?)
    }

    async fn get_commit(&self, sha: &str) -> Result<GitCommit> {
        let mut url = self.repo_url()?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| WaylogError::Internal("failed to build GitHub URL".to_string()))?;
            segments.extend(["git", "commits", sha]);
        }

        let response = self
            .send(Method::GET, url, None::<&serde_json::Value>)
            .await?;
        Ok(response.json().await?)
    }

    async fn create_blob(&self, content: &[u8]) -> Result<String> {
        let mut url = self.repo_url()?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| WaylogError::Internal("failed to build GitHub URL".to_string()))?;
            segments.extend(["git", "blobs"]);
        }

        let payload = serde_json::json!({
            "content": base64::engine::general_purpose::STANDARD.encode(content),
            "encoding": "base64",
        });
        let response = self.send(Method::POST, url, Some(&payload)).await?;
        let blob: BlobResponse = response.json().await?;
        Ok(blob.sha)
    }

    async fn create_tree(
        &self,
        base_tree: &str,
        entries: &[CreateTreeEntry],
    ) -> Result<TreeResponse> {
        let mut url = self.repo_url()?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| WaylogError::Internal("failed to build GitHub URL".to_string()))?;
            segments.extend(["git", "trees"]);
        }

        let payload = serde_json::json!({
            "base_tree": base_tree,
            "tree": entries,
        });
        let response = self.send(Method::POST, url, Some(&payload)).await?;
        Ok(response.json().await?)
    }

    async fn create_commit(
        &self,
        message: &str,
        tree_sha: &str,
        parent_sha: &str,
    ) -> Result<String> {
        let mut url = self.repo_url()?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| WaylogError::Internal("failed to build GitHub URL".to_string()))?;
            segments.extend(["git", "commits"]);
        }

        let payload = serde_json::json!({
            "message": message,
            "tree": tree_sha,
            "parents": [parent_sha],
        });
        let response = self.send(Method::POST, url, Some(&payload)).await?;
        let commit: CommitResponse = response.json().await?;
        Ok(commit.sha)
    }

    async fn update_reference(&self, branch: &str, commit_sha: &str) -> Result<()> {
        let mut url = self.repo_url()?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| WaylogError::Internal("failed to build GitHub URL".to_string()))?;
            segments.extend(["git", "refs", "heads"]);
            segments.push(branch);
        }

        let payload = serde_json::json!({
            "sha": commit_sha,
            "force": false,
        });
        self.send(Method::PATCH, url, Some(&payload)).await?;
        Ok(())
    }

    fn repo_url(&self) -> Result<Url> {
        let mut url = Url::parse("https://api.github.com/")
            .map_err(|error| WaylogError::Internal(error.to_string()))?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| WaylogError::Internal("failed to build GitHub URL".to_string()))?;
            segments.extend(["repos", &self.owner, &self.repo]);
        }
        Ok(url)
    }

    async fn send<T: Serialize>(
        &self,
        method: Method,
        url: Url,
        payload: Option<&T>,
    ) -> Result<reqwest::Response> {
        let mut request = self
            .client
            .request(method, url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("X-GitHub-Api-Version", API_VERSION)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", USER_AGENT);

        if let Some(payload) = payload {
            request = request.json(payload);
        }

        let response = request.send().await?;
        if response.status().is_success() {
            return Ok(response);
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Err(match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => WaylogError::PathError(
                "GitHub token is invalid or does not have repository write access".to_string(),
            ),
            StatusCode::NOT_FOUND => WaylogError::PathError(
                "GitHub repository, branch, or path was not found".to_string(),
            ),
            _ => WaylogError::Internal(format!(
                "GitHub API request failed with {}: {}",
                status, body
            )),
        })
    }
}

fn parse_repo(repo: &str) -> Result<(String, String)> {
    let mut parts = repo.split('/');
    let owner = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| {
            WaylogError::PathError("Repository must be in OWNER/REPO format".to_string())
        })?;
    let name = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| {
            WaylogError::PathError("Repository must be in OWNER/REPO format".to_string())
        })?;
    if parts.next().is_some() {
        return Err(WaylogError::PathError(
            "Repository must be in OWNER/REPO format".to_string(),
        ));
    }
    Ok((owner.to_string(), name.to_string()))
}

fn normalize_repo_path(repo_path: &str) -> String {
    repo_path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

fn build_remote_path(repo_path: &str, relative_path: &Path) -> String {
    let relative = relative_path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/");
    if repo_path.is_empty() {
        relative
    } else if relative.is_empty() {
        repo_path.to_string()
    } else {
        format!("{}/{}", repo_path, relative)
    }
}

fn collect_archive_files(archive_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(archive_dir) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.is_file() {
            let relative = path
                .strip_prefix(archive_dir)
                .map_err(|error| WaylogError::Internal(error.to_string()))?;
            files.push(relative.to_path_buf());
        }
    }
    files.sort();
    Ok(files)
}

#[derive(Debug, Deserialize)]
struct RepoResponse {
    default_branch: String,
}

#[derive(Debug, Deserialize)]
struct GitReference {
    object: GitObject,
}

#[derive(Debug, Deserialize)]
struct GitObject {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct GitCommit {
    tree: GitObject,
}

#[derive(Debug, Deserialize)]
struct BlobResponse {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct TreeResponse {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct CommitResponse {
    sha: String,
}

#[derive(Debug, Serialize)]
struct CreateTreeEntry {
    path: String,
    mode: String,
    #[serde(rename = "type")]
    entry_type: String,
    sha: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_parse_repo_requires_owner_and_name() {
        assert_eq!(
            parse_repo("openai/waylog").unwrap(),
            ("openai".to_string(), "waylog".to_string())
        );
        assert!(parse_repo("openai").is_err());
        assert!(parse_repo("openai/waylog/extra").is_err());
    }

    #[test]
    fn test_build_remote_path_preserves_nested_relative_paths() {
        let remote = build_remote_path("waylog", Path::new("sessions/file.md"));
        assert_eq!(remote, "waylog/sessions/file.md");
    }

    #[test]
    fn test_collect_archive_files_returns_sorted_relative_paths() {
        let temp_dir = TempDir::new().unwrap();
        let sessions_dir = temp_dir.path().join("sessions");
        let indexes_dir = temp_dir.path().join("indexes");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::create_dir_all(&indexes_dir).unwrap();
        std::fs::write(sessions_dir.join("b.md"), "b").unwrap();
        std::fs::write(sessions_dir.join("a.md"), "a").unwrap();
        std::fs::write(indexes_dir.join("manifest.json"), "{}").unwrap();

        let files = collect_archive_files(temp_dir.path()).unwrap();
        assert_eq!(
            files,
            vec![
                PathBuf::from("indexes/manifest.json"),
                PathBuf::from("sessions/a.md"),
                PathBuf::from("sessions/b.md"),
            ]
        );
    }
}
