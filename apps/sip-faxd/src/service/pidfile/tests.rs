use super::*;

#[test]
fn m01_the_lock_writes_the_pid_excludes_a_second_holder_and_cleans_up() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("run").join("x.pid");
    assert_eq!(running_pid(&p), None, "no file");
    let lock = PidLock::acquire(&p).unwrap();
    assert_eq!(read_pid(&p), Some(std::process::id()));
    // flock locks belong to the open file description: a second open in this process
    // conflicts just like another process would
    assert_eq!(running_pid(&p), Some(std::process::id()));
    match PidLock::acquire(&p) {
        Err(LockError::Held { pid }) => assert_eq!(pid, Some(std::process::id())),
        other => panic!("second lock: {other:?}"),
    }
    assert!(
        PidLock::acquire(&p)
            .unwrap_err()
            .to_string()
            .contains("already running")
    );
    drop(lock);
    assert!(!p.exists(), "the file goes away with the lock");
    assert_eq!(running_pid(&p), None);
    // and it can be taken again
    drop(PidLock::acquire(&p).unwrap());
}

#[test]
fn m03_a_stale_file_without_a_lock_is_not_a_running_service() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("x.pid");
    fs::write(&p, "999999\n").unwrap();
    assert_eq!(read_pid(&p), Some(999_999));
    assert_eq!(running_pid(&p), None, "the pid in the file means nothing");
    remove_if_stale(&p);
    assert!(!p.exists());
    // taking it over works too
    fs::write(&p, "999999\n").unwrap();
    let _l = PidLock::acquire(&p).unwrap();
    assert_eq!(read_pid(&p), Some(std::process::id()));
    // a held lock is not removed as stale
    remove_if_stale(&p);
    assert!(p.exists());
}

#[test]
fn m01_a_garbage_pid_file_reads_as_none() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("x.pid");
    fs::write(&p, "not a pid").unwrap();
    assert_eq!(read_pid(&p), None);
    assert_eq!(read_pid(&d.path().join("missing")), None);
}
