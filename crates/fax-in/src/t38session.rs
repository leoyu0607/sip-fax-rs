//! The real fax session: a UDPTL socket, the sans-IO [`UdptlEngine`] driven on a real-time
//! clock, and the output files (M2d).
//!
//! * The socket is bound on the call's port from the port pool. We start by sending to the
//!   address in the peer's SDP; the first datagram that parses as UDPTL *latches* the peer's
//!   real address (NAT, gateways that send from another port) and datagrams from any other
//!   source are ignored from then on.
//! * The engine is told what time it is (milliseconds since the session started) whenever a
//!   datagram arrives or `next_wakeup_ms` passes; everything it releases is sent at once.
//! * The session ends when T.30 is done, the call is cancelled underneath it (peer BYE,
//!   shutdown) or `max_duration_secs` pass. Pages received so far are written in every case;
//!   a failed call with pages is flagged `partial`.
//! * Output: `<data_dir>/faxes/<YYYY>/<MM>/<DD>/<call-id>.tif` (multi-page G4) and `.pdf`.
//!   Files are written under a temporary name and renamed, so a reader never sees half a file.

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use t30::{Modems, ResultCode, TcfMode};
use t38::{ErrorRecovery, ReceiverConfig, T38Version};
use tokio::net::UdpSocket;
use tokio::time::Instant;

use crate::capture::{Capture, dir_name};
use crate::event::{FaxMedia, FaxReport, ReportSink, RtpCounters, UdptlCounters};
use crate::session::{
    AudioContext, AudioFaxSession, FaxSession, FaxSessionFactory, SessionContext, SessionOutcome,
};
use crate::t38engine::{EngineConfig, Pacing, ReceivedPage, UdptlEngine};
use crate::timefmt;

/// Largest datagram we read.
const MAX_DATAGRAM: usize = 4096;
/// Longest file name stem taken from a Call-ID.
const FILE_STEM_MAX: usize = 100;

/// Everything the sessions of one server share.
#[derive(Clone)]
pub struct T38SessionEnv {
    /// Resolved data directory (`<data_dir>/faxes/...` is created below it).
    pub data_dir: PathBuf,
    /// Capture writer for `udptl.log` (`None` when capture is off).
    pub capture: Option<Arc<Capture>>,
    /// Address the UDPTL sockets bind to (wildcard of the SIP bind's family when unspecified).
    pub bind_ip: IpAddr,
    /// Our station id (CSI).
    pub station_id: String,
    /// Longest a session runs.
    pub max_duration: Duration,
    /// Transmit pacing of the engine (SpanDSP's timing unless a test turns it off).
    pub pacing: Pacing,
}

/// Creates [`T38FaxSession`]s and, for calls on audio, [`crate::audiosession`] sessions (M5).
#[derive(Clone)]
pub struct T38SessionFactory {
    env: Arc<T38SessionEnv>,
}

impl T38SessionFactory {
    /// A factory for the given environment.
    pub fn new(env: T38SessionEnv) -> Self {
        T38SessionFactory { env: Arc::new(env) }
    }
}

impl FaxSessionFactory for T38SessionFactory {
    fn create(&self, _ctx: &SessionContext) -> Box<dyn FaxSession> {
        Box::new(T38FaxSession {
            env: self.env.clone(),
        })
    }

    fn receives_audio(&self) -> bool {
        true
    }

    fn create_audio(&self, _ctx: &AudioContext) -> Option<Box<dyn AudioFaxSession>> {
        Some(Box::new(crate::audiosession::G711FaxSession {
            env: self.env.clone(),
        }))
    }
}

/// A fax session receiving over T.38.
pub struct T38FaxSession {
    env: Arc<T38SessionEnv>,
}

impl FaxSession for T38FaxSession {
    fn run(
        self: Box<Self>,
        ctx: SessionContext,
    ) -> Pin<Box<dyn Future<Output = SessionOutcome> + Send>> {
        Box::pin(async move { run_session(&self.env, ctx).await })
    }
}

/// `ip:port` or `[v6]:port` of the peer's SDP.
pub(crate) fn parse_peer(text: Option<&str>) -> Option<SocketAddr> {
    let a: SocketAddr = text?.parse().ok()?;
    (a.port() != 0).then_some(a)
}

/// The UDPTL error recovery we send for the scheme in force.
pub fn recovery_for(udp_ec: &str, s: &crate::session::UdptlSettings) -> ErrorRecovery {
    match udp_ec {
        "t38UDPFEC" => ErrorRecovery::Fec {
            span: s.fec_span,
            entries: s.fec_entries,
        },
        "t38UDPNoEC" => ErrorRecovery::None,
        _ => ErrorRecovery::Redundancy {
            depth: s.redundancy_depth,
        },
    }
}

/// The T.30 modem set for `T38MaxBitRate`.
fn modems_for(max_bit_rate: u32) -> Modems {
    Modems {
        v27ter: true,
        v29: max_bit_rate >= 7200,
        v17: max_bit_rate >= 7200,
    }
}

fn engine_config(env: &T38SessionEnv, ctx: &SessionContext) -> EngineConfig {
    let mut cfg = EngineConfig {
        version: T38Version::from_number(u32::from(ctx.t38.version)).unwrap_or(T38Version::V0),
        pacing: env.pacing,
        ..EngineConfig::default()
    };
    cfg.t30.local_id = ctx
        .station_id
        .clone()
        .unwrap_or_else(|| env.station_id.clone());
    cfg.t30.tcf = if ctx.t38.rate_management == "localTCF" {
        TcfMode::Local
    } else {
        TcfMode::Transferred
    };
    cfg.t30.modems = modems_for(ctx.t38.max_bit_rate);
    cfg.t30.fine = ctx.fax.fine();
    // MMR (T.6) is only valid together with ECM (T.30 Table 2 notes 9 and 17).
    cfg.t30.ecm = ctx.fax.ecm;
    cfg.t30.mmr = ctx.fax.ecm;
    cfg
}

/// How the session loop ended.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum End {
    Done,
    Cancelled,
    MaxDuration,
}

struct Sink<'a> {
    capture: Option<&'a Arc<Capture>>,
    call_id: &'a str,
    version: T38Version,
    /// HDLC octets of the frame in progress, per direction (RX, TX), for the summary log.
    hdlc: [std::sync::Mutex<Vec<u8>>; 2],
}

impl Sink<'_> {
    fn log(&self, transmit: bool, datagram: &[u8]) {
        if let Some(c) = self.capture {
            let mut buf = self.hdlc[usize::from(transmit)]
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            c.udptl(
                self.call_id,
                SystemTime::now(),
                transmit,
                datagram,
                self.version,
                &mut buf,
            );
        }
    }
}

async fn run_session(env: &T38SessionEnv, ctx: SessionContext) -> SessionOutcome {
    let started_at = SystemTime::now();
    let bind = SocketAddr::new(env.bind_ip, ctx.t38.local_port);
    let sock = match UdpSocket::bind(bind).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(call_id = %ctx.call_id, %bind, "cannot bind the UDPTL port: {e}");
            return SessionOutcome::Failed(format!("cannot bind UDPTL port {bind}: {e}"));
        }
    };
    let mut remote = parse_peer(ctx.t38.peer_media.as_deref());
    let Some(first_remote) = remote else {
        return SessionOutcome::Failed("the peer's SDP has no usable media address".into());
    };
    tracing::info!(call_id = %ctx.call_id, local = %bind, remote = %first_remote,
        version = ctx.t38.version, ec = %ctx.t38.udp_ec, rate_management = %ctx.t38.rate_management,
        "T.38 session starts");

    let recovery = recovery_for(&ctx.t38.udp_ec, &ctx.udptl);
    let mut eng = UdptlEngine::new(
        engine_config(env, &ctx),
        recovery,
        ReceiverConfig::default(),
    );
    eng.set_max_datagram(
        usize::try_from(ctx.t38.peer_max_datagram)
            .unwrap_or(usize::MAX)
            .max(72),
    );
    let sink = Sink {
        capture: env.capture.as_ref(),
        call_id: &ctx.call_id,
        version: eng.engine().config().version,
        hdlc: Default::default(),
    };
    let mut counters = UdptlCounters::default();
    let mut latched = false;
    let mut cancel = ctx.cancel.clone();
    let t0 = Instant::now();
    let deadline = t0 + ctx.max_duration.unwrap_or(env.max_duration);
    let now_ms = |t0: Instant| u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut buf = vec![0u8; MAX_DATAGRAM];

    eng.start(0);
    let mut pages_reported = 0usize;
    let end = loop {
        send_due(&sock, &mut eng, remote, &sink, &mut counters).await;
        report_progress(&ctx, &eng, &mut pages_reported);
        if eng.is_done() {
            break End::Done;
        }
        let wake = eng.next_wakeup_ms().map_or_else(
            || deadline,
            |ms| (t0 + Duration::from_millis(ms)).min(deadline),
        );
        tokio::select! {
            r = sock.recv_from(&mut buf) => {
                let (n, src) = match r {
                    Ok(x) => x,
                    Err(e) => {
                        // e.g. ICMP port unreachable while the peer is not up yet
                        tracing::debug!(call_id = %ctx.call_id, "UDPTL receive error: {e}");
                        continue;
                    }
                };
                counters.rx_datagrams += 1;
                sink.log(false, &buf[..n]);
                if latched && Some(src) != remote {
                    counters.rx_foreign += 1;
                    continue;
                }
                match eng.on_datagram(now_ms(t0), &buf[..n]) {
                    Ok(()) => {
                        if !latched {
                            latched = true;
                            if Some(src) != remote {
                                tracing::info!(call_id = %ctx.call_id, sdp = %first_remote, %src,
                                    "UDPTL peer address latched (differs from the SDP)");
                            }
                            remote = Some(src);
                        }
                    }
                    Err(_) => counters.rx_malformed += 1,
                }
            }
            () = tokio::time::sleep_until(wake) => {
                if Instant::now() >= deadline {
                    break End::MaxDuration;
                }
                eng.advance(now_ms(t0));
            }
            () = crate::session::cancelled(&mut cancel) => break End::Cancelled,
        }
    };
    let mut state_at_cancel = None;
    if end != End::Done {
        state_at_cancel = Some(eng.engine().state());
        eng.abort(now_ms(t0));
        // a DCN/response already queued before the abort is not sent: nothing more is wanted
    }
    let stats = eng.receiver_stats();
    counters.rx_malformed += eng.malformed;
    counters.rx_duplicates = stats.duplicates;
    counters.recovered_redundancy = stats.recovered_redundancy;
    counters.recovered_fec = stats.recovered_fec;
    counters.lost = stats.lost;
    for line in eng.engine_mut().drain_log() {
        tracing::debug!(call_id = %ctx.call_id, "t30: {line}");
    }
    let result = eng.engine().result().cloned();
    let pages = eng.engine_mut().take_pages();
    drop(sock);
    conclude(
        env,
        Concluding {
            call_id: &ctx.call_id,
            report: &ctx.report,
            started_at,
            end,
            state_at_cancel,
            result,
            pages,
            media: FaxMedia::T38,
            udptl: counters,
            rtp: None,
        },
    )
    .await
}

/// What a session hands to [`conclude`] when its loop is over.
pub(crate) struct Concluding<'a> {
    pub call_id: &'a str,
    pub report: &'a ReportSink,
    pub started_at: SystemTime,
    pub end: End,
    /// The T.30 state when the loop was cut short (not `Done`).
    pub state_at_cancel: Option<t30::State>,
    pub result: Option<t30::T30Result>,
    pub pages: Vec<ReceivedPage>,
    pub media: FaxMedia,
    pub udptl: UdptlCounters,
    pub rtp: Option<RtpCounters>,
}

/// The end of a session, T.38 or G.711: writes the pages, leaves the report and returns the
/// outcome.
pub(crate) async fn conclude(env: &T38SessionEnv, c: Concluding<'_>) -> SessionOutcome {
    let Concluding {
        call_id,
        report: sink,
        started_at,
        end,
        state_at_cancel,
        mut result,
        pages,
        media,
        udptl,
        rtp,
    } = c;
    // The peer hung up while we waited for its DCN after the final MCF: every page was
    // confirmed, so that is a success.
    if end == End::Cancelled
        && state_at_cancel == Some(t30::State::WaitDcn)
        && let Some(r) = result.as_mut()
        && r.code == ResultCode::Aborted
        && r.pages > 0
    {
        r.code = ResultCode::Ok;
    }
    let Some(result) = result else {
        return SessionOutcome::Failed("the T.30 engine produced no result".into());
    };

    let mut report = FaxReport {
        code: result.code.as_u8(),
        code_name: result.code.name().into(),
        pages: result.pages,
        bit_rate: result.bit_rate,
        modem: result.modem.map(|m| format!("{m:?}").to_lowercase()),
        resolution: result.resolution.map(|m| format!("{m:?}").to_lowercase()),
        compression: result.compression.map(|m| format!("{m:?}").to_lowercase()),
        remote_ident: result.remote_ident.clone(),
        bad_lines_per_page: result.bad_lines_per_page.clone(),
        duration_ms: result.duration_ms,
        partial: result.is_partial(),
        tiff: None,
        pdf: None,
        store_error: None,
        udptl,
        media,
        rtp,
        tiff_size: None,
        pdf_size: None,
        ecm: result.ecm,
        ecm_ppr_sent: result.ecm_ppr_sent,
        ecm_frames_lost: result.ecm_frames_lost,
    };
    if !pages.is_empty() {
        match store_pages(
            &env.data_dir,
            call_id,
            started_at,
            &pages,
            result.remote_ident.as_deref(),
        )
        .await
        {
            Ok(stored) => {
                report.tiff = Some(stored.tiff.display().to_string());
                report.pdf = Some(stored.pdf.display().to_string());
                report.tiff_size = Some(stored.tiff_size);
                report.pdf_size = Some(stored.pdf_size);
            }
            Err(e) => {
                tracing::error!(call_id = %call_id, "cannot store the received fax: {e}");
                report.store_error = Some(e);
            }
        }
    }
    let label = match media {
        FaxMedia::T38 => "T.38",
        FaxMedia::G711 => "G.711",
    };
    tracing::info!(call_id = %call_id, code = report.code, code_name = %report.code_name,
        pages = report.pages, bit_rate = report.bit_rate, modem = ?report.modem,
        tsi = ?report.remote_ident, tiff = ?report.tiff, partial = report.partial,
        "{label} session ended");
    let outcome = match (end, result.code) {
        (End::MaxDuration, _) => SessionOutcome::MaxDuration,
        (_, ResultCode::Ok) if report.store_error.is_none() => {
            if end == End::Cancelled {
                SessionOutcome::Aborted
            } else {
                SessionOutcome::Completed
            }
        }
        (End::Cancelled, ResultCode::Aborted) => SessionOutcome::Aborted,
        (_, code) => SessionOutcome::Failed(match &report.store_error {
            Some(e) => format!("fax received but not stored: {e}"),
            None => format!(
                "T.30 {} ({}) after {} page(s)",
                code.name(),
                code.as_u8(),
                report.pages
            ),
        }),
    };
    *sink
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(report);
    outcome
}

/// Tells the controller about pages T.30 confirmed since the last call.
fn report_progress(ctx: &SessionContext, eng: &UdptlEngine, reported: &mut usize) {
    let n = eng.engine().pages().len();
    while *reported < n {
        *reported += 1;
        if let Some(tx) = &ctx.progress {
            let _ = tx.send(u32::try_from(*reported).unwrap_or(u32::MAX));
        }
    }
}

/// Sends what the engine released, to the current peer address.
async fn send_due(
    sock: &UdpSocket,
    eng: &mut UdptlEngine,
    remote: Option<SocketAddr>,
    sink: &Sink<'_>,
    counters: &mut UdptlCounters,
) {
    let Some(to) = remote else {
        return;
    };
    for d in eng.poll_datagrams() {
        match sock.send_to(&d.bytes, to).await {
            Ok(_) => {
                counters.tx_datagrams += 1;
                sink.log(true, &d.bytes);
            }
            Err(e) => tracing::debug!("UDPTL send to {to} failed: {e}"),
        }
    }
}

/// The file name stem for a Call-ID: filesystem-safe and, if anything had to be changed or
/// cut, suffixed with a hash of the original so that two Call-IDs never share a file.
pub fn file_stem(call_id: &str) -> String {
    let safe = dir_name(call_id);
    if safe == call_id && call_id.len() <= FILE_STEM_MAX {
        return safe;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in call_id.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{safe}-{:08x}", h >> 32)
}

/// The files of a received fax.
struct Stored {
    tiff: PathBuf,
    pdf: PathBuf,
    tiff_size: u64,
    pdf_size: u64,
}

/// Writes `<data_dir>/faxes/<YYYY>/<MM>/<DD>/<stem>.tif` and `.pdf`; returns both paths and sizes.
async fn store_pages(
    data_dir: &Path,
    call_id: &str,
    at: SystemTime,
    pages: &[ReceivedPage],
    tsi: Option<&str>,
) -> Result<Stored, String> {
    let (y, m, d) = timefmt::ymd(at);
    let dir = data_dir
        .join("faxes")
        .join(format!("{y:04}"))
        .join(format!("{m:02}"))
        .join(format!("{d:02}"));
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("create {}: {e}", dir.display()))?;
    let secs = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let datetime = t4::format_tiff_datetime(secs);
    let software = format!("sip-faxd {}", env!("CARGO_PKG_VERSION"));
    let desc = tsi.map(|t| format!("TSI: {t}"));
    let meta = t4::TiffMeta {
        software: &software,
        datetime: &datetime,
        image_description: desc.as_deref(),
    };
    let t4_pages: Vec<t4::Page<'_>> = pages.iter().map(ReceivedPage::as_t4_page).collect();
    let tiff = t4::write_tiff(&t4_pages, &meta).map_err(|e| format!("TIFF: {e}"))?;
    let pdf = t4::write_pdf(&t4_pages).map_err(|e| format!("PDF: {e}"))?;
    let stem = file_stem(call_id);
    // never overwrite another call's file
    let mut name = stem.clone();
    let mut n = 1;
    while tokio::fs::try_exists(dir.join(format!("{name}.tif")))
        .await
        .unwrap_or(false)
        || tokio::fs::try_exists(dir.join(format!("{name}.pdf")))
            .await
            .unwrap_or(false)
    {
        n += 1;
        name = format!("{stem}-{n}");
    }
    let tif_path = dir.join(format!("{name}.tif"));
    let pdf_path = dir.join(format!("{name}.pdf"));
    write_atomic(&tif_path, &tiff).await?;
    write_atomic(&pdf_path, &pdf).await?;
    let tiff_size = tiff.len() as u64;
    write_previews(&dir, &name, tiff).await;
    Ok(Stored {
        tiff: tif_path,
        pdf: pdf_path,
        tiff_size,
        pdf_size: pdf.len() as u64,
    })
}

/// The PNG previews next to the TIFF and the PDF (ADR-057): `<name>-p<n>.png` and
/// `<name>-p<n>-thumb.png` for every page, rendered from the TIFF just written. A failure is
/// logged and costs nothing else: the fax is stored, and the web interface converts a missing
/// page when it is first shown.
async fn write_previews(dir: &Path, name: &str, tiff: Vec<u8>) {
    let rendered = tokio::task::spawn_blocking(move || store::render_pages(&tiff)).await;
    let pages = match rendered {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            tracing::warn!(file = %name, "PNG previews not written: {e}");
            return;
        }
        Err(e) => {
            tracing::warn!(file = %name, "PNG previews not written: {e}");
            return;
        }
    };
    let tif = format!("{name}.tif");
    for (i, (full, thumb)) in pages.iter().enumerate() {
        let page = u32::try_from(i + 1).unwrap_or(u32::MAX);
        for (kind, data) in [(store::PngKind::Full, full), (store::PngKind::Thumb, thumb)] {
            let path = dir.join(store::png_rel_path(&tif, page, kind));
            if let Err(e) = write_atomic(&path, data).await {
                tracing::warn!("PNG preview not written: {e}");
            }
        }
    }
}

async fn write_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("part");
    tokio::fs::write(&tmp, data)
        .await
        .map_err(|e| format!("write {}: {e}", tmp.display()))?;
    tokio::fs::rename(&tmp, path)
        .await
        .map_err(|e| format!("rename to {}: {e}", path.display()))
}

#[cfg(test)]
mod tests;
