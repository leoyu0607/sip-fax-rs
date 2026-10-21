//! D05: retry schedule, giving up and restart, with an injected clock (nothing sleeps) and a real
//! SQLite file database.

mod common;

use chrono::Duration;
use common::*;
use delivery::EngineOptions;
use store::{DeliveryStatus, FaxStatus, NewDelivery};

fn hook_routes(url: &str) -> delivery::Routes {
    routes(&format!(
        "[[routes]]\ndefault = true\n[[routes.deliveries]]\nkind = \"webhook\"\nurl = \"{url}\"\n"
    ))
}

#[tokio::test]
async fn d05_schedule_is_1_5_15_60_then_4h_and_gives_up_after_max_attempts() {
    let mut env = Env::new().await;
    let hook = Hook::start(503).await;
    let eng = env.engine(&hook_routes(&hook.url()));
    let f = env.fax("t", "1", FaxStatus::Received).await;
    let before = env.record(f).await;
    eng.dispatch(f).await.unwrap();

    // Minutes after T0 at which attempts 1..=10 run: 0, +1, +5, +15, +60, then every 240.
    let gaps = [1i64, 5, 15, 60, 240, 240, 240, 240, 240];
    let mut at = 0i64;
    let mut starts = vec![0i64];
    for g in gaps {
        at += g;
        starts.push(at);
    }
    for (i, start) in starts.iter().enumerate() {
        let attempt = i as i32 + 1;
        env.clock.set(t(T0 + start * 60_000));
        let rep = eng.run_due().await.unwrap();
        assert_eq!(rep.attempted, 1, "attempt {attempt} at +{start} min");
        let rows = env.attempts(f).await;
        let want_rows = if attempt < 10 { attempt + 1 } else { 10 };
        assert_eq!(rows.len() as i32, want_rows, "rows after attempt {attempt}");
        let last = rows.last().unwrap();
        if attempt < 10 {
            assert_eq!(rep.retry_scheduled, 1);
            assert_eq!(
                (last.status, last.attempt),
                (DeliveryStatus::Pending, attempt + 1)
            );
            let next = start + gaps[i];
            assert_eq!(
                last.next_try_at,
                Some(t(T0 + next * 60_000)),
                "after attempt {attempt}"
            );
            // One millisecond early: nothing is due.
            env.clock.set(t(T0 + next * 60_000 - 1));
            assert_eq!(
                eng.run_due().await.unwrap().attempted,
                0,
                "early at attempt {attempt}"
            );
            assert_eq!(hook.count() as i32, attempt);
        } else {
            assert_eq!(rep.gave_up, 1);
            assert_eq!((last.status, last.attempt), (DeliveryStatus::GivingUp, 10));
            assert_eq!(last.next_try_at, None);
        }
    }
    // Final state: nine failed attempts then giving_up, every failure recorded.
    let rows = env.attempts(f).await;
    assert_eq!(rows.len(), 10);
    for (i, r) in rows.iter().enumerate() {
        assert_eq!(r.attempt, i as i32 + 1);
        assert_eq!(r.last_error.as_deref(), Some("HTTP 503"));
        assert_eq!(
            r.status,
            if i < 9 {
                DeliveryStatus::Failed
            } else {
                DeliveryStatus::GivingUp
            }
        );
    }
    assert_eq!(hook.count(), 10);
    // Much later: nothing more happens.
    env.clock.advance(Duration::days(30));
    assert_eq!(eng.run_due().await.unwrap().attempted, 0);
    assert_eq!(hook.count(), 10);
    // The fax record itself is untouched by the failed delivery.
    assert_eq!(env.record(f).await, before);
}

#[tokio::test]
async fn d05_max_attempts_is_configurable() {
    let mut env = Env::new().await;
    let hook = Hook::start(500).await;
    let opts = EngineOptions {
        max_attempts: 2,
        ..Default::default()
    };
    let eng = env.engine_with(&hook_routes(&hook.url()), opts);
    let f = env.fax("t", "1", FaxStatus::Received).await;
    eng.dispatch(f).await.unwrap();
    assert_eq!(eng.run_due().await.unwrap().retry_scheduled, 1);
    env.clock.advance(Duration::minutes(1));
    assert_eq!(eng.run_due().await.unwrap().gave_up, 1);
    let rows = env.attempts(f).await;
    assert_eq!(
        rows.iter().map(|r| r.status).collect::<Vec<_>>(),
        vec![DeliveryStatus::Failed, DeliveryStatus::GivingUp]
    );
    // max_attempts = 1: the first failure gives up at once.
    let opts = EngineOptions {
        max_attempts: 1,
        ..Default::default()
    };
    let eng = env.engine_with(&hook_routes(&format!("{}x", hook.url())), opts);
    let g = env.fax("t", "1", FaxStatus::Received).await;
    eng.dispatch(g).await.unwrap();
    assert_eq!(eng.run_due().await.unwrap().gave_up, 1);
    assert_eq!(env.attempts(g).await.len(), 1);
}

#[tokio::test]
async fn d05_recovers_on_a_later_attempt() {
    let mut env = Env::new().await;
    let hook = Hook::start(200).await;
    hook.script(&[502, 503]);
    let eng = env.engine(&hook_routes(&hook.url()));
    let f = env.fax("t", "1", FaxStatus::Received).await;
    eng.dispatch(f).await.unwrap();
    eng.run_due().await.unwrap();
    env.clock.advance(Duration::minutes(1));
    eng.run_due().await.unwrap();
    env.clock.advance(Duration::minutes(5));
    let rep = eng.run_due().await.unwrap();
    assert_eq!(rep.done, 1);
    let rows = env.attempts(f).await;
    assert_eq!(
        rows.iter()
            .map(|r| (r.attempt, r.status))
            .collect::<Vec<_>>(),
        vec![
            (1, DeliveryStatus::Failed),
            (2, DeliveryStatus::Failed),
            (3, DeliveryStatus::Done)
        ]
    );
    assert_eq!(rows[2].last_error, None);
    assert_eq!(rows[2].updated_at, t(T0 + 6 * 60_000));
    assert_eq!(hook.count(), 3);
}

#[tokio::test]
async fn d05_connection_failure_is_retried_until_the_server_returns() {
    let mut env = Env::new().await;
    // Reserve a port, release it, and start the real server there only after the first failure.
    let port = closed_port().await;
    let eng = env.engine(&hook_routes(&format!("http://127.0.0.1:{port}/hook")));
    let f = env.fax("t", "1", FaxStatus::Received).await;
    eng.dispatch(f).await.unwrap();
    assert_eq!(eng.run_due().await.unwrap().retry_scheduled, 1);
    let rows = env.attempts(f).await;
    assert_eq!(rows[1].next_try_at, Some(t(T0 + 60_000)));

    let hook = Hook::start_on(port, 200).await;
    env.clock.advance(Duration::minutes(1));
    assert_eq!(eng.run_due().await.unwrap().done, 1);
    assert_eq!(hook.count(), 1);
}

#[tokio::test]
async fn d05_restart_continues_from_the_database() {
    let mut env = Env::new().await;
    let hook = Hook::start(503).await;
    let routes = hook_routes(&hook.url());
    let eng = env.engine(&routes);
    let f = env.fax("t", "1", FaxStatus::Received).await;
    eng.dispatch(f).await.unwrap();
    eng.run_due().await.unwrap(); // attempt 1 fails -> pending at +1 min
    env.clock.advance(Duration::minutes(1));
    eng.run_due().await.unwrap(); // attempt 2 fails -> pending at +1+5 min
    assert_eq!(hook.count(), 2);

    // "Restart": drop the engine, close and reopen the database file, build a new engine.
    drop(eng);
    env.restart_store().await;
    let eng = env.engine(&routes);
    let rows = env.attempts(f).await;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[2].status, DeliveryStatus::Pending);
    assert_eq!(rows[2].next_try_at, Some(t(T0 + 6 * 60_000)));

    // Not yet due after the restart.
    env.clock.set(t(T0 + 6 * 60_000 - 1));
    assert_eq!(eng.run_due().await.unwrap().attempted, 0);
    // Due: the server has recovered meanwhile; the third attempt delivers.
    hook.set_status(200);
    env.clock.set(t(T0 + 6 * 60_000));
    assert_eq!(eng.run_due().await.unwrap().done, 1);
    let rows = env.attempts(f).await;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[2].status, DeliveryStatus::Done);
    assert_eq!(hook.count(), 3);
    // Already delivered: dispatch again (as catch_up would) and run: still exactly 3 requests.
    assert!(eng.dispatch(f).await.unwrap().is_empty());
    assert_eq!(eng.run_due().await.unwrap().attempted, 0);
    assert_eq!(hook.count(), 3);
}

#[tokio::test]
async fn d05_permanent_error_is_not_retried() {
    let mut env = Env::new().await;
    let hook = Hook::start(404).await;
    let eng = env.engine(&hook_routes(&hook.url()));
    let f = env.fax("t", "1", FaxStatus::Received).await;
    eng.dispatch(f).await.unwrap();
    let rep = eng.run_due().await.unwrap();
    assert_eq!((rep.failed, rep.retry_scheduled, rep.gave_up), (1, 0, 0));
    env.clock.advance(Duration::days(2));
    assert_eq!(eng.run_due().await.unwrap().attempted, 0);
    let rows = env.attempts(f).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].status, rows[0].next_try_at),
        (DeliveryStatus::Failed, None)
    );
    assert_eq!(hook.count(), 1);
}

#[tokio::test]
async fn d05_pending_row_with_successor_is_closed_not_run_twice() {
    // A crash after "insert the next attempt" but before "close this one" leaves this shape.
    let mut env = Env::new().await;
    let hook = Hook::start(200).await;
    let eng = env.engine(&hook_routes(&hook.url()));
    let f = env.fax("t", "1", FaxStatus::Received).await;
    let ids = eng.dispatch(f).await.unwrap();
    let target = env.attempts(f).await[0].target.clone();
    env.store
        .add_delivery(&NewDelivery {
            fax_id: f,
            target,
            kind: "webhook".into(),
            attempt: 2,
            status: DeliveryStatus::Pending,
            next_try_at: Some(t(T0 + 60_000)),
        })
        .await
        .unwrap();
    let rep = eng.run_due().await.unwrap();
    assert_eq!(rep.attempted, 0, "the superseded row is not an attempt");
    assert_eq!(hook.count(), 0);
    let rows = env.attempts(f).await;
    assert_eq!(rows[0].id, ids[0]);
    assert_eq!(rows[0].status, DeliveryStatus::Failed);
    assert!(
        rows[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("superseded")
    );
    assert_eq!(rows[1].status, DeliveryStatus::Pending);
    env.clock.advance(Duration::minutes(1));
    assert_eq!(eng.run_due().await.unwrap().done, 1);
    assert_eq!(hook.count(), 1);
}

#[tokio::test]
async fn d05_orphans_are_closed_cleanly() {
    let mut env = Env::new().await;
    let hook = Hook::start(200).await;
    let f = env.fax("t", "1", FaxStatus::Received).await;
    let eng = env.engine(&hook_routes(&hook.url()));
    eng.dispatch(f).await.unwrap();
    // Config changed between runs: the same fax now matches another target.
    let other = env.engine(&hook_routes(&format!("{}-other", hook.url())));
    let rep = other.run_due().await.unwrap();
    assert_eq!((rep.failed, hook.count()), (1, 0));
    let row = &env.attempts(f).await[0];
    assert_eq!(row.status, DeliveryStatus::Failed);
    assert!(
        row.last_error
            .as_deref()
            .unwrap()
            .contains("no longer configured")
    );
}
