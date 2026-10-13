use indicatif::{ProgressBar, ProgressStyle};
use std::io::{self, IsTerminal, Write};
use termcolor::{Color, ColorChoice, ColorSpec, StandardStream, WriteColor};

pub mod init;
pub mod pull;
pub mod run;

/// Output handler for user-facing messages
/// Uses Write trait for flexibility and testability
pub struct Output {
    stdout: OutputStream,
    stderr: OutputStream,
    quiet: bool,
    json: bool,
}

impl Output {
    /// Create a new Output instance
    pub fn new(quiet: bool, json: bool) -> Self {
        let color_choice = if std::io::stdout().is_terminal() {
            ColorChoice::Auto
        } else {
            ColorChoice::Never
        };

        Self {
            stdout: OutputStream::Terminal(StandardStream::stdout(color_choice)),
            stderr: OutputStream::Terminal(StandardStream::stderr(color_choice)),
            quiet,
            json,
        }
    }

    #[cfg(target_os = "macos")]
    pub fn redirect_service_log(&mut self, log: crate::service_log::ServiceLog) {
        self.stdout = OutputStream::Service(log.clone());
        self.stderr = OutputStream::Service(log);
    }

    // ========== Basic Output Methods ==========

    /// Print an info message
    #[allow(dead_code)]
    pub fn info(&mut self, msg: impl AsRef<str>) -> io::Result<()> {
        if !self.quiet {
            if self.json {
                self.print_json("info", msg.as_ref())?;
            } else {
                writeln!(self.stdout, "{}", msg.as_ref())?;
            }
        }
        Ok(())
    }

    /// Print a success message (green)
    #[allow(dead_code)]
    pub fn success(&mut self, msg: impl AsRef<str>) -> io::Result<()> {
        if !self.quiet {
            if self.json {
                self.print_json("success", msg.as_ref())?;
            } else {
                self.stdout
                    .set_color(ColorSpec::new().set_fg(Some(Color::Green)))?;
                writeln!(self.stdout, "✓ {}", msg.as_ref())?;
                self.stdout.reset()?;
            }
        }
        Ok(())
    }

    /// Print an error message (red, always shown)
    pub fn error(&mut self, msg: impl AsRef<str>) -> io::Result<()> {
        if self.json {
            self.print_json("error", msg.as_ref())?;
        } else {
            self.stderr
                .set_color(ColorSpec::new().set_fg(Some(Color::Red)))?;
            writeln!(self.stderr, "✗ {}", msg.as_ref())?;
            self.stderr.reset()?;
        }
        Ok(())
    }

    /// Print a warning message (yellow)
    #[allow(dead_code)]
    pub fn warn(&mut self, msg: impl AsRef<str>) -> io::Result<()> {
        if !self.quiet {
            if self.json {
                self.print_json("warn", msg.as_ref())?;
            } else {
                self.stderr
                    .set_color(ColorSpec::new().set_fg(Some(Color::Yellow)))?;
                writeln!(self.stderr, "⚠ {}", msg.as_ref())?;
                self.stderr.reset()?;
            }
        }
        Ok(())
    }

    // ========== Progress Bar ==========

    /// Create a progress bar (returns None if quiet or json mode)
    #[allow(dead_code)]
    pub fn create_progress(&self, total: u64, message: &str) -> Option<ProgressBar> {
        if self.quiet || self.json {
            return None;
        }

        let pb = ProgressBar::new(total);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] {msg}")
                .unwrap(),
        );
        pb.set_message(message.to_string());
        Some(pb)
    }

    // ========== JSON Output ==========

    fn print_json(&mut self, level: &str, message: &str) -> io::Result<()> {
        let json = serde_json::json!({
            "level": level,
            "message": message,
            "timestamp": crate::utils::time::format_local_rfc3339(&chrono::Utc::now()),
        });
        writeln!(self.stdout, "{}", json)?;
        Ok(())
    }

    // ========== Internal helpers for submodules ==========

    pub(crate) fn stdout(&mut self) -> &mut impl WriteColor {
        &mut self.stdout
    }

    pub(crate) fn stderr(&mut self) -> &mut impl WriteColor {
        &mut self.stderr
    }

    pub(crate) fn quiet(&self) -> bool {
        self.quiet
    }

    pub(crate) fn json(&self) -> bool {
        self.json
    }

    pub(crate) fn print_json_internal(&mut self, level: &str, message: &str) -> io::Result<()> {
        self.print_json(level, message)
    }
}

// Normal CLI output remains a StandardStream; only native macOS service mode redirects it.
enum OutputStream {
    Terminal(StandardStream),
    #[cfg(target_os = "macos")]
    Service(crate::service_log::ServiceLog),
}

impl Write for OutputStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Terminal(stream) => stream.write(bytes),
            #[cfg(target_os = "macos")]
            Self::Service(log) => log.write(bytes),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Terminal(stream) => stream.flush(),
            #[cfg(target_os = "macos")]
            Self::Service(log) => log.flush(),
        }
    }
}

impl WriteColor for OutputStream {
    fn supports_color(&self) -> bool {
        match self {
            Self::Terminal(stream) => stream.supports_color(),
            #[cfg(target_os = "macos")]
            Self::Service(_) => false,
        }
    }
    fn set_color(&mut self, spec: &ColorSpec) -> io::Result<()> {
        match self {
            Self::Terminal(stream) => stream.set_color(spec),
            #[cfg(target_os = "macos")]
            Self::Service(_) => Ok(()),
        }
    }
    fn reset(&mut self) -> io::Result<()> {
        match self {
            Self::Terminal(stream) => stream.reset(),
            #[cfg(target_os = "macos")]
            Self::Service(_) => Ok(()),
        }
    }
}
