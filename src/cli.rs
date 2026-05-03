use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(name = "waylog")]
#[command(about = "Automatically sync AI chat history from various CLI tools", long_about = None)]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Suppress all output (except errors)
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Output format
    #[arg(long, default_value = "text", global = true)]
    pub output: OutputFormat,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, ValueEnum, PartialEq, Eq)]
pub enum Browser {
    Chrome,
    Atlas,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Run an AI CLI tool and automatically sync its chat history
    Run {
        /// The AI tool to run (codex, claude, gemini)
        agent: Option<String>,

        /// Unified archive directory for real-time export
        #[arg(long)]
        archive_dir: Option<std::path::PathBuf>,

        /// Additional arguments to pass to the agent
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Pull chat history from providers
    Pull {
        /// Specific provider to pull (if not specified, pulls all)
        #[arg(short, long)]
        provider: Option<String>,

        /// Force re-pull even if up to date
        #[arg(short, long)]
        force: bool,
    },

    /// Export host-wide chat history to a unified archive directory
    Export {
        /// Specific provider to export (if not specified, exports all)
        #[arg(short, long)]
        provider: Option<String>,

        /// Specific browser history source to export
        #[arg(long, value_enum)]
        browser: Option<Browser>,

        /// Skip browser history export even when it is enabled by default
        #[arg(long, conflicts_with = "browser")]
        no_browser: bool,

        /// Output archive directory
        #[arg(long)]
        archive_dir: Option<std::path::PathBuf>,

        /// Force re-export even if unchanged
        #[arg(short, long)]
        force: bool,
    },

    /// Watch local provider session directories and keep the archive updated
    Watch {
        /// Specific provider to watch (if not specified, watches all supported providers)
        #[arg(short, long)]
        provider: Option<String>,

        /// Specific browser history source to watch
        #[arg(long, value_enum)]
        browser: Option<Browser>,

        /// Skip browser history watch even when it is enabled by default
        #[arg(long, conflicts_with = "browser")]
        no_browser: bool,

        /// Output archive directory
        #[arg(long)]
        archive_dir: Option<std::path::PathBuf>,
    },

    /// Publish an archive directory to GitHub using a token
    Publish {
        /// Archive directory to upload
        #[arg(long)]
        archive_dir: Option<std::path::PathBuf>,

        /// Target GitHub repository in OWNER/REPO format
        #[arg(long)]
        repo: Option<String>,

        /// Target path inside the repository
        #[arg(long, default_value = "waylog")]
        repo_path: String,

        /// Branch to update (defaults to the repository default branch)
        #[arg(long)]
        branch: Option<String>,

        /// Environment variable that holds the GitHub token
        #[arg(long, default_value = "GITHUB_TOKEN")]
        github_token_env: String,

        /// Commit message to use for the GitHub update
        #[arg(long)]
        message: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_parses_archive_dir_after_agent() {
        let cli = Cli::parse_from(["waylog", "run", "codex", "--archive-dir", "/tmp/archive"]);

        match cli.command {
            Commands::Run {
                agent,
                archive_dir,
                args,
            } => {
                assert_eq!(agent.as_deref(), Some("codex"));
                assert_eq!(
                    archive_dir.as_deref(),
                    Some(std::path::Path::new("/tmp/archive"))
                );
                assert!(args.is_empty());
            }
            _ => panic!("expected run command"),
        }
    }

    #[test]
    fn test_publish_parses_repo_and_token_env() {
        let cli = Cli::parse_from([
            "waylog",
            "publish",
            "--repo",
            "openai/knowledge",
            "--repo-path",
            "waylog",
            "--github-token-env",
            "MY_TOKEN",
        ]);

        match cli.command {
            Commands::Publish {
                archive_dir,
                repo,
                repo_path,
                branch,
                github_token_env,
                message,
            } => {
                assert!(archive_dir.is_none());
                assert_eq!(repo.as_deref(), Some("openai/knowledge"));
                assert_eq!(repo_path, "waylog");
                assert!(branch.is_none());
                assert_eq!(github_token_env, "MY_TOKEN");
                assert!(message.is_none());
            }
            _ => panic!("expected publish command"),
        }
    }

    #[test]
    fn test_watch_parses_provider_and_archive_dir() {
        let cli = Cli::parse_from([
            "waylog",
            "watch",
            "--provider",
            "claude",
            "--archive-dir",
            "/tmp/archive",
        ]);

        match cli.command {
            Commands::Watch {
                provider,
                archive_dir,
                browser,
                no_browser,
            } => {
                assert_eq!(provider.as_deref(), Some("claude"));
                assert_eq!(
                    archive_dir.as_deref(),
                    Some(std::path::Path::new("/tmp/archive"))
                );
                assert!(browser.is_none());
                assert!(!no_browser);
            }
            _ => panic!("expected watch command"),
        }
    }

    #[test]
    fn test_export_parses_browser_flag() {
        let cli = Cli::parse_from(["waylog", "export", "--browser", "chrome"]);

        match cli.command {
            Commands::Export {
                provider,
                archive_dir,
                force,
                browser,
                no_browser,
            } => {
                assert!(provider.is_none());
                assert!(archive_dir.is_none());
                assert!(!force);
                assert_eq!(browser, Some(Browser::Chrome));
                assert!(!no_browser);
            }
            _ => panic!("expected export command"),
        }
    }

    #[test]
    fn test_watch_parses_browser_and_provider_together() {
        let cli = Cli::parse_from([
            "waylog",
            "watch",
            "--provider",
            "codex",
            "--browser",
            "chrome",
        ]);

        match cli.command {
            Commands::Watch {
                provider,
                archive_dir,
                browser,
                no_browser,
            } => {
                assert_eq!(provider.as_deref(), Some("codex"));
                assert!(archive_dir.is_none());
                assert_eq!(browser, Some(Browser::Chrome));
                assert!(!no_browser);
            }
            _ => panic!("expected watch command"),
        }
    }

    #[test]
    fn test_export_parses_atlas_browser_flag() {
        let cli = Cli::parse_from(["waylog", "export", "--browser", "atlas"]);

        match cli.command {
            Commands::Export {
                browser,
                no_browser,
                ..
            } => {
                assert_eq!(browser, Some(Browser::Atlas));
                assert!(!no_browser);
            }
            _ => panic!("expected export command"),
        }
    }

    #[test]
    fn test_export_parses_no_browser_flag() {
        let cli = Cli::parse_from(["waylog", "export", "--no-browser"]);

        match cli.command {
            Commands::Export {
                browser,
                no_browser,
                ..
            } => {
                assert!(browser.is_none());
                assert!(no_browser);
            }
            _ => panic!("expected export command"),
        }
    }

    #[test]
    fn test_watch_parses_no_browser_flag() {
        let cli = Cli::parse_from(["waylog", "watch", "--no-browser"]);

        match cli.command {
            Commands::Watch {
                browser,
                no_browser,
                ..
            } => {
                assert!(browser.is_none());
                assert!(no_browser);
            }
            _ => panic!("expected watch command"),
        }
    }
}
