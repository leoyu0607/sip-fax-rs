//! The configuration file's side of the runtime configuration: which top-level tables are
//! runtime sections, and how they are parsed. `sip-faxd`'s `AppConfig` removes its own tables
//! (`[log]`, `[database]`) and hands the rest to [`RuntimeFile::from_table`]; `config import` and
//! the first-start import use the same code, so an exported file always imports.

use fax_in::FaxInConfig;
use serde::Deserialize;

use crate::model::{DeliverySection, RetentionSection, RuntimeConfig};

/// Top-level tables of the configuration file that are runtime sections (and are stored in the
/// database after the first start). `storage` is not among them: it stays in the file.
pub const FILE_RUNTIME_TABLES: [&str; 8] = [
    "sip",
    "media",
    "call",
    "capture",
    "trunks",
    "routes",
    "delivery",
    "retention",
];

/// The parsed runtime part of a file.
#[derive(Debug, Clone)]
pub struct RuntimeFile {
    /// The runtime sections.
    pub runtime: RuntimeConfig,
    /// What fax-in reads (the runtime sections plus `[storage]`).
    pub fax_in: FaxInConfig,
    /// The runtime tables the file actually contains, in the order of [`FILE_RUNTIME_TABLES`].
    pub present: Vec<String>,
}

fn section<T: for<'de> Deserialize<'de> + Default>(
    table: &mut toml::Table,
    name: &str,
) -> Result<T, String> {
    match table.remove(name) {
        None => Ok(T::default()),
        Some(v) => v
            .try_into()
            .map_err(|e: toml::de::Error| format!("[{name}]: {}", e.message())),
    }
}

impl RuntimeFile {
    /// Parses the tables that are left after the application's own (`[log]`, `[database]`) were
    /// removed.
    ///
    /// # Errors
    /// A message naming the table and the problem.
    pub fn from_table(mut table: toml::Table) -> Result<RuntimeFile, String> {
        let present: Vec<String> = FILE_RUNTIME_TABLES
            .iter()
            .filter(|t| table.contains_key(**t))
            .map(|t| (*t).to_string())
            .collect();
        let retention: RetentionSection = section(&mut table, "retention")?;
        let delivery: DeliverySection = section(&mut table, "delivery")?;
        let mut routes = delivery::Routes::default();
        if let Some(v) = table.remove("routes") {
            let mut t = toml::Table::new();
            t.insert("routes".into(), v);
            routes = t
                .try_into()
                .map_err(|e: toml::de::Error| format!("[[routes]]: {}", e.message()))?;
        }
        let fax_in = FaxInConfig::from_table(table).map_err(|e| e.to_string())?;
        let runtime = RuntimeConfig::from_file_parts(&fax_in, routes, delivery, retention);
        let mut problems = Vec::new();
        if let Err(e) = runtime.retention.settings().validate() {
            problems.push(format!("[retention] {e}"));
        }
        if runtime.delivery.max_attempts == 0 {
            problems.push("[delivery] max_attempts must be at least 1".to_string());
        }
        if let Err(errs) = runtime.routes.validate() {
            problems.push(format!("[[routes]] {}", errs.join("\n  ")));
        }
        if !problems.is_empty() {
            return Err(problems.join("\n"));
        }
        Ok(RuntimeFile {
            runtime,
            fax_in,
            present,
        })
    }

    /// Parses configuration text that has no application tables (an exported file).
    ///
    /// # Errors
    /// Invalid TOML, or see [`RuntimeFile::from_table`].
    pub fn parse(text: &str) -> Result<RuntimeFile, String> {
        let table: toml::Table = text
            .parse()
            .map_err(|e: toml::de::Error| format!("invalid TOML: {}", e.message()))?;
        Self::from_table(table)
    }
}
