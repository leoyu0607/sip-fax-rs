//! K07 at the daemon level: a change made through the [`ConfigService`] reaches the running
//! fax-in controller and the delivery service, on a real SQLite store and real sockets.
use super::*;
use crate::delivery_service::DeliveryService;
use fax_in::StartOptions;
use serde_json::json;

const TEXT: &str = "[sip]\nbind = \"127.0.0.1:0\"\n[capture]\nenabled = false\n\
        [call]\nmax_duration_secs = 600\n\
        [[trunks]]\nname = \"local\"\nallowed_sources = [\"127.0.0.0/8\"]\n";

#[tokio::test(flavor = "multi_thread")]
async fn k07_changes_through_the_service_reach_fax_in_and_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let slash = |p: std::path::PathBuf| p.display().to_string().replace('\\', "/");
    let text = format!(
        "[storage]\ndata_dir = {:?}\n[database]\nurl = {:?}\n{TEXT}",
        slash(dir.path().join("data")),
        format!("sqlite://{}", slash(dir.path().join("fax.db"))),
    );
    let cfg = AppConfig::parse(&text, &dir.path().join("sip-faxd.toml")).unwrap();
    let db = Arc::new(Db::new(&cfg, dir.path()));
    assert!(matches!(
        db.start().await.unwrap(),
        crate::db::Startup::Ready { .. }
    ));
    let rt = Runtime::start(&cfg, db.clone(), true).await;
    // the first administrator exists, with the password in a private file
    let users = rt.auth.list_users().await.unwrap();
    assert_eq!(users.len(), 1);
    assert!(users[0].must_change_password);
    assert!(dir.path().join("initial-admin-password").exists());
    assert!(dir.path().join("secret.key").exists());

    let live = rt.current();
    let delivery = DeliveryService::start_default(
        db.clone(),
        live.routes.clone(),
        dir.path().join("data"),
        live.delivery.clone(),
    );
    let (fax, _events) = FaxIn::start(live.to_fax_in(&cfg.fax_in.storage), StartOptions::default())
        .await
        .unwrap();
    let fax = Arc::new(fax);
    rt.attach(fax.clone(), delivery.reconfigurer());

    // a call setting: applied to the next call, nothing needs a restart
    let call = rt.config.get_section("call").await.unwrap();
    let mut body = call.body.clone();
    body["max_duration_secs"] = json!(77);
    let out = rt
        .config
        .update("call", body, call.version, "admin")
        .await
        .unwrap();
    assert!(out.restart_required.is_empty());
    assert_eq!(fax.snapshot().cfg.call.max_duration_secs, 77);
    assert!(out.applied.trunks_changed.is_empty());

    // a trunk: the controller's trunk table follows
    let t = rt.config.get_section("trunk:local").await.unwrap();
    let mut body = t.body.clone();
    body["numbers"]["strip_plus"] = json!(true);
    let out = rt
        .config
        .update("trunk:local", body, t.version, "admin")
        .await
        .unwrap();
    assert_eq!(out.applied.trunks_changed, ["local"]);
    assert!(fax.snapshot().table.trunks()[0].cfg.numbers.strip_plus);

    // a setting that needs a restart is stored, reported, and not applied to the sockets
    let sip = rt.config.get_section("sip").await.unwrap();
    let mut body = sip.body.clone();
    body["bind"] = json!("127.0.0.1:5999");
    let out = rt
        .config
        .update("sip", body, sip.version, "admin")
        .await
        .unwrap();
    assert_eq!(out.restart_required, ["sip.bind"]);
    assert_eq!(fax.pending_restart(), ["sip.bind"]);
    assert_ne!(fax.udp_addr().unwrap().port(), 5999);

    // routes: the delivery service takes them (a broken set never gets this far)
    let routes = rt.config.get_section("routes").await.unwrap();
    let body = json!({"routes": [{"dnis": "1", "deliveries": [{"kind": "folder", "dir": slash(dir.path().join("out"))}]}]});
    let out = rt
        .config
        .update("routes", body, routes.version, "admin")
        .await
        .unwrap();
    assert!(out.applied.notes.is_empty(), "{:?}", out.applied.notes);

    rt.detach();
    let fax = Arc::try_unwrap(fax)
        .ok()
        .expect("nothing else holds the controller");
    fax.shutdown().await;
    delivery.shutdown().await;
    db.close().await;
}

#[test]
fn m14_the_operator_is_cli_and_the_os_user() {
    let who = cli_operator();
    assert!(who.starts_with("cli:") && who.len() > 4, "{who}");
}
