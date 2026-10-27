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
