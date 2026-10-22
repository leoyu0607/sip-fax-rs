//! Delivery of received faxes: folder, e-mail and webhook, with persistent retries (M3c).
//!
//! * [`Routes`] / [`Route`] / [`DeliveryConfig`]: serde types for `[[routes]]` and
//!   `[[routes.deliveries]]`; [`Routes::validate`] backs `sip-faxd config check`.
//! * [`Router::find`]: the first route matching a [`store::FaxRecord`].
//! * [`DeliveryEngine`]: turns a finalized fax into `delivery_attempts` rows and works them off
//!   with the retry schedule 1, 5, 15, 60 minutes, then every 4 hours, at most `max_attempts`
//!   (default 10) attempts. The queue is the database table, so a restart simply continues.
//!   A failed delivery never touches the fax record.
//!
//! # Wiring it into sip-faxd
//!
//! ```ignore
//! // config: `routes: delivery::Routes` flattened into the daemon config (or `Vec<Route>`).
//! // `config check`:
//! cfg.routes.validate().map_err(|errs| errs.join("\n"))?;
//!
//! // serve: build once after the store is connected, using the store's clock.
//! let engine = delivery::DeliveryEngine::new(
//!     store.clone(), &cfg.routes, &cfg.data_dir,
//!     Arc::new(store::SystemClock), delivery::EngineOptions::default(),
//! )?;
//! engine.catch_up(Utc::now() - chrono::Duration::days(1)).await?; // repair after a crash
//! let delivery = engine.spawn();
//!
//! // fax-in event loop, right after `store.finalize(..)` succeeded:
//! delivery.notify_finalized(record.id);   // non-blocking, never fails the call path
//!
//! // shutdown:
//! delivery.shutdown().await;
//! ```
//!
//! Tests drive the engine without the background task: `engine.dispatch(id)` then
//! `engine.run_due()` with a `store::ManualClock` that is also the store's clock.

pub mod config;
pub mod email;
pub mod engine;
pub mod error;
pub mod folder;
pub mod mask;
pub mod retry;
pub mod router;
pub mod source;
pub mod template;
pub mod webhook;

pub use config::{
    DeliveryConfig, EmailConfig, FolderConfig, Format, Route, Routes, Security, WebhookConfig,
};
pub use engine::{ChangeHook, DeliveryEngine, DeliveryHandle, EngineOptions, RunReport};
pub use error::{DeliveryError, Error, FailureKind, Result};
pub use retry::{DEFAULT_MAX_ATTEMPTS, backoff};
pub use router::Router;
