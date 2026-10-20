//! `sip-uac --target 127.0.0.1:5060 --mode gateway|server [--transport udp|tcp]`
//!
//! Places one simulated gateway call into a SIP fax server (see `fax_sim::sip_uac`) and prints
//! what the server answered. Exit code 0 when the call went through to the BYE.

use fax_sim::sip_uac::{ReinviteBy, UacConfig, run_call};
use sip_core::transport::TransportKind;

fn usage() -> ! {
    eprintln!("usage: sip-uac --target ip:port --mode gateway|server [--transport udp|tcp]");
    std::process::exit(2);
}

#[tokio::main]
async fn main() {
    let mut target = None;
    let mut mode = ReinviteBy::Gateway;
    let mut transport = TransportKind::Udp;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match (a.as_str(), args.next()) {
            ("--target", Some(v)) => target = v.parse().ok(),
            ("--mode", Some(v)) => {
                mode = match v.as_str() {
                    "gateway" => ReinviteBy::Gateway,
                    "server" => ReinviteBy::Server,
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
            _ => usage(),
        }
    }
    let Some(target) = target else { usage() };
    let mut cfg = UacConfig::new(target, mode);
    cfg.transport = transport;
    match run_call(cfg).await {
        Ok(r) => {
            println!("call {} ok, BYE {}", r.call_id, r.bye_status);
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
