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
mod tests {
    use super::*;

    fn pool(text: &str) -> PortPool {
        PortPool::new(PortRange::parse(text).unwrap())
    }

    #[test]
    fn hands_out_even_ports_only_and_ascending_first() {
        let p = pool("30001-30008");
        assert_eq!(p.capacity(), 4);
        let got: Vec<u16> = (0..4).map(|_| p.acquire().unwrap().port()).collect();
        // leases were dropped immediately, so the order rotates: 30002, 30004, 30006, 30008
        assert_eq!(got, vec![30002, 30004, 30006, 30008]);
    }

    #[test]
    fn exhausted_pool_returns_none_and_release_makes_ports_available_again() {
        let p = pool("30000-30003");
        assert_eq!((p.capacity(), p.available()), (2, 2));
        let a = p.acquire().unwrap();
        let b = p.acquire().unwrap();
        assert_eq!((a.port(), b.port()), (30000, 30002));
        assert_eq!(p.available(), 0);
        assert!(p.acquire().is_none());
        drop(a);
        assert_eq!(p.available(), 1);
        let c = p.acquire().unwrap();
        assert_eq!(c.port(), 30000);
        // FIFO reuse: after b is released the next lease is not b's port while others are free
        drop(b);
        drop(c);
        assert_eq!(p.acquire().unwrap().port(), 30002);
    }

    #[test]
    fn no_port_is_ever_leased_twice() {
        let p = pool("40000-40099");
        let mut held = Vec::new();
        let mut seen = std::collections::HashSet::new();
        while let Some(l) = p.acquire() {
            assert!(seen.insert(l.port()), "port {} leased twice", l.port());
            assert_eq!(l.port() % 2, 0);
            held.push(l);
        }
        assert_eq!(held.len(), 50);
        drop(held);
        assert_eq!(p.available(), 50);
    }
}
