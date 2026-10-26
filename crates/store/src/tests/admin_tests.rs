//! M4a store cases: K13 (settings -> retention section), K06 (optimistic lock, store level),
//! the audit trail, users, sessions and the manual re-delivery. Run on every backend by the
//! db-matrix.

use super::*;
use crate::migrate::{Migration, apply, migrations};
use chrono::Duration;

fn audit(action: &str, before: Option<&str>, after: Option<&str>) -> ConfigAudit {
    ConfigAudit {
        action: action.into(),
        before: before.map(str::to_string),
        after: after.map(str::to_string),
    }
}

async fn drop_all(env: &Env) {
    let be = env.store.raw();
    for stmt in [
        "DROP VIEW fax_records",
        "DROP TABLE api_keys",
        "DROP TABLE sessions",
        "DROP TABLE users",
        "DROP TABLE config_audit",
        "DROP TABLE config_sections",
        "DROP TABLE delivery_attempts",
        "DROP TABLE settings",
        "DROP TABLE faxes",
        "DROP TABLE schema_migrations",
    ] {
        be.execute(stmt, &[]).await.unwrap();
    }
}

/// A database that is at schema version 1, as M3 left it.
async fn at_version_one(env: &Env) {
    drop_all(env).await;
    let kind = env.store.backend();
    let first: Vec<Migration> = migrations(kind)[..1]
        .iter()
        .map(|m| Migration {
            version: m.version,
            name: m.name,
            sql: m.sql,
        })
        .collect();
    let rep = apply(env.store.raw(), env.clock.as_ref(), &first)
        .await
        .unwrap();
    assert_eq!(rep.to, 1);
}

// ---------------------------------------------------------------- K13
db_case!(
    k13_retention_settings_move_into_the_section,
    k13_retention_settings_move_into_the_section_body
);
async fn k13_retention_settings_move_into_the_section_body(env: &Env) {
    if env.is_memory() {
        return;
    }
    at_version_one(env).await;
    let st = &env.store;
    st.set_setting("retention.files_days", "30", Some("admin"))
        .await
        .unwrap();
    st.set_setting("retention.records_days", "400", Some("admin"))
        .await
        .unwrap();
    st.set_setting("retention.run_at", "01:05", Some("admin"))
        .await
        .unwrap();
    st.set_setting("other.key", "keep me", None).await.unwrap();
    let rep = st.migrate().await.unwrap();
    assert_eq!((rep.from, rep.to, rep.applied), (1, 4, vec![2, 3, 4]));
    let s = purge::load_retention(st.as_ref() as &dyn FaxStore)
        .await
        .unwrap();
    assert_eq!(
        (
            s.policy.files_days,
            s.policy.records_days,
            s.run_at.as_str()
        ),
        (30, 400, "01:05"),
        "the values are the ones of the settings table"
    );
    let row = st
        .config_get(purge::RETENTION_SECTION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (row.version, row.updated_by.as_deref()),
        (1, Some("migration"))
    );
    // the retention keys are gone from `settings`, the others stay
    let names: Vec<String> = st
        .list_settings()
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["other.key"]);
    // running the migration statements again changes nothing (MySQL reruns a failed migration)
    st.set_setting("retention.files_days", "7", None)
        .await
        .unwrap();
    for stmt in crate::db::split_statements(migrations(st.backend())[1].sql) {
        if stmt
            .to_ascii_uppercase()
            .starts_with("INSERT INTO CONFIG_SECTIONS")
        {
            st.raw().execute(&stmt, &[]).await.unwrap();
        }
    }
    assert_eq!(
        purge::load_retention(st.as_ref() as &dyn FaxStore)
            .await
            .unwrap()
            .policy
            .files_days,
        30,
        "an existing section is not overwritten"
    );
}

db_case!(
    k13b_partial_and_empty_settings,
    k13b_partial_and_empty_settings_body
);
async fn k13b_partial_and_empty_settings_body(env: &Env) {
    if env.is_memory() {
        return;
    }
    // only one key was ever written: the others take the M3 defaults
    at_version_one(env).await;
    env.store
        .set_setting("retention.files_days", "10", None)
        .await
        .unwrap();
    env.store.migrate().await.unwrap();
    let s = purge::load_retention(env.store.as_ref() as &dyn FaxStore)
        .await
        .unwrap();
    assert_eq!(
        (
            s.policy.files_days,
            s.policy.records_days,
            s.run_at.as_str()
        ),
        (10, 365, "03:17")
    );
    // no retention settings at all: no section is invented
    at_version_one(env).await;
    env.store.migrate().await.unwrap();
    assert!(
        env.store
            .config_get(purge::RETENTION_SECTION)
            .await
            .unwrap()
            .is_none()
    );
}

// ---------------------------------------------------------------- K06 (store level)
db_case!(k06_config_put_is_an_optimistic_lock, k06_config_put_body);
async fn k06_config_put_body(env: &Env) {
    let st = &env.store;
    assert!(st.config_get("sip").await.unwrap().is_none());
    // create
    env.clock.set(t(T0 + 1_000));
    let a = st
        .config_put(
            "sip",
            r#"{"a":1}"#,
            None,
            "admin",
            &audit("create", None, Some(r#"{"a":1}"#)),
        )
        .await
        .unwrap();
    assert_eq!(
        (a.version, a.updated_by.as_deref(), a.updated_at),
        (1, Some("admin"), t(T0 + 1_000))
    );
    // a second create is a conflict
    assert!(matches!(
        st.config_put("sip", "{}", None, "x", &audit("create", None, None))
            .await,
        Err(Error::Conflict(_))
    ));
    // two writers hold version 1: exactly one wins
    let (s1, s2) = (st.clone(), st.clone());
    let (r1, r2) = tokio::join!(
        async move {
            s1.config_put(
                "sip",
                r#"{"a":2}"#,
                Some(1),
                "alice",
                &audit("update", None, None),
            )
            .await
        },
        async move {
            s2.config_put(
                "sip",
                r#"{"a":3}"#,
                Some(1),
                "bob",
                &audit("update", None, None),
            )
            .await
        },
    );
    let wins = [&r1, &r2].iter().filter(|r| r.is_ok()).count();
    assert_eq!(wins, 1, "{r1:?} {r2:?}");
    let loser = if r1.is_err() { r1 } else { r2 };
    match loser {
        Err(Error::Conflict(m)) => assert!(m.contains("version 2"), "{m}"),
        other => panic!("{other:?}"),
    }
    let cur = st.config_get("sip").await.unwrap().unwrap();
    assert_eq!(cur.version, 2);
    // a missing section is NotFound, not a conflict
    assert!(matches!(
        st.config_put("nope", "{}", Some(1), "x", &audit("update", None, None))
            .await,
        Err(Error::NotFound(_))
    ));
    // list is ordered by name; delete needs the current version
    st.config_put("media", "{}", None, "admin", &audit("create", None, None))
        .await
        .unwrap();
    let names: Vec<_> = st
        .config_list()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert_eq!(names, ["media", "sip"]);
    assert!(matches!(
        st.config_delete("sip", 1, "admin", &audit("delete", None, None))
            .await,
        Err(Error::Conflict(_))
    ));
    st.config_delete("sip", 2, "admin", &audit("delete", Some("{}"), None))
        .await
        .unwrap();
    assert!(st.config_get("sip").await.unwrap().is_none());
    assert!(matches!(
        st.config_delete("sip", 2, "admin", &audit("delete", None, None))
            .await,
        Err(Error::NotFound(_))
    ));
    // a name that does not fit the column is refused up front
    assert!(matches!(
        st.config_put(
            &"x".repeat(65),
            "{}",
            None,
            "a",
            &audit("create", None, None)
        )
        .await,
        Err(Error::TooLong { .. })
    ));
}

db_case!(k06b_the_audit_trail, k06b_the_audit_trail_body);
async fn k06b_the_audit_trail_body(env: &Env) {
    let st = &env.store;
    for i in 0..5 {
        env.clock.set(t(T0 + i * 1_000));
        let (exp, act) = if i == 0 {
            (None, "create")
        } else {
            (Some(i as i32), "update")
        };
        st.config_put(
            "call",
            &format!(r#"{{"n":{i}}}"#),
            exp,
            "admin",
            &audit(
                act,
                Some(&format!("before {i}")),
                Some("after \u{4e2d}\u{6587} '; --"),
            ),
        )
        .await
        .unwrap();
    }
    let all = st.config_audit_list(100, None).await.unwrap();
    assert_eq!(all.len(), 5);
    assert!(all.windows(2).all(|w| w[0].id > w[1].id), "newest first");
    assert_eq!(all[4].action, "create");
    assert_eq!(all[0].action, "update");
    assert_eq!(all[0].before_json.as_deref(), Some("before 4"));
    assert_eq!(
        all[0].after_json.as_deref(),
        Some("after \u{4e2d}\u{6587} '; --")
    );
    assert_eq!((all[0].by.as_str(), all[0].at), ("admin", t(T0 + 4_000)));
    // paging
    let p1 = st.config_audit_list(2, None).await.unwrap();
    let p2 = st.config_audit_list(2, Some(p1[1].id)).await.unwrap();
    let p3 = st.config_audit_list(2, Some(p2[1].id)).await.unwrap();
    assert_eq!((p1.len(), p2.len(), p3.len()), (2, 2, 1));
    assert_eq!(p3[0].id, all[4].id);
    // a failed (conflicting) change leaves no audit row
    let _ = st
        .config_put(
            "call",
            "{}",
            Some(1),
            "mallory",
            &audit("update", None, None),
        )
        .await
        .unwrap_err();
    assert_eq!(st.config_audit_list(100, None).await.unwrap().len(), 5);
}

// ---------------------------------------------------------------- users
fn new_user(name: &str, role: &str) -> NewUser {
    NewUser {
        username: name.into(),
        display_name: Some("Display \u{540d}".into()),
        role: role.into(),
        password_hash: "$argon2id$stub".into(),
        locale: Some("zh-TW".into()),
        must_change_password: true,
    }
}

db_case!(a12s_users_store, a12s_users_store_body);
async fn a12s_users_store_body(env: &Env) {
    let st = &env.store;
    env.clock.set(t(T0));
    assert_eq!(st.user_count().await.unwrap(), 0);
    let alice = st.user_create(&new_user("alice", "admin")).await.unwrap();
    assert_eq!(alice.username, "alice");
    assert!(alice.must_change_password && !alice.disabled);
    assert_eq!(alice.display_name.as_deref(), Some("Display \u{540d}"));
    assert_eq!((alice.created_at, alice.last_login_at), (t(T0), None));
    assert!(matches!(
        st.user_create(&new_user("alice", "agent")).await,
        Err(Error::Duplicate(_))
    ));
    let bob = st.user_create(&new_user("bob", "agent")).await.unwrap();
    assert_eq!(st.user_count().await.unwrap(), 2);
    assert_eq!(st.user_count_active_admins().await.unwrap(), 1);
    assert_eq!(
        st.user_get_by_name("bob").await.unwrap().unwrap().id,
        bob.id
    );
    assert!(st.user_get_by_name("nobody").await.unwrap().is_none());
    assert!(st.user_get(9_999).await.unwrap().is_none());
    let names: Vec<_> = st
        .user_list()
        .await
        .unwrap()
        .into_iter()
        .map(|u| u.username)
        .collect();
    assert_eq!(names, ["alice", "bob"]);

    env.clock.set(t(T0 + 5_000));
    let upd = st
        .user_update(
            bob.id,
            &UserUpdate {
                role: Some("admin".into()),
                disabled: Some(true),
                must_change_password: Some(false),
                display_name: Some(None),
                locale: Some(Some("en".into())),
                password_hash: Some("$argon2id$new".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        (
            upd.role.as_str(),
            upd.disabled,
            upd.must_change_password,
            upd.display_name.clone(),
            upd.locale.as_deref(),
            upd.password_hash.as_str(),
            upd.updated_at
        ),
        (
            "admin",
            true,
            false,
            None,
            Some("en"),
            "$argon2id$new",
            t(T0 + 5_000)
        )
    );
    assert_eq!(
        st.user_count_active_admins().await.unwrap(),
        1,
        "the disabled admin does not count"
    );
    // an empty update only touches updated_at
    let same = st
        .user_update(bob.id, &UserUpdate::default())
        .await
        .unwrap();
    assert_eq!(same.role, "admin");
    assert!(matches!(
        st.user_update(9_999, &UserUpdate::default()).await,
        Err(Error::NotFound(_))
    ));
    st.user_touch_login(alice.id, t(T0 + 9_000)).await.unwrap();
    assert_eq!(
        st.user_get(alice.id).await.unwrap().unwrap().last_login_at,
        Some(t(T0 + 9_000))
    );
    assert!(matches!(
        st.user_create(&NewUser {
            username: "x".repeat(65),
            ..new_user("x", "agent")
        })
        .await,
        Err(Error::TooLong { .. })
    ));
    // delete removes the sessions too
    st.session_create(&NewSession {
        token_hash: "a".repeat(64),
        user_id: bob.id,
        created_at: t(T0),
        expires_at: t(T0 + 1_000_000),
        ip: None,
        user_agent: None,
    })
    .await
    .unwrap();
    assert!(st.user_delete(bob.id).await.unwrap());
    assert!(!st.user_delete(bob.id).await.unwrap());
    assert!(st.session_get(&"a".repeat(64)).await.unwrap().is_none());
}

db_case!(a07s_sessions_store, a07s_sessions_store_body);
async fn a07s_sessions_store_body(env: &Env) {
    let st = &env.store;
    let u = st.user_create(&new_user("carol", "agent")).await.unwrap();
    let v = st.user_create(&new_user("dave", "agent")).await.unwrap();
    let mk = |c: char, uid: i64, exp_s: i64| NewSession {
        token_hash: c.to_string().repeat(64),
        user_id: uid,
        created_at: t(T0),
        expires_at: t(T0 + exp_s * 1_000),
        ip: Some("203.0.113.9".into()),
        user_agent: Some("UA \u{4e2d}".repeat(100)),
    };
    let id1 = st.session_create(&mk('a', u.id, 100)).await.unwrap();
    st.session_create(&mk('b', u.id, 200)).await.unwrap();
    st.session_create(&mk('c', v.id, 300)).await.unwrap();
    assert!(matches!(
        st.session_create(&mk('a', v.id, 1)).await,
        Err(Error::Duplicate(_))
    ));
    let s = st.session_get(&"a".repeat(64)).await.unwrap().unwrap();
    assert_eq!(
        (s.id, s.user_id, s.ip.as_deref()),
        (id1, u.id, Some("203.0.113.9"))
    );
    assert_eq!(
        (s.created_at, s.last_seen_at, s.expires_at),
        (t(T0), t(T0), t(T0 + 100_000))
    );
    assert!(
        s.user_agent.unwrap().chars().count() <= 120,
        "a long user agent is cut"
    );
    st.session_touch(id1, t(T0 + 7_000)).await.unwrap();
    assert_eq!(
        st.session_get(&"a".repeat(64))
            .await
            .unwrap()
            .unwrap()
            .last_seen_at,
        t(T0 + 7_000)
    );
    assert_eq!(st.session_count_for_user(u.id).await.unwrap(), 2);
    // expired sessions go (strictly before now)
    assert_eq!(
        st.sessions_delete_expired(t(T0 + 100_000)).await.unwrap(),
        0
    );
    assert_eq!(
        st.sessions_delete_expired(t(T0 + 100_001)).await.unwrap(),
        1
    );
    assert!(st.session_get(&"a".repeat(64)).await.unwrap().is_none());
    assert!(st.session_delete(&"b".repeat(64)).await.unwrap());
    assert!(!st.session_delete(&"b".repeat(64)).await.unwrap());
    st.session_create(&mk('d', u.id, 100)).await.unwrap();
    st.session_create(&mk('e', u.id, 100)).await.unwrap();
    assert_eq!(st.sessions_delete_for_user(u.id).await.unwrap(), 2);
    assert_eq!(st.session_count_for_user(u.id).await.unwrap(), 0);
    assert_eq!(
        st.session_count_for_user(v.id).await.unwrap(),
        1,
        "the other user is untouched"
    );
}

// ---------------------------------------------------------------- redeliver (M14 store part)
db_case!(m14_redeliver, m14_redeliver_body);
async fn m14_redeliver_body(env: &Env) {
    let st = &env.store;
    let id = st
        .upsert_finished(
            &nf("call-m14", "t", T0),
            &ff(T0 + 10_000, FaxStatus::Received, 1),
        )
        .await
        .unwrap()
        .0;
    let add = |target: &str, attempt: i32, status: DeliveryStatus| {
        let st = st.clone();
        let target = target.to_string();
        async move {
            st.add_delivery(&NewDelivery {
                fax_id: id,
                target,
                kind: "folder".into(),
                attempt,
                status,
                next_try_at: None,
            })
            .await
            .unwrap()
        }
    };
    // target A: failed after attempt 2 (latest). target B: done. target C: pending. D: giving up.
    add("folder:/a", 1, DeliveryStatus::Failed).await;
    add("folder:/a", 2, DeliveryStatus::Failed).await;
    add("folder:/b", 1, DeliveryStatus::Done).await;
    add("folder:/c", 1, DeliveryStatus::Pending).await;
    add("folder:/d", 3, DeliveryStatus::GivingUp).await;
    // target E: failed, then a newer pending attempt exists: not re-queued
    add("folder:/e", 1, DeliveryStatus::Failed).await;
    add("folder:/e", 2, DeliveryStatus::Pending).await;
    let at = t(T0 + 99_000);
    let created = st.redeliver(id, at).await.unwrap();
    assert_eq!(created.len(), 2, "only A and D");
    let all = st.list_deliveries(id).await.unwrap();
    assert_eq!(all.len(), 9);
    let new: Vec<_> = all.iter().filter(|d| created.contains(&d.id)).collect();
    for d in &new {
        assert_eq!(
            (d.status, d.next_try_at),
            (DeliveryStatus::Pending, Some(at))
        );
    }
    let a = new.iter().find(|d| d.target == "folder:/a").unwrap();
    let d = new.iter().find(|d| d.target == "folder:/d").unwrap();
    assert_eq!((a.attempt, d.attempt), (3, 4));
    // asking again finds nothing (the new rows are pending)
    assert!(st.redeliver(id, at).await.unwrap().is_empty());
    assert!(st.redeliver(424_242, at).await.unwrap().is_empty());
    let due = st
        .due_deliveries(at + Duration::seconds(1), 50)
        .await
        .unwrap();
    assert!(due.iter().any(|x| x.id == a.id));
}
