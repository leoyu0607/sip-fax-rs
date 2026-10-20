//! fax-sim SIP UAC mode against the real `sip-faxd serve` (M1d, item 4): one test per
//! direction of the T.38 switch. The UAC is sip-core's; the server is the shipped binary
//! started with a generated configuration (see the `interop` crate).

use std::time::Duration;

use fax_sim::sip_uac::{ReinviteBy, UacConfig, run_call};
use interop::{Faxd, FaxdOptions};
use sip_core::transport::TransportKind;

fn media_summary(sdp: &sip_core::sdp::SessionDescription) -> Vec<String> {
    sdp.media
        .iter()
        .map(|m| format!("{} {} {} {}", m.media, m.port, m.proto, m.formats.join(" ")))
        .collect()
}

fn attr<'a>(m: &'a sip_core::sdp::MediaDescription, n: &str) -> Option<&'a str> {
    m.attribute(n).and_then(|a| a.value.as_deref())
}

/// Gateway-initiated: the simulated gateway sends the T.38 re-INVITE after the G.711 call is up.
#[tokio::test]
async fn uac_gateway_initiated_t38_reinvite_against_sip_faxd() {
    for transport in [TransportKind::Udp, TransportKind::Tcp] {
        let faxd = Faxd::start("uac-gateway", &FaxdOptions::default());
        let addr = faxd.addr().parse().unwrap();
        let mut cfg = UacConfig::new(addr, ReinviteBy::Gateway);
        cfg.transport = transport;
        let r = run_call(cfg)
            .await
            .unwrap_or_else(|e| panic!("{e}\n{}", faxd.log().join("\n")));
        assert_eq!(r.bye_status, 200);
        let g711 = r.initial_answer.expect("G.711 answer");
        assert_eq!(media_summary(&g711), vec!["audio 20000 RTP/AVP 0 8 101"]);
        let t38 = r.server_t38_answer.expect("T.38 answer");
        assert_eq!(media_summary(&t38), vec!["image 30000 udptl t38"]);
        let m = &t38.media[0];
        assert_eq!(attr(m, "T38FaxVersion"), Some("0"));
        assert_eq!(attr(m, "T38MaxBitRate"), Some("14400"));
        assert_eq!(attr(m, "T38FaxRateManagement"), Some("transferredTCF"));
        assert_eq!(attr(m, "T38FaxUdpEC"), Some("t38UDPRedundancy"));
        // o= session version moved on between the audio answer and the T.38 answer
        assert_eq!(t38.origin.sess_id, g711.origin.sess_id);
        assert_eq!(t38.origin.sess_version, g711.origin.sess_version + 1);

        let res = faxd.wait_results(1);
        assert_eq!(res[0]["call_id"], r.call_id.as_str());
        assert_eq!(res[0]["trunk"], "sipp");
        assert_eq!(res[0]["outcome"]["status"], "completed");
        assert_eq!(res[0]["t38"]["path"], "peer_reinvite");
        assert_eq!(res[0]["t38"]["peer_media"], "127.0.0.1:5004");
        faxd.stop();
    }
}

/// Server-initiated: the simulated gateway waits; sip-faxd sends its own T.38 re-INVITE
/// `initiate_after_secs` after the ACK.
#[tokio::test]
async fn uac_waits_for_the_server_t38_reinvite_against_sip_faxd() {
    for transport in [TransportKind::Udp, TransportKind::Tcp] {
        let opts = FaxdOptions {
            trunk: "initiate_after_secs = 0.3\n".into(),
            ..FaxdOptions::default()
        };
        let faxd = Faxd::start("uac-server", &opts);
        let mut cfg = UacConfig::new(faxd.addr().parse().unwrap(), ReinviteBy::Server);
        cfg.transport = transport;
        cfg.wait_for_server = Duration::from_secs(10);
        let started = std::time::Instant::now();
        let r = run_call(cfg)
            .await
            .unwrap_or_else(|e| panic!("{e}\n{}", faxd.log().join("\n")));
        assert!(
            started.elapsed() >= Duration::from_millis(300),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(r.bye_status, 200);
        let offer = r.server_t38_offer.expect("server T.38 offer");
        assert_eq!(media_summary(&offer), vec!["image 30000 udptl t38"]);
        let m = &offer.media[0];
        assert_eq!(attr(m, "T38FaxVersion"), Some("0"));
        assert_eq!(attr(m, "T38MaxBitRate"), Some("14400"));
        assert_eq!(attr(m, "T38FaxMaxDatagram"), Some("260"));
        assert_eq!(attr(m, "T38FaxUdpEC"), Some("t38UDPRedundancy"));

        let res = faxd.wait_results(1);
        assert_eq!(res[0]["outcome"]["status"], "completed");
        assert_eq!(res[0]["t38"]["path"], "our_reinvite");
        faxd.stop();
    }
}
