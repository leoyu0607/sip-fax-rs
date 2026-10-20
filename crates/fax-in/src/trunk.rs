//! Trunk table: maps the source IP of an inbound call to its trunk profile (C02) and derives
//! the transport-level source filter from the union of all trunks' allowed sources.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use sip_core::transport::{Cidr, SourcePolicy};

use crate::config::{ConfigError, FaxInConfig, TrunkConfig, UnknownSource};

/// Name used for calls from sources outside every trunk when `unknown_source = "log"`.
pub const UNKNOWN_TRUNK: &str = "unknown";

/// A trunk with its resolved addresses.
#[derive(Debug)]
pub struct Trunk {
    /// The configuration (preset already applied).
    pub cfg: TrunkConfig,
    /// Networks calls may come from.
    pub allowed: Vec<Cidr>,
    /// The peer address, if configured.
    pub peer: Option<SocketAddr>,
}

impl Trunk {
    /// The trunk name.
    pub fn name(&self) -> &str {
        &self.cfg.name
    }
}

/// All trunks of a configuration.
#[derive(Debug)]
pub struct TrunkTable {
    trunks: Vec<Arc<Trunk>>,
    fallback: Arc<Trunk>,
    unknown_source: UnknownSource,
}

impl TrunkTable {
    /// Resolves and validates every trunk of `cfg`.
    pub fn build(cfg: &FaxInConfig) -> Result<TrunkTable, ConfigError> {
        let mut trunks: Vec<Arc<Trunk>> = Vec::new();
        for t in &cfg.trunks {
            if trunks.iter().any(|o| o.cfg.name == t.name) {
                return Err(ConfigError::Invalid(format!(
                    "duplicate trunk name \"{}\"",
                    t.name
                )));
            }
            let (allowed, peer) = t.validate()?;
            trunks.push(Arc::new(Trunk {
                cfg: t.clone(),
                allowed,
                peer,
            }));
        }
        let fallback = Arc::new(Trunk {
            cfg: TrunkConfig {
                name: UNKNOWN_TRUNK.into(),
                ..TrunkConfig::default()
            },
            allowed: Vec::new(),
            peer: None,
        });
        for t in &trunks {
            if !cfg.transports().contains(&t.cfg.transport) {
                return Err(ConfigError::Invalid(format!(
                    "trunk \"{}\" uses transport {:?} but [sip] transports does not open it",
                    t.cfg.name, t.cfg.transport
                )));
            }
        }
        Ok(TrunkTable {
            trunks,
            fallback,
            unknown_source: cfg.sip.unknown_source,
        })
    }

    /// The trunk whose allowed sources contain `ip` (the first one in configuration order).
    pub fn lookup(&self, ip: IpAddr) -> Option<Arc<Trunk>> {
        self.trunks
            .iter()
            .find(|t| t.allowed.iter().any(|c| c.contains(ip)))
            .cloned()
    }

    /// The profile used for unknown sources in `log` mode (a `generic` trunk).
    pub fn fallback(&self) -> Arc<Trunk> {
        self.fallback.clone()
    }

    /// All trunks in configuration order.
    pub fn trunks(&self) -> &[Arc<Trunk>] {
        &self.trunks
    }

    /// What to do with unknown sources.
    pub fn unknown_source(&self) -> UnknownSource {
        self.unknown_source
    }

    /// The transport-level filter: the union of all allowed sources, `unknown_source` as mode.
    pub fn source_policy(&self) -> SourcePolicy {
        let all: Vec<Cidr> = self
            .trunks
            .iter()
            .flat_map(|t| t.allowed.iter().copied())
            .collect();
        SourcePolicy::new(all, self.unknown_source.policy_mode())
    }
}
