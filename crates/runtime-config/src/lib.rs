//! Runtime configuration kept in the database (M4a, ADR-051; `docs/design/m4-web.md` section 1).
//!
//! * [`model`]: [`RuntimeConfig`], its sections and their names;
//! * [`validate`]: per-field validation of a section (the 422 body of the REST layer);
//! * [`secret`] and [`codec`]: AES-256-GCM encryption of secret fields (`conf/secret.key`) and
//!   the stored / plain / API forms of a body;
//! * [`service`]: [`ConfigService`], the one API that changes, imports, exports and applies the
//!   configuration.

#![deny(missing_docs)]

pub mod codec;
pub mod file;
pub mod model;
pub mod secret;
pub mod service;
pub mod validate;

pub use codec::{SecretIssue, UNDECRYPTABLE};
pub use file::RuntimeFile;
pub use model::{DeliverySection, RetentionSection, RuntimeConfig, Section, TRUNK_ORDER};
pub use secret::{KeyStatus, SecretBox, SecretError};
pub use service::{
    ApplyReport, Boot, ConfigApplier, ConfigService, ConfigStatus, ImportError, ImportReport,
    ReloadOutcome, SectionView, Source, StoreSource, UpdateError, UpdateOutcome, to_toml,
};
pub use validate::{FieldError, SectionValue, parse_section};
