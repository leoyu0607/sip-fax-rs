//! Per-call SIP capture (C11), built on the sip-core message tap.
//!
//! The tap callback runs inline on the transport's I/O tasks, so it only pushes the event into
//! an unbounded channel (bounded by a counter; events beyond the bound are dropped and
//! counted). A writer task sorts the messages by Call-ID and writes
//! `<dir>/<call-id>/sip.log` plus, when the call ends, `result.json`.
//!
//! Only Call-IDs the controller announces with [`Capture::begin`] get a directory: the INVITE
//! reaches the tap before the controller knows about the call, so unannounced messages wait
//! in a small bounded buffer (a scanner flooding us with unknown Call-IDs cannot fill the
//! disk). With `keep = "failed"` nothing is written for a call until it ends with a failure.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use sip_core::transport::{MessageTap, TapDirection, TapEvent};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::config::{CaptureConfig, KeepMode, UdptlLogMode};
use crate::timefmt::rfc3339;

/// Events queued for the writer before the tap starts dropping.
const MAX_QUEUED: usize = 20_000;
/// Call-IDs waiting for their `begin`.
const MAX_PENDING_CALLS: usize = 256;
/// Messages kept per not-yet-announced Call-ID.
const MAX_PENDING_PER_CALL: usize = 16;
/// How long late messages (retransmissions, the 200 to our BYE) are still appended.
const LATE_WINDOW: Duration = Duration::from_secs(64);

/// Buffered `udptl.log` text is written out once it reaches this size (`keep = "all"`).
const UDPTL_FLUSH_BYTES: usize = 16 * 1024;

enum Msg {
    Tap(TapEvent),
    Udptl {
        call_id: String,
        line: String,
    },
    Begin(String),
    End {
        call_id: String,
        failed: bool,
        result_json: String,
    },
    Close(oneshot::Sender<()>),
}

/// Handle to the capture writer.
pub struct Capture {
    udptl_mode: UdptlLogMode,
    tx: mpsc::UnboundedSender<Msg>,
    queued: Arc<AtomicUsize>,
    dropped: Arc<AtomicU64>,
    join: std::sync::Mutex<Option<JoinHandle<()>>>,
}

impl Capture {
    /// Starts the writer. `dir` is the (already resolved) capture directory. Returns the
    /// handle and the tap to install in the transport configuration.
    pub fn start(cfg: &CaptureConfig, dir: PathBuf) -> (Arc<Capture>, MessageTap) {
        let (tx, rx) = mpsc::unbounded_channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicU64::new(0));
        let writer = Writer {
            dir,
            keep: cfg.keep,
            max_bytes: cfg.max_bytes_per_call,
            queued: queued.clone(),
            pending: VecDeque::new(),
            active: HashMap::new(),
            done: HashMap::new(),
        };
        let join = tokio::spawn(writer.run(rx));
        let tap_tx = tx.clone();
        let tap_q = queued.clone();
        let tap_d = dropped.clone();
        let tap = MessageTap::new(move |ev| {
            if tap_q.fetch_add(1, Ordering::Relaxed) >= MAX_QUEUED {
                tap_q.fetch_sub(1, Ordering::Relaxed);
                tap_d.fetch_add(1, Ordering::Relaxed);
                return;
            }
            if tap_tx.send(Msg::Tap(ev)).is_err() {
                tap_q.fetch_sub(1, Ordering::Relaxed);
            }
        });
        (
            Arc::new(Capture {
                udptl_mode: cfg.udptl,
                tx,
                queued,
                dropped,
                join: std::sync::Mutex::new(Some(join)),
            }),
            tap,
        )
    }

    /// The controller handles this Call-ID: capture it.
    pub fn begin(&self, call_id: &str) {
        let _ = self.tx.send(Msg::Begin(call_id.to_string()));
    }

    /// The call ended; `failed` decides whether `keep = "failed"` retains it.
    pub fn end(&self, call_id: &str, failed: bool, result_json: String) {
        let _ = self.tx.send(Msg::End {
            call_id: call_id.to_string(),
            failed,
            result_json,
        });
    }

    /// Records one UDPTL datagram of the call in `udptl.log` according to `[capture] udptl`:
    /// `summary`: `<time> RX|TX <seq> len=<n> <IFP summary>` (T.30 frames in hex, image data
    /// never); `full`: `<time> RX|TX <seq> <hex of the whole datagram>`; `off`: nothing.
    /// Subject to the same `keep` and `max_bytes_per_call` rules as `sip.log` (separate budget).
    pub fn udptl(
        &self,
        call_id: &str,
        at: std::time::SystemTime,
        transmit: bool,
        datagram: &[u8],
        version: t38::T38Version,
        hdlc: &mut Vec<u8>,
    ) {
        if self.udptl_mode == UdptlLogMode::Off {
            return;
        }
        if self.queued.fetch_add(1, Ordering::Relaxed) >= MAX_QUEUED {
            self.queued.fetch_sub(1, Ordering::Relaxed);
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let line = udptl_line(at, transmit, datagram, self.udptl_mode, version, hdlc);
        if self
            .tx
            .send(Msg::Udptl {
                call_id: call_id.to_string(),
                line,
            })
            .is_err()
        {
            self.queued.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Events dropped because the writer could not keep up.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Messages queued for the writer right now.
    pub fn queued(&self) -> usize {
        self.queued.load(Ordering::Relaxed)
    }

    /// Flushes everything queued so far and stops the writer.
    pub async fn shutdown(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Msg::Close(tx)).is_ok() {
            let _ = rx.await;
        }
        let j = self.join.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(j) = j {
            let _ = j.await;
        }
    }
}

/// The Call-ID of a raw SIP message (long or compact header name), if it has one.
pub fn call_id_of(raw: &[u8]) -> Option<String> {
    let head = &raw[..raw.len().min(8192)];
    let text = String::from_utf8_lossy(head);
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim();
            if name.eq_ignore_ascii_case("call-id") || name.eq_ignore_ascii_case("i") {
                let v = value.trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// Directory name for a Call-ID: only `[A-Za-z0-9._@-]`, everything else becomes `_`.
pub fn dir_name(call_id: &str) -> String {
    let s: String = call_id
        .chars()
        .take(100)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '@' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() || s.chars().all(|c| c == '.') {
        "_".into()
    } else {
        s
    }
}

/// One `udptl.log` line (see [`Capture::udptl`]).
pub fn udptl_line(
    at: std::time::SystemTime,
    transmit: bool,
    datagram: &[u8],
    mode: UdptlLogMode,
    version: t38::T38Version,
    hdlc: &mut Vec<u8>,
) -> String {
    use std::fmt::Write as _;
    let seq = match datagram {
        [a, b, ..] => u16::from_be_bytes([*a, *b]).to_string(),
        _ => "-".to_string(),
    };
    let mut s = String::with_capacity(64);
    let _ = write!(
        s,
        "{} {} {} ",
        rfc3339(at),
        if transmit { "TX" } else { "RX" },
        seq
    );
    if mode == UdptlLogMode::Full {
        s.reserve(datagram.len() * 2);
        for b in datagram {
            let _ = write!(s, "{b:02x}");
        }
    } else {
        let _ = write!(
            s,
            "len={} {}",
            datagram.len(),
            ifp_summary(datagram, version, hdlc)
        );
    }
    s.push('\n');
    s
}

/// The primary IFP packet of a datagram, described without any image data. The octets of
/// V.21 (HDLC) fields are T.30 control frames: shown in hex, and at the end of a frame the
/// whole frame (`frame=...`, assembled from the packets of the same direction); the data of every other
/// field (`t4-non-ecm-data`, ...) only as a length.
fn ifp_summary(datagram: &[u8], version: t38::T38Version, hdlc: &mut Vec<u8>) -> String {
    use std::fmt::Write as _;
    let Ok(pkt) = t38::UdptlPacket::decode(datagram) else {
        return "undecodable-udptl".into();
    };
    match t38::Ifp::decode_prefix(&pkt.primary, version) {
        Ok((t38::Ifp::Indicator(i), _)) => format!("ind={i:?}"),
        Ok((t38::Ifp::Data { data_type, fields }, _)) => {
            let mut s = format!("data={data_type:?}");
            for f in &fields {
                let _ = write!(s, " {:?}({})", f.field_type, f.data.len());
                if data_type != t38::DataType::V21 {
                    continue;
                }
                if !f.data.is_empty() {
                    s.push('=');
                    for b in &f.data {
                        let _ = write!(s, "{b:02x}");
                    }
                    hdlc.extend_from_slice(&f.data);
                }
                // the end of a frame: show the whole frame (T.30 control frames carry no image)
                if matches!(
                    f.field_type,
                    t38::FieldType::HdlcFcsOk
                        | t38::FieldType::HdlcFcsBad
                        | t38::FieldType::HdlcFcsOkSigEnd
                        | t38::FieldType::HdlcFcsBadSigEnd
                ) && !hdlc.is_empty()
                {
                    let _ = write!(s, " frame=");
                    for b in hdlc.iter().take(300) {
                        let _ = write!(s, "{b:02x}");
                    }
                    hdlc.clear();
                }
            }
            s
        }
        Err(_) => "undecodable-ifp".into(),
    }
}

fn format_entry(ev: &TapEvent) -> String {
    let (dir, prep) = match ev.direction {
        TapDirection::Received => ("RECV", "from"),
        TapDirection::Sent => ("SEND", "to"),
    };
    let mut s = format!(
        "[{}] {dir} {prep} {} ({}, {} bytes)\n",
        rfc3339(ev.at),
        ev.peer,
        ev.transport.as_str(),
        ev.bytes.len()
    );
    s.push_str(&String::from_utf8_lossy(&ev.bytes));
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s.push('\n');
    s
}

struct Log {
    buf: String,
    bytes: usize,
    skipped: u64,
    /// `buf` has been written to disk already (keep = all).
    on_disk: bool,
    /// `udptl.log` text not written yet.
    udptl_buf: String,
    udptl_bytes: usize,
    udptl_skipped: u64,
}

struct Writer {
    dir: PathBuf,
    keep: KeepMode,
    max_bytes: usize,
    queued: Arc<AtomicUsize>,
    pending: VecDeque<(String, Vec<String>)>,
    active: HashMap<String, Log>,
    done: HashMap<String, (bool, Instant)>,
}

impl Writer {
    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Msg>) {
        while let Some(msg) = rx.recv().await {
            match msg {
                Msg::Tap(ev) => {
                    self.queued.fetch_sub(1, Ordering::Relaxed);
                    self.on_tap(&ev).await;
                }
                Msg::Udptl { call_id, line } => {
                    self.queued.fetch_sub(1, Ordering::Relaxed);
                    self.on_udptl(&call_id, line).await;
                }
                Msg::Begin(id) => self.on_begin(id).await,
                Msg::End {
                    call_id,
                    failed,
                    result_json,
                } => self.on_end(call_id, failed, result_json).await,
                Msg::Close(ack) => {
                    // finish what the producers queued before the Close
                    let _ = ack.send(());
                    break;
                }
            }
        }
    }

    fn call_dir(&self, call_id: &str) -> PathBuf {
        self.dir.join(dir_name(call_id))
    }

    async fn on_tap(&mut self, ev: &TapEvent) {
        let Some(id) = call_id_of(&ev.bytes) else {
            return;
        };
        let entry = format_entry(ev);
        let now = Instant::now();
        self.done.retain(|_, (_, until)| *until > now);
        if self.active.contains_key(&id) {
            self.append(&id, entry).await;
        } else if let Some((kept, _)) = self.done.get(&id) {
            if *kept {
                let dir = self.call_dir(&id);
                let _ = append_file(&dir, "sip.log", entry.as_bytes()).await;
            }
        } else if let Some((_, v)) = self.pending.iter_mut().find(|(k, _)| *k == id) {
            if v.len() < MAX_PENDING_PER_CALL {
                v.push(entry);
            }
        } else {
            if self.pending.len() >= MAX_PENDING_CALLS {
                self.pending.pop_front();
            }
            self.pending.push_back((id, vec![entry]));
        }
    }

    async fn on_begin(&mut self, id: String) {
        let early = match self.pending.iter().position(|(k, _)| *k == id) {
            Some(i) => self.pending.remove(i).map(|(_, v)| v).unwrap_or_default(),
            None => Vec::new(),
        };
        self.active.insert(
            id.clone(),
            Log {
                buf: String::new(),
                bytes: 0,
                skipped: 0,
                on_disk: false,
                udptl_buf: String::new(),
                udptl_bytes: 0,
                udptl_skipped: 0,
            },
        );
        for e in early {
            self.append(&id, e).await;
        }
    }

    async fn on_udptl(&mut self, id: &str, line: String) {
        let keep_all = self.keep == KeepMode::All;
        let dir = self.call_dir(id);
        let max = self.max_bytes;
        let Some(log) = self.active.get_mut(id) else {
            return;
        };
        if log.udptl_bytes + line.len() > max {
            log.udptl_skipped += 1;
            return;
        }
        log.udptl_bytes += line.len();
        log.udptl_buf.push_str(&line);
        if keep_all && log.udptl_buf.len() >= UDPTL_FLUSH_BYTES {
            let text = std::mem::take(&mut log.udptl_buf);
            let _ = append_file(&dir, "udptl.log", text.as_bytes()).await;
        }
    }

    async fn append(&mut self, id: &str, entry: String) {
        let keep_all = self.keep == KeepMode::All;
        let dir = self.call_dir(id);
        let max = self.max_bytes;
        let Some(log) = self.active.get_mut(id) else {
            return;
        };
        if log.bytes + entry.len() > max {
            log.skipped += 1;
            return;
        }
        log.bytes += entry.len();
        if keep_all {
            if append_file(&dir, "sip.log", entry.as_bytes()).await.is_ok() {
                log.on_disk = true;
            }
        } else {
            log.buf.push_str(&entry);
        }
    }

    async fn on_end(&mut self, id: String, failed: bool, result_json: String) {
        let Some(log) = self.active.remove(&id) else {
            return;
        };
        let keep = self.keep == KeepMode::All || failed;
        if keep {
            let dir = self.call_dir(&id);
            let mut utail = log.udptl_buf.clone();
            if log.udptl_skipped > 0 {
                utail.push_str(&format!(
                    "[capture] {} further datagram(s) not stored (max_bytes_per_call reached)\n",
                    log.udptl_skipped
                ));
            }
            if !utail.is_empty() {
                let _ = append_file(&dir, "udptl.log", utail.as_bytes()).await;
            }
            let mut tail = String::new();
            if !log.buf.is_empty() {
                tail.push_str(&log.buf);
            }
            if log.skipped > 0 {
                tail.push_str(&format!(
                    "[capture] {} further message(s) not stored (max_bytes_per_call reached)\n",
                    log.skipped
                ));
            }
            if !tail.is_empty() || !log.on_disk {
                let _ = append_file(&dir, "sip.log", tail.as_bytes()).await;
            }
            let _ = append_file_truncate(&dir, "result.json", result_json.as_bytes()).await;
        }
        self.done.insert(id, (keep, Instant::now() + LATE_WINDOW));
    }
}

async fn append_file(dir: &Path, name: &str, data: &[u8]) -> std::io::Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(name))
        .await?;
    f.write_all(data).await?;
    f.flush().await
}

async fn append_file_truncate(dir: &Path, name: &str, data: &[u8]) -> std::io::Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    tokio::fs::write(dir.join(name), data).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_id_extraction() {
        let m = b"INVITE sip:a@b SIP/2.0\r\nVia: x\r\ni: abc@host\r\n\r\nCall-ID: not-this\r\n";
        assert_eq!(call_id_of(m).as_deref(), Some("abc@host"));
        let m = b"SIP/2.0 200 OK\r\ncall-id :  X-1@h \r\nCSeq: 1 BYE\r\n\r\n";
        assert_eq!(call_id_of(m).as_deref(), Some("X-1@h"));
        assert_eq!(call_id_of(b"garbage"), None);
        assert_eq!(call_id_of(b"\r\n\r\n"), None);
    }

    #[test]
    fn directory_names_are_safe() {
        assert_eq!(dir_name("abc@10.0.0.1"), "abc@10.0.0.1");
        assert_eq!(dir_name("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(dir_name(".."), "_");
        assert_eq!(dir_name(""), "_");
        assert_eq!(dir_name(&"x".repeat(300)).len(), 100);
        assert!(!dir_name("a/b\\c").contains(['/', '\\']));
    }
}
