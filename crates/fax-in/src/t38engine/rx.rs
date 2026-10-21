//! Receive side of the T.38 engine: IFP packets to T.30 events.
//!
//! T.38 carries each HDLC frame as `hdlc-data` fields followed by `hdlc-fcs-OK` (or `-BAD`,
//! with or without `-sig-end`), so no bit level HDLC is needed. Non-ECM image data arrives as
//! `t4-non-ecm-data` and ends with `t4-non-ecm-sig-end`. The octets of both are in the order
//! T.30 and T.4 write them (first transmitted bit = most significant bit), which is exactly what
//! the `t30` and `t4` crates take; unlike SpanDSP's internal representation nothing is
//! bit-reversed here (checked against SpanDSP in the X tests).
//!
//! # ECM
//!
//! In the error correction mode the image is carried as HDLC frames at the image rate: after the
//! training indicator come `hdlc-data` fields of a fast data type (`v17-14400` and the like)
//! closed by `hdlc-fcs-OK` / `hdlc-fcs-BAD`, the last frame (the third RCP) with
//! `-sig-end`. They are turned into the same [`Event::Frame`]s as V.21 frames; T.30 tells FCD and
//! RCP frames from the commands by their FCF. A frame hit by a lost packet gets a bad FCS, which
//! is how the frame ends up in the PPR bitmap.
//!
//! Real gateways repeat indicators and `sig-end` fields (the sender's redundancy and the
//! "x3" rule), so repeats are recognised by context and dropped, the same way SpanDSP's
//! `t38_terminal` does it.

use t30::{Event, RxSignal};
use t38::{DataType, FieldType, Ifp, Indicator};

/// Longest HDLC frame accepted (DIS with every extension is under 100 octets).
const MAX_HDLC_OCTETS: usize = 260;

#[derive(Debug, Default)]
pub(crate) struct RxAdapter {
    /// The last indicator seen, or implied by data.
    indicator: Option<Indicator>,
    hdlc: Vec<u8>,
    /// Octets of the current frame (or whole packets) are known to be missing.
    missing: bool,
    /// Image (or TCF) data is flowing and its end has not been seen.
    image_active: bool,
    pub(crate) duplicates: u64,
    pub(crate) ignored: u64,
}

impl RxAdapter {
    /// The transport lost data: the frame in progress cannot be trusted.
    pub(crate) fn on_loss(&mut self) {
        self.missing = true;
    }

    pub(crate) fn on_ifp(&mut self, ifp: &Ifp, out: &mut Vec<Event>) {
        match ifp {
            Ifp::Indicator(ind) => self.on_indicator(*ind, out),
            Ifp::Data { data_type, fields } => {
                for f in fields {
                    self.on_field(*data_type, f.field_type, &f.data, out);
                }
            }
        }
    }

    fn on_indicator(&mut self, ind: Indicator, out: &mut Vec<Event>) {
        if self.indicator == Some(ind) {
            self.duplicates += 1;
            return;
        }
        self.indicator = Some(ind);
        let signal = Self::signal_of(ind);
        self.indicator_event(signal, out);
    }

    /// The T.30 view of an indicator.
    fn signal_of(ind: Indicator) -> Option<RxSignal> {
        match ind {
            Indicator::NoSignal => Some(RxSignal::NoSignal),
            Indicator::Cng => Some(RxSignal::Cng),
            Indicator::Ced => Some(RxSignal::Ced),
            Indicator::V21Preamble => Some(RxSignal::V21Preamble),
            Indicator::V27ter2400Training
            | Indicator::V27ter4800Training
            | Indicator::V29_7200Training
            | Indicator::V29_9600Training
            | Indicator::V17_7200ShortTraining
            | Indicator::V17_7200LongTraining
            | Indicator::V17_9600ShortTraining
            | Indicator::V17_9600LongTraining
            | Indicator::V17_12000ShortTraining
            | Indicator::V17_12000LongTraining
            | Indicator::V17_14400ShortTraining
            | Indicator::V17_14400LongTraining
            | Indicator::V33_12000Training
            | Indicator::V33_14400Training => Some(RxSignal::Training),
            Indicator::V8Ansam
            | Indicator::V8Signal
            | Indicator::V34CntlChannel1200
            | Indicator::V34PriChannel
            | Indicator::V34CcRetrain => None,
        }
    }

    fn is_training(ind: Option<Indicator>) -> bool {
        ind.and_then(Self::signal_of) == Some(RxSignal::Training)
    }

    fn indicator_event(&mut self, signal: Option<RxSignal>, out: &mut Vec<Event>) {
        // A new signal starts with a clean slate.
        self.hdlc.clear();
        self.missing = false;
        match signal {
            Some(RxSignal::NoSignal) => {
                // An image whose end marker was lost still ends when its signal goes away.
                self.image_active = false;
                out.push(Event::Signal(RxSignal::NoSignal));
            }
            Some(s) => {
                if s == RxSignal::Training {
                    self.image_active = true;
                }
                out.push(Event::Signal(s));
            }
            None => self.ignored += 1,
        }
    }

    fn on_field(&mut self, dt: DataType, ft: FieldType, data: &[u8], out: &mut Vec<Event>) {
        match dt {
            DataType::V21 => self.on_hdlc_field(ft, data, out),
            DataType::V27ter2400
            | DataType::V27ter4800
            | DataType::V29_7200
            | DataType::V29_9600
            | DataType::V17_7200
            | DataType::V17_9600
            | DataType::V17_12000
            | DataType::V17_14400
            | DataType::V33_12000
            | DataType::V33_14400 => self.on_image_field(ft, data, out),
            _ => self.ignored += 1,
        }
    }

    fn on_hdlc_field(&mut self, ft: FieldType, data: &[u8], out: &mut Vec<Event>) {
        self.on_hdlc(ft, data, false, out);
    }

    /// HDLC fields of V.21 (`fast = false`) or of an image modem (ECM, `fast = true`).
    fn on_hdlc(&mut self, ft: FieldType, data: &[u8], fast: bool, out: &mut Vec<Event>) {
        let (ok, sig_end) = match ft {
            FieldType::HdlcData if fast => {
                if !Self::is_training(self.indicator) {
                    // HDLC data at image rate with no training indicator seen (lost): the
                    // signal started with this field; a frame not starting with the address
                    // octet lost its beginning.
                    self.indicator = Some(Indicator::V17_14400LongTraining);
                    self.hdlc.clear();
                    self.missing = data.first() != Some(&0xFF);
                    out.push(Event::Signal(RxSignal::Training));
                }
                self.append(data);
                return;
            }
            FieldType::HdlcData => {
                if self.indicator != Some(Indicator::V21Preamble) {
                    // HDLC can start with no indicator at all; accept it, like SpanDSP, but
                    // a frame that does not start with the address octet lost its beginning.
                    self.indicator = Some(Indicator::V21Preamble);
                    self.hdlc.clear();
                    self.missing = data.first() != Some(&0xFF);
                    out.push(Event::Signal(RxSignal::V21Preamble));
                }
                self.append(data);
                return;
            }
            FieldType::HdlcFcsOk => (true, false),
            FieldType::HdlcFcsBad => (false, false),
            FieldType::HdlcFcsOkSigEnd => (true, true),
            FieldType::HdlcFcsBadSigEnd => (false, true),
            FieldType::HdlcSigEnd => {
                self.hdlc.clear();
                self.missing = false;
                self.signal_end(out);
                return;
            }
            // Image fields on a V.21 data type, or ECM-only fields: nothing to do.
            _ => {
                self.ignored += 1;
                return;
            }
        };
        // Some implementations put the last octets into the FCS field.
        self.append(data);
        if !self.hdlc.is_empty() {
            let frame = std::mem::take(&mut self.hdlc);
            out.push(Event::Frame {
                data: frame,
                fcs_ok: ok && !self.missing,
            });
        } else {
            self.duplicates += 1;
        }
        self.missing = false;
        if sig_end {
            self.signal_end(out);
        }
    }

    fn append(&mut self, data: &[u8]) {
        if self.hdlc.len() + data.len() <= MAX_HDLC_OCTETS {
            self.hdlc.extend_from_slice(data);
        } else {
            self.missing = true;
        }
    }

    /// Carrier dropped after HDLC: the same as a `no-signal` indicator, once.
    fn signal_end(&mut self, out: &mut Vec<Event>) {
        if self.indicator != Some(Indicator::NoSignal) {
            self.indicator = Some(Indicator::NoSignal);
            out.push(Event::Signal(RxSignal::NoSignal));
        } else {
            self.duplicates += 1;
        }
    }

    fn on_image_field(&mut self, ft: FieldType, data: &[u8], out: &mut Vec<Event>) {
        match ft {
            FieldType::T4NonEcmData => {
                self.image_active = true;
                if !data.is_empty() {
                    out.push(Event::ImageData(data.to_vec()));
                }
            }
            FieldType::T4NonEcmSigEnd => {
                if !self.image_active {
                    // A repeat of a sig-end that already ended the image. The copies the
                    // sender emits for UDP loss are identical, so they carry the last octets
                    // again: they must not be taken for new data (they used to be dropped by
                    // accident while T.30 was still "sending" its response).
                    self.duplicates += 1;
                    return;
                }
                if !data.is_empty() {
                    out.push(Event::ImageData(data.to_vec()));
                }
                self.image_active = false;
                self.indicator = Some(Indicator::NoSignal);
                out.push(Event::DataEnd);
            }
            FieldType::HdlcData
            | FieldType::HdlcSigEnd
            | FieldType::HdlcFcsOk
            | FieldType::HdlcFcsBad
            | FieldType::HdlcFcsOkSigEnd
            | FieldType::HdlcFcsBadSigEnd => self.on_hdlc(ft, data, true, out),
            _ => self.ignored += 1,
        }
    }
}
