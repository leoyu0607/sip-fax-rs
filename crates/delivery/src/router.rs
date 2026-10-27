//! Route matching.

use crate::config::{Route, Routes};
use store::FaxRecord;

/// Picks the first matching route.
#[derive(Debug, Clone, Default)]
pub struct Router {
    routes: Vec<Route>,
}

impl Router {
    /// Build from the configured routes (call [`Routes::validate`] first).
    pub fn new(routes: &Routes) -> Self {
        Router {
            routes: routes.routes.clone(),
        }
    }

    /// First route, in configuration order, that matches `rec` (`dnis`, `dnis_prefix` and `trunk`
    /// are ANDed when several are set on one route; `default = true` matches everything).
    pub fn find(&self, rec: &FaxRecord) -> Option<&Route> {
        self.routes.iter().find(|r| r.matches(rec))
    }

    /// The routes in order.
    pub fn routes(&self) -> &[Route] {
        &self.routes
    }
}

#[cfg(test)]
mod tests;
