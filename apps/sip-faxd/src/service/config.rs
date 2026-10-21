//! `[log]` (file and rotation) and `[service]` (PID file, control socket, stop timeout).

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// `[log]`
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LogSection {
    /// Default log filter (overridden by `RUST_LOG`).
    pub level: String,
    /// Log file, relative to the executable directory. Empty: no file, stderr only.
    pub file: String,
    /// Rotate when the file would grow beyond this many MiB (fractions are fine).
    pub max_size_mb: f64,
    /// Also rotate when the local date changes.
    pub daily: bool,
    /// Rotated files to keep (the current file is not counted).
    pub keep: u32,
    /// Compress rotated files with gzip.
    pub compress: bool,
}

impl Default for LogSection {
    fn default() -> Self {
        LogSection {
            level: String::new(),
            file: "logs/sip-faxd.log".into(),
            max_size_mb: 50.0,
            daily: true,
            keep: 14,
            compress: true,
        }
    }
}

impl LogSection {
    /// The log file against the executable directory (`None` when file logging is off).
    pub fn file_path(&self, base: &Path) -> Option<PathBuf> {
        (!self.file.trim().is_empty()).then(|| resolve(base, Path::new(&self.file)))
    }

    /// Checks the values.
    pub fn validate(&self) -> Result<(), String> {
        if !(self.max_size_mb.is_finite() && self.max_size_mb > 0.0) {
            return Err("[log] max_size_mb must be greater than 0".into());
        }
        if self.keep == 0 {
            return Err("[log] keep must be at least 1".into());
        }
        Ok(())
    }
}

/// `[service]`
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ServiceSection {
    /// PID file (held with `flock` while the service runs).
    pub pid_file: String,
    /// Control socket (unix domain socket, mode 0600).
    pub socket: String,
    /// `stop`, SIGTERM: how long the calls in progress are waited for. 0 ends them at once.
    pub stop_timeout_secs: u64,
    /// `start`: how long to wait until the service answers on the control socket.
    pub start_timeout_secs: u64,
}

impl Default for ServiceSection {
    fn default() -> Self {
        ServiceSection {
            pid_file: "run/sip-faxd.pid".into(),
            socket: "run/sip-faxd.sock".into(),
            stop_timeout_secs: 300,
            start_timeout_secs: 30,
        }
    }
}

impl ServiceSection {
    /// Checks the values.
    pub fn validate(&self) -> Result<(), String> {
        if self.pid_file.trim().is_empty() || self.socket.trim().is_empty() {
            return Err("[service] pid_file and socket must not be empty".into());
        }
        if self.start_timeout_secs == 0 {
            return Err("[service] start_timeout_secs must be at least 1".into());
        }
        Ok(())
    }

    /// The PID file against the executable directory.
    pub fn pid_path(&self, base: &Path) -> PathBuf {
        resolve(base, Path::new(&self.pid_file))
    }

    /// The control socket against the executable directory.
    pub fn socket_path(&self, base: &Path) -> PathBuf {
        resolve(base, Path::new(&self.socket))
    }
}

fn resolve(base: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

/// Just the sections the service commands (`start`, `stop`, `status`, `logs`) need. The rest of
/// the file is not looked at, so a half-edited route does not stop `sip-faxd stop`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServiceConfig {
    /// `[log]`
    pub log: LogSection,
    /// `[service]`
    pub service: ServiceSection,
}

impl ServiceConfig {
    /// Parses configuration text; `path` only appears in error messages.
    pub fn parse(text: &str, path: &Path) -> Result<ServiceConfig, String> {
        let mut table: toml::Table = text.parse().map_err(|e: toml::de::Error| {
            format!("{}: invalid TOML: {}", path.display(), e.message())
        })?;
        let log = take(&mut table, "log", path)?;
        let service = take(&mut table, "service", path)?;
        let c = ServiceConfig { log, service };
        c.log
            .validate()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        c.service
            .validate()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(c)
    }

    /// Reads and parses a file.
    pub fn load(path: &Path) -> Result<ServiceConfig, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read configuration {}: {e}", path.display()))?;
        Self::parse(&text, path)
    }
}

/// Removes section `name` from `table` and parses it (defaults when absent).
pub fn take<T: for<'de> Deserialize<'de> + Default>(
    table: &mut toml::Table,
    name: &str,
    path: &Path,
) -> Result<T, String> {
    match table.remove(name) {
        None => Ok(T::default()),
        Some(v) => v
            .try_into()
            .map_err(|e: toml::de::Error| format!("{}: [{name}]: {}", path.display(), e.message())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(t: &str) -> Result<ServiceConfig, String> {
        ServiceConfig::parse(t, Path::new("c.toml"))
    }

    #[test]
    fn m00_defaults_follow_the_spec() {
        let c = parse("").unwrap();
        assert_eq!(c.log.file, "logs/sip-faxd.log");
        assert_eq!((c.log.max_size_mb, c.log.keep), (50.0, 14));
        assert!(c.log.daily && c.log.compress);
        assert_eq!(c.service.pid_file, "run/sip-faxd.pid");
        assert_eq!(c.service.socket, "run/sip-faxd.sock");
        assert_eq!(c.service.stop_timeout_secs, 300);
        assert_eq!(c.service.start_timeout_secs, 30);
    }

    #[test]
    fn m00_values_are_read_and_paths_are_relative_to_the_executable() {
        let c = parse(
            "[log]\nlevel=\"debug\"\nfile=\"l/x.log\"\nmax_size_mb=0.5\ndaily=false\nkeep=3\ncompress=false\n\
             [service]\npid_file=\"/var/run/f.pid\"\nsocket=\"s.sock\"\nstop_timeout_secs=5\n\
             [sip]\nbogus = 1\n",
        )
        .unwrap();
        assert_eq!(c.log.level, "debug");
        assert_eq!(
            c.log.file_path(Path::new("/opt/fax")),
            Some(PathBuf::from("/opt/fax/l/x.log"))
        );
        assert!(!c.log.daily && !c.log.compress);
        assert_eq!(c.log.max_size_mb, 0.5);
        assert_eq!(
            c.service.pid_path(Path::new("/opt/fax")),
            PathBuf::from("/var/run/f.pid")
        );
        assert_eq!(
            c.service.socket_path(Path::new("/opt/fax")),
            PathBuf::from("/opt/fax/s.sock")
        );
        assert_eq!(c.service.stop_timeout_secs, 5);
        // an empty file name switches file logging off
        let off = parse("[log]\nfile = \"\"\n").unwrap();
        assert_eq!(off.log.file_path(Path::new("/opt")), None);
    }

    #[test]
    fn m00_bad_values_and_unknown_keys_are_refused_with_the_section_name() {
        for (text, needle) in [
            ("[log]\nbogus = 1\n", "[log]"),
            ("[service]\nbogus = 1\n", "[service]"),
            ("[log]\nmax_size_mb = 0\n", "max_size_mb"),
            ("[log]\nkeep = 0\n", "keep"),
            ("[service]\nsocket = \"\"\n", "socket"),
            ("[service]\nstart_timeout_secs = 0\n", "start_timeout_secs"),
            ("[log\n", "invalid TOML"),
        ] {
            let e = parse(text).unwrap_err();
            assert!(e.contains(needle) && e.contains("c.toml"), "{text}: {e}");
        }
    }
}
