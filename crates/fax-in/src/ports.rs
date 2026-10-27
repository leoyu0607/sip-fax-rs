//! Per-call UDPTL port allocation from `[media] port_range`.
//!
//! The pool hands out the even ports of the range, one per call, first-in first-out so that a
//! port is reused as late as possible (late UDPTL datagrams of the previous call are then much
//! less likely to hit the next one). A [`PortLease`] gives its port back when dropped, whatever
//! way the call ends.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use crate::config::PortRange;

#[derive(Debug)]
struct Inner {
    free: Mutex<VecDeque<u16>>,
    capacity: usize,
}

/// The set of free UDPTL ports.
#[derive(Debug, Clone)]
pub struct PortPool {
    inner: Arc<Inner>,
}

/// A port taken from a [`PortPool`]; returned to it on drop.
#[derive(Debug)]
pub struct PortLease {
    port: u16,
    pool: Arc<Inner>,
}

impl PortPool {
    /// A pool holding the even ports of `range`.
    pub fn new(range: PortRange) -> PortPool {
        let free: VecDeque<u16> = range.ports().collect();
        PortPool {
            inner: Arc::new(Inner {
                capacity: free.len(),
                free: Mutex::new(free),
            }),
        }
    }

    /// Takes a free port, or `None` when every port is in use.
    pub fn acquire(&self) -> Option<PortLease> {
        let port = self
            .inner
            .free
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()?;
        Some(PortLease {
            port,
            pool: self.inner.clone(),
        })
    }

    /// Ports currently free.
    pub fn available(&self) -> usize {
        self.inner
            .free
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Total ports in the pool.
    pub fn capacity(&self) -> usize {
        self.inner.capacity
    }
}

impl PortLease {
    /// The leased (even) port.
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for PortLease {
    fn drop(&mut self) {
        self.pool
            .free
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(self.port);
    }
}

#[cfg(test)]
mod tests;
