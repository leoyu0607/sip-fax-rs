use super::*;
use crate::service::hub::{HubInfo, Progress};
use std::os::unix::fs::MetadataExt;

fn hub() -> Arc<Hub> {
    Hub::new(
        HubInfo {
            pid: std::process::id(),
            version: "t".into(),
            listen: vec![],
        },
        Duration::from_secs(300),
        Vec::new,
    )
}

fn short_dir() -> tempfile::TempDir {
    // unix socket paths are limited to about 100 bytes
    tempfile::Builder::new()
        .prefix("sf")
        .tempdir_in("/tmp")
        .unwrap()
}

#[tokio::test]
async fn m11_the_bind_leaves_only_the_socket_in_its_directory() {
    // the private directory the socket is bound in is gone afterwards
    let d = short_dir();
    let p = d.path().join("c.sock");
    let srv = ControlServer::bind(&p, hub()).unwrap();
    let names: Vec<String> = std::fs::read_dir(d.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["c.sock".to_string()]);
    srv.shutdown().await;
}

#[tokio::test]
async fn m10_the_socket_is_mode_0600_and_answers_status_even_under_a_permissive_umask() {
    let d = short_dir();
    let p = d.path().join("run").join("c.sock");
    let old = rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o000));
    let srv = ControlServer::bind(&p, hub());
    rustix::process::umask(old);
    let srv = srv.unwrap();
    let meta = std::fs::metadata(&p).unwrap();
    assert_eq!(meta.mode() & 0o777, 0o600, "mode {:o}", meta.mode());
    let s = fetch_status(&p).await.unwrap();
    assert_eq!((s.pid, s.state.as_str()), (std::process::id(), "running"));
    srv.shutdown().await;
    assert!(!p.exists(), "the socket file goes away");
}

#[tokio::test]
async fn m03_a_stale_socket_file_is_replaced_and_a_live_one_is_refused() {
    let d = short_dir();
    let p = d.path().join("c.sock");
    // a socket nobody listens on: bind, then drop the listener without unlinking
    drop(std::os::unix::net::UnixListener::bind(&p).unwrap());
    assert!(p.exists());
    let srv = ControlServer::bind(&p, hub()).expect("stale file replaced");
    assert!(fetch_status(&p).await.is_ok());
    let second = ControlServer::bind(&p, hub());
    let e = second.err().expect("a live socket is refused");
    assert_eq!(e.kind(), io::ErrorKind::AddrInUse);
    assert!(e.to_string().contains("another sip-faxd"), "{e}");
    // the first one still works
    assert!(fetch_status(&p).await.is_ok());
    srv.shutdown().await;
}

#[tokio::test]
async fn m03_a_path_that_is_too_long_for_a_unix_socket_is_reported_with_the_path() {
    let d = short_dir();
    let p = d.path().join("x".repeat(120)).join("c.sock");
    let e = ControlServer::bind(&p, hub()).err().expect("too long");
    assert!(e.to_string().contains("control socket"), "{e}");
}

#[tokio::test]
async fn m09_a_stop_over_the_real_socket_ends_with_stopped_and_shutdown_waits_for_it() {
    let d = short_dir();
    let p = d.path().join("c.sock");
    let h = hub();
    let srv = ControlServer::bind(&p, h.clone()).unwrap();
    let mut c = Client::connect(&p).await.unwrap();
    c.send(&Request::Stop {
        force: false,
        timeout: Some(30),
    })
    .await
    .unwrap();
    assert!(matches!(
        c.next().await.unwrap(),
        Some(Reply::Stopping {
            timeout_secs: 30,
            ..
        })
    ));
    assert_eq!(h.stop_requested().await.timeout, Duration::from_secs(30));
    h.set_progress(Progress::Stopped);
    srv.shutdown().await;
    assert_eq!(c.next().await.unwrap(), Some(Reply::Stopped));
    assert_eq!(c.next().await.unwrap(), None);
}

#[tokio::test]
async fn m07_the_client_reads_replies_line_by_line() {
    let d = short_dir();
    let p = d.path().join("c.sock");
    let srv = ControlServer::bind(&p, hub()).unwrap();
    let mut c = Client::connect(&p).await.unwrap();
    assert!(
        matches!(c.call(&Request::Calls).await.unwrap(), Reply::Calls { calls } if calls.is_empty())
    );
    assert!(matches!(
        c.call(&Request::Status).await.unwrap(),
        Reply::Status(_)
    ));
    srv.shutdown().await;
    assert!(Client::connect(&p).await.is_err(), "no socket any more");
}
