//! Bounded in-process logs for a launchd-managed macOS watch.
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing_subscriber::{fmt, layer::SubscriberExt, EnvFilter};

#[derive(Clone)]
pub struct ServiceLog(Arc<Mutex<LogFile>>);

struct LogFile {
    path: PathBuf,
    file: File,
    bytes: u64,
    max_bytes: u64,
    backups: usize,
    line_start: bool,
}

fn backup_path(path: &Path, number: usize) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{number}"));
    PathBuf::from(name)
}

impl ServiceLog {
    pub fn open(path: &Path, max_bytes: u64, backups: usize) -> io::Result<Self> {
        if max_bytes < 64 || backups == 0 {
            return Err(io::Error::other("Invalid service log limits"));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let bytes = file.metadata()?.len();
        Ok(Self(Arc::new(Mutex::new(LogFile {
            path: path.to_owned(),
            file,
            bytes,
            max_bytes,
            backups,
            line_start: true,
        }))))
    }

    pub fn setup_tracing(&self, verbose: bool) -> crate::error::Result<()> {
        let level = if verbose { "debug" } else { "warn" };
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
        let subscriber = tracing_subscriber::registry().with(filter).with(
            fmt::layer()
                .with_writer(self.clone())
                .with_ansi(false)
                .with_target(false)
                .without_time(),
        );
        tracing::subscriber::set_global_default(subscriber)
            .map_err(|error| io::Error::other(error.to_string()))?;
        Ok(())
    }
}

impl LogFile {
    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        let oldest = backup_path(&self.path, self.backups);
        if oldest.exists() {
            std::fs::remove_file(oldest)?;
        }
        for n in (1..self.backups).rev() {
            let old = backup_path(&self.path, n);
            if old.exists() {
                std::fs::rename(old, backup_path(&self.path, n + 1))?;
            }
        }
        std::fs::rename(&self.path, backup_path(&self.path, 1))?;
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.bytes = 0;
        Ok(())
    }

    fn append(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            if self.bytes >= self.max_bytes {
                self.rotate()?;
            }
            let count = bytes.len().min((self.max_bytes - self.bytes) as usize);
            self.file.write_all(&bytes[..count])?;
            self.bytes += count as u64;
            bytes = &bytes[count..];
        }
        Ok(())
    }
}

impl Write for ServiceLog {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut log = self
            .0
            .lock()
            .map_err(|_| io::Error::other("Service log poisoned"))?;
        for part in bytes.split_inclusive(|byte| *byte == b'\n') {
            if log.line_start && !part.is_empty() {
                let stamp = chrono::Local::now()
                    .format("%Y-%m-%d %H:%M:%S%.3f ")
                    .to_string();
                // Keep an ordinary short record together when the old file is nearly full.
                if log.bytes > 0
                    && stamp.len() + part.len() <= log.max_bytes as usize
                    && log.bytes + (stamp.len() + part.len()) as u64 > log.max_bytes
                {
                    log.rotate()?;
                }
                log.append(stamp.as_bytes())?;
            }
            log.append(part)?;
            log.line_start = part.last() == Some(&b'\n');
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("Service log poisoned"))?
            .file
            .flush()
    }
}

impl<'a> fmt::MakeWriter<'a> for ServiceLog {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_bounds_disk_use_and_keeps_latest_messages() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("watch-output.log");
        let mut log = ServiceLog::open(&path, 96, 3).unwrap();
        for n in 0..30 {
            writeln!(log, "event {n:02} collected").unwrap();
        }
        log.flush().unwrap();
        let entries: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().collect();
        assert!(entries.len() <= 4);
        for entry in entries {
            assert!(entry.unwrap().metadata().unwrap().len() <= 96);
        }
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("event 29 collected"));
    }

    #[test]
    fn cloned_writers_share_one_log_and_handle_oversize_records() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("watch-output.log");
        let mut first = ServiceLog::open(&path, 128, 3).unwrap();
        let mut second = first.clone();
        first.write_all(&vec![b'x'; 1000]).unwrap();
        second.write_all(b"\nlast-record\n").unwrap();
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("last-record"));
        for entry in std::fs::read_dir(tmp.path()).unwrap() {
            assert!(entry.unwrap().metadata().unwrap().len() <= 128);
        }
    }
}
