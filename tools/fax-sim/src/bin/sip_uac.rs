//! `sip-uac --target 127.0.0.1:5060 --mode gateway|server|initial [--transport udp|tcp]
//!          [--fax FILE.tif [--redundancy N] [--loss PERMILLE] [--seed N] [--version 0..3]]`
//!
//! Places one simulated gateway call into a SIP fax server (see `fax_sim::sip_uac`) and prints
//! what the server answered. With `--fax` the gateway really sends the TIFF over T.38
//! (SpanDSP `t38_terminal` on a UDP socket). Exit code 0 when the call went through to the BYE
//! (and, with `--fax`, SpanDSP reported success).

use fax_sim::gateway::FaxTx;
use fax_sim::sip_uac::{ReinviteBy, UacConfig, run_call};
use sip_core::transport::TransportKind;
use t38::ErrorRecovery;

fn usage() -> ! {
    eprintln!(
        "usage: sip-uac --target ip:port --mode gateway|server|initial [--transport udp|tcp] \
         [--fax FILE.tif [--redundancy N] [--loss PERMILLE] [--seed N] [--version 0..3]]"
    );
    std::process::exit(2);
}

#[tokio::main]
async fn main() {
    let mut target = None;
    let mut mode = ReinviteBy::Gateway;
    let mut transport = TransportKind::Udp;
    let mut fax: Option<FaxTx> = None;
    let (mut redundancy, mut loss, mut seed, mut version) = (3u8, 0u32, 1u64, 0u8);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match (a.as_str(), args.next()) {
            ("--target", Some(v)) => target = v.parse().ok(),
            ("--mode", Some(v)) => {
                mode = match v.as_str() {
                    "gateway" => ReinviteBy::Gateway,
                    "server" => ReinviteBy::Server,
                    "initial" => ReinviteBy::InitialInvite,
                    _ => usage(),
                }
            }
            ("--transport", Some(v)) => {
                transport = match v.as_str() {
                    "udp" => TransportKind::Udp,
                    "tcp" => TransportKind::Tcp,
                    _ => usage(),
                }
            }
            ("--fax", Some(v)) => fax = Some(FaxTx::new(v.into())),
            ("--redundancy", Some(v)) => redundancy = v.parse().unwrap_or_else(|_| usage()),
            ("--loss", Some(v)) => loss = v.parse().unwrap_or_else(|_| usage()),
            ("--seed", Some(v)) => seed = v.parse().unwrap_or_else(|_| usage()),
            ("--version", Some(v)) => version = v.parse().unwrap_or_else(|_| usage()),
            _ => usage(),
        }
    }
    let Some(target) = target else { usage() };
    let mut cfg = UacConfig::new(target, mode);
    cfg.transport = transport;
    if let Some(mut f) = fax {
        f.recovery = ErrorRecovery::Redundancy { depth: redundancy };
        f.loss_permille = loss;
        f.seed = seed;
        f.version = version;
        cfg.t38_version = version;
        cfg.fax = Some(f);
    }
    match run_call(cfg).await {
        Ok(r) => {
            println!(
                "call {} ok, BYE {} (server hung up first: {})",
                r.call_id, r.bye_status, r.peer_bye
            );
            if let Some(f) = &r.fax {
                println!(
                    "fax: SpanDSP {:?} ({}), {} ms, tx {} (dropped {}), rx {} (dropped {}), gaps {}",
                    f.caller_code,
                    f.caller_text,
                    f.wall_ms,
                    f.tx_datagrams,
                    f.tx_dropped,
                    f.rx_datagrams,
                    f.rx_dropped,
                    f.rx_gaps
                );
                if f.caller_code != Some(0) {
                    std::process::exit(1);
                }
            }
            if let Some(a) = r.initial_answer {
                println!("--- G.711 answer ---\n{a}");
            }
            if let Some(o) = r.server_t38_offer {
                println!("--- server T.38 offer ---\n{o}");
            }
            if let Some(a) = r.server_t38_answer {
                println!("--- server T.38 answer ---\n{a}");
            }
        }
        Err(e) => {
            eprintln!("sip-uac: {e}");
            std::process::exit(1);
        }
    }
}
