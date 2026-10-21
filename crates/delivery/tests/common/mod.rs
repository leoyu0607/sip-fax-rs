//! Shared harness: a real SQLite store with a manual clock, fax files on disk, and real servers
//! listening on localhost (HTTP via axum, a scriptable SMTP server). Nothing here calls the code
//! under test.
#![allow(dead_code)]

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use chrono::{DateTime, Duration, TimeZone, Utc};
use delivery::{DeliveryEngine, EngineOptions, Routes};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use store::{
    ConnectOptions, FaxFinal, FaxRecord, FaxStatus, FaxStore, HangupDirection, ManualClock, NewFax,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// 2026-10-21T00:00:00Z
pub const T0: i64 = 1_792_540_800_000;

pub fn t(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(ms).single().unwrap()
}

pub struct Env {
    pub dir: tempfile::TempDir,
    pub data: PathBuf,
    pub db_url: String,
    pub store: Arc<dyn FaxStore>,
    pub clock: Arc<ManualClock>,
    seq: i64,
}

impl Env {
    pub async fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(data.join("faxes")).unwrap();
        let db_url = format!("sqlite://{}", dir.path().join("fax.db").display());
        let clock = Arc::new(ManualClock::at_millis(T0));
        let store = Self::open(&db_url, clock.clone()).await;
        Env {
            dir,
            data,
            db_url,
            store,
            clock,
            seq: 0,
        }
    }

    async fn open(url: &str, clock: Arc<ManualClock>) -> Arc<dyn FaxStore> {
        store::connect_with(
            url,
            ConnectOptions {
                max_connections: 4,
                auto_migrate: true,
                clock,
            },
        )
        .await
        .unwrap()
    }

    /// Close the database and open it again (a process restart as far as the queue can tell).
    pub async fn restart_store(&mut self) {
        self.store.close().await;
        self.store = Self::open(&self.db_url, self.clock.clone()).await;
    }

    pub fn engine(&self, routes: &Routes) -> DeliveryEngine {
        self.engine_with(routes, EngineOptions::default())
    }

    pub fn engine_with(&self, routes: &Routes, opts: EngineOptions) -> DeliveryEngine {
        DeliveryEngine::new(
            self.store.clone(),
            routes,
            &self.data,
            self.clock.clone(),
            opts,
        )
        .unwrap()
    }

    /// Insert and finalize a fax with real files on disk. Returns its id.
    pub async fn fax(&mut self, trunk: &str, dnis: &str, status: FaxStatus) -> i64 {
        self.seq += 1;
        let call_id = format!("call-{}-{}", self.seq, dnis);
        let id = self
            .store
            .insert_receiving(&NewFax {
                call_id,
                trunk: trunk.into(),
                ani: Some("0227001234".into()),
                ani_source: Some("from".into()),
                dnis: Some(dnis.into()),
                dnis_source: Some("ruri".into()),
                started_at: self.clock.now_utc(),
                answered_at: None,
            })
            .await
            .unwrap();
        let pdf = format!("faxes/{id}.pdf");
        let tif = format!("faxes/{id}.tif");
        std::fs::write(self.data.join(&pdf), pdf_bytes(id)).unwrap();
        std::fs::write(self.data.join(&tif), tiff_bytes(id)).unwrap();
        let ended = self.clock.now_utc() + Duration::seconds(60);
        let (code, status, pages) = match status {
            FaxStatus::Received => ("completed", status, 2),
            FaxStatus::Partial => ("peer_hangup", status, 1),
            FaxStatus::Failed => ("t30_failed", status, 0),
            _ => ("no_fax_signal", status, 0),
        };
        self.store
            .finalize(
                id,
                &FaxFinal {
                    ended_at: ended,
                    answered_at: None,
                    duration_sec: Some(60),
                    hangup_direction: HangupDirection::Remote,
                    hangup_code: code.into(),
                    hangup_text: Some("text".into()),
                    sip_status: None,
                    t30_code: Some(0),
                    fax_status: status,
                    pages,
                    remote_ident: Some("+886 2 2700 1234".into()),
                    resolution: Some("fine".into()),
                    bit_rate: Some(14400),
                    compression: Some("MMR".into()),
                    ecm: true,
                    tiff_path: Some(tif),
                    tiff_size: Some(tiff_bytes(id).len() as i64),
                    pdf_path: Some(pdf),
                    pdf_size: Some(pdf_bytes(id).len() as i64),
                },
            )
            .await
            .unwrap();
        id
    }

    pub async fn record(&self, id: i64) -> FaxRecord {
        self.store.get(id).await.unwrap().unwrap()
    }

    pub async fn attempts(&self, id: i64) -> Vec<store::DeliveryAttempt> {
        self.store.list_deliveries(id).await.unwrap()
    }
}

pub trait ClockExt {
    fn now_utc(&self) -> DateTime<Utc>;
}
impl ClockExt for ManualClock {
    fn now_utc(&self) -> DateTime<Utc> {
        store::Clock::now(self)
    }
}

/// Binary content that exercises every byte value and CRLF sequences.
pub fn pdf_bytes(id: i64) -> Vec<u8> {
    let mut v = b"%PDF-1.4\r\n".to_vec();
    v.extend((0..=255u8).cycle().take(6000));
    v.extend(format!("\r\n.\r\n%%EOF {id}\r\n").as_bytes());
    v
}

pub fn tiff_bytes(id: i64) -> Vec<u8> {
    let mut v = b"II*\0".to_vec();
    v.extend((0..=255u8).rev().cycle().take(3000));
    v.extend(id.to_le_bytes());
    v
}

pub fn routes(toml_src: &str) -> Routes {
    let r: Routes = toml::from_str(toml_src).unwrap();
    r.validate().unwrap();
    r
}

// ---------------------------------------------------------------------------- HTTP test server

#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    pub path: String,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Req {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

#[derive(Clone)]
pub struct Hook {
    pub addr: SocketAddr,
    pub reqs: Arc<Mutex<Vec<Req>>>,
    script: Arc<Mutex<VecDeque<u16>>>,
    default: Arc<Mutex<u16>>,
}

impl Hook {
    /// Start a server answering `default` (changed with [`Hook::set_status`]); queued statuses
    /// from [`Hook::script`] are used first.
    pub async fn start(default: u16) -> Hook {
        Self::start_on(0, default).await
    }

    /// Like [`Hook::start`] on a given local port (0 = any).
    pub async fn start_on(port: u16, default: u16) -> Hook {
        let reqs = Arc::new(Mutex::new(Vec::new()));
        let script = Arc::new(Mutex::new(VecDeque::new()));
        let default = Arc::new(Mutex::new(default));
        let (r2, s2, d2) = (reqs.clone(), script.clone(), default.clone());
        let app = axum::Router::new().fallback(move |req: Request<Body>| {
            let (r, s, d) = (r2.clone(), s2.clone(), d2.clone());
            async move {
                let (parts, body) = req.into_parts();
                let body = axum::body::to_bytes(body, 64 * 1024 * 1024).await.unwrap();
                r.lock().unwrap().push(Req {
                    method: parts.method.to_string(),
                    path: parts.uri.path().to_string(),
                    headers: parts.headers,
                    body: body.to_vec(),
                });
                let code = s.lock().unwrap().pop_front().unwrap_or(*d.lock().unwrap());
                StatusCode::from_u16(code).unwrap()
            }
        });
        let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Hook {
            addr,
            reqs,
            script,
            default,
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}/hook", self.addr)
    }
    pub fn script(&self, codes: &[u16]) {
        self.script.lock().unwrap().extend(codes.iter().copied());
    }
    pub fn set_status(&self, code: u16) {
        *self.default.lock().unwrap() = code;
    }
    pub fn count(&self) -> usize {
        self.reqs.lock().unwrap().len()
    }
    pub fn requests(&self) -> Vec<Req> {
        self.reqs.lock().unwrap().clone()
    }
}

/// A local port nothing listens on (connection refused).
pub async fn closed_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

// ---------------------------------------------------------------------------- scripted SMTP

/// Replies of the scripted SMTP server.
#[derive(Clone)]
pub struct SmtpScript {
    pub rcpt: &'static str,
    pub data_end: &'static str,
    pub auth: &'static str,
    pub advertise_auth: bool,
}

impl Default for SmtpScript {
    fn default() -> Self {
        Self {
            rcpt: "250 ok",
            data_end: "250 queued",
            auth: "235 authenticated",
            advertise_auth: false,
        }
    }
}

/// Minimal SMTP server on localhost. Records the raw DATA of accepted messages.
pub struct FakeSmtp {
    pub port: u16,
    pub messages: Arc<Mutex<Vec<Vec<u8>>>>,
    pub commands: Arc<Mutex<Vec<String>>>,
}

impl FakeSmtp {
    pub async fn start(script: SmtpScript) -> FakeSmtp {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let messages = Arc::new(Mutex::new(Vec::new()));
        let commands = Arc::new(Mutex::new(Vec::new()));
        let (m2, c2) = (messages.clone(), commands.clone());
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                let (m, c, s) = (m2.clone(), c2.clone(), script.clone());
                tokio::spawn(async move {
                    let _ = serve_smtp(sock, s, m, c).await;
                });
            }
        });
        FakeSmtp {
            port,
            messages,
            commands,
        }
    }
}

async fn serve_smtp(
    sock: tokio::net::TcpStream,
    s: SmtpScript,
    messages: Arc<Mutex<Vec<Vec<u8>>>>,
    commands: Arc<Mutex<Vec<String>>>,
) -> std::io::Result<()> {
    let (rd, mut wr) = sock.into_split();
    let mut rd = BufReader::new(rd);
    wr.write_all(b"220 fake ESMTP\r\n").await?;
    let mut line = String::new();
    loop {
        line.clear();
        if rd.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let cmd = line.trim_end().to_string();
        commands.lock().unwrap().push(cmd.clone());
        let up = cmd.to_ascii_uppercase();
        if up.starts_with("EHLO") {
            let mut r = String::from("250-fake\r\n");
            if s.advertise_auth {
                r.push_str("250-AUTH PLAIN LOGIN\r\n");
            }
            r.push_str("250 8BITMIME\r\n");
            wr.write_all(r.as_bytes()).await?;
        } else if up.starts_with("AUTH") {
            // AUTH PLAIN <base64> in one line (lettre sends it that way).
            wr.write_all(format!("{}\r\n", s.auth).as_bytes()).await?;
        } else if up.starts_with("MAIL FROM") {
            wr.write_all(b"250 ok\r\n").await?;
        } else if up.starts_with("RCPT TO") {
            wr.write_all(format!("{}\r\n", s.rcpt).as_bytes()).await?;
        } else if up == "DATA" {
            wr.write_all(b"354 go\r\n").await?;
            let mut data = Vec::new();
            let mut buf = [0u8; 4096];
            while !data.ends_with(b"\r\n.\r\n") {
                let n = rd.read(&mut buf).await?;
                if n == 0 {
                    return Ok(());
                }
                data.extend_from_slice(&buf[..n]);
            }
            if s.data_end.starts_with('2') {
                messages.lock().unwrap().push(data);
            }
            wr.write_all(format!("{}\r\n", s.data_end).as_bytes())
                .await?;
        } else if up == "QUIT" {
            wr.write_all(b"221 bye\r\n").await?;
            return Ok(());
        } else {
            wr.write_all(b"250 ok\r\n").await?;
        }
    }
}
