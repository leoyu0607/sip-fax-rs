//! Receiver configuration.

/// Which high-speed modems the receiver offers in its DIS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Modems {
    /// V.27ter (2400 / 4800 bit/s).
    pub v27ter: bool,
    /// V.29 (7200 / 9600 bit/s).
    pub v29: bool,
    /// V.17 (7200 .. 14400 bit/s). T.30 only allows V.17 together with V.27ter and V.29, so
    /// enabling it also advertises those two.
    pub v17: bool,
}

impl Default for Modems {
    fn default() -> Self {
        Self {
            v27ter: true,
            v29: true,
            v17: true,
        }
    }
}

/// Widest page the receiver accepts (T.30 Table 2 bits 17 and 18).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PaperWidth {
    /// 215 mm, 1728 pixels per line.
    A4,
    /// 255 mm, 2048 pixels per line.
    B4,
    /// 303 mm, 2432 pixels per line.
    A3,
}

impl PaperWidth {
    /// Pixels per line at 8 pels/mm.
    pub fn pixels(self) -> u32 {
        match self {
            PaperWidth::A4 => 1728,
            PaperWidth::B4 => 2048,
            PaperWidth::A3 => 2432,
        }
    }
}

/// Longest page the receiver accepts (T.30 Table 2 bits 19 and 20).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PaperLength {
    /// A4 (297 mm).
    A4,
    /// A4 and B4 (364 mm).
    B4,
    /// Unlimited.
    Unlimited,
}

/// Minimum scan line time (T.30 Table 2 bits 21 to 23), at 3.85 lines/mm.
///
/// The code is the 3-bit number whose least significant bit is bit 21; the values follow
/// SpanDSP 3.1.1 (`T30_MIN_SCAN_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MinScanTime {
    /// 20 ms (code 0).
    Ms20,
    /// 5 ms (code 1).
    Ms5,
    /// 10 ms (code 2).
    Ms10,
    /// 40 ms (code 4).
    Ms40,
    /// 0 ms: no fill bits needed (code 7).
    Ms0,
    /// Any other code (combinations that T.30 defines for the two vertical resolutions).
    Other(u8),
}

impl MinScanTime {
    /// The 3-bit code.
    pub fn code(self) -> u8 {
        match self {
            MinScanTime::Ms20 => 0,
            MinScanTime::Ms5 => 1,
            MinScanTime::Ms10 => 2,
            MinScanTime::Ms40 => 4,
            MinScanTime::Ms0 => 7,
            MinScanTime::Other(c) => c & 7,
        }
    }

    /// From the 3-bit code.
    pub fn from_code(code: u8) -> Self {
        match code & 7 {
            0 => MinScanTime::Ms20,
            1 => MinScanTime::Ms5,
            2 => MinScanTime::Ms10,
            4 => MinScanTime::Ms40,
            7 => MinScanTime::Ms0,
            c => MinScanTime::Other(c),
        }
    }
}

/// `T38FaxRateManagement`: who runs the training check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcfMode {
    /// `transferredTCF`: the TCF crosses the IP network and this side judges it.
    Transferred,
    /// `localTCF`: the gateway trains locally; no TCF arrives, CFR follows the DCS directly.
    Local,
}

/// T.30 timers in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timers {
    /// T1, the identification timeout (35 s +- 5): from the start of the call until the first
    /// valid command, and again after returning to phase B.
    pub t1_ms: u32,
    /// T2, the command timeout (6 s +- 1): how long to wait for the next command or image.
    pub t2_ms: u32,
    /// T4, the response timeout (3 s +- 15 %): the interval of DIS retransmissions.
    pub t4_ms: u32,
    /// Silence tolerated in the middle of an image before the data is considered finished.
    pub data_idle_ms: u32,
}

impl Default for Timers {
    fn default() -> Self {
        Self {
            t1_ms: 35_000,
            t2_ms: 6_000,
            t4_ms: 3_000,
            data_idle_ms: 15_000,
        }
    }
}

/// Receiver configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Our identification (CSI); empty sends no CSI frame.
    pub local_id: String,
    /// Offered modems.
    pub modems: Modems,
    /// Offer fine vertical resolution (7.7 lines/mm). Standard (3.85) is always offered.
    pub fine: bool,
    /// Offer T.4 two-dimensional coding (MR). MH is always offered.
    pub mr: bool,
    /// Offer T.6 coding (MMR). T.30 only permits T.6 together with ECM (Table 2 notes 9 and
    /// 17), so the DIS carries bit 31 only when [`Config::ecm`] is set as well; a DCS that
    /// selects T.6 is decoded either way.
    pub mmr: bool,
    /// Offer and accept the error correction mode (T.30 Annex A, DIS bit 27). Off by default
    /// in the library (the product configuration `[trunks.fax] ecm` defaults to on).
    pub ecm: bool,
    /// PPR responses tolerated for one block (counting from the last CTC) before the call is
    /// dropped. A sender follows the fourth PPR with CTC or EOR, so this only stops a peer that
    /// never does.
    pub ecm_max_ppr: u32,
    /// Widest accepted page.
    pub max_width: PaperWidth,
    /// Longest accepted page.
    pub max_length: PaperLength,
    /// Minimum scan line time advertised.
    pub min_scan: MinScanTime,
    /// Advertise Internet-aware fax (T.38) in DIS bit 3.
    pub t38_iaf: bool,
    /// Training check mode.
    pub tcf: TcfMode,
    /// With [`TcfMode::Local`]: answer the DCS with CFR anyway. Off by default: with local TCF
    /// the training result is settled at the gateway (and SpanDSP's receiver sends nothing), so
    /// a CFR would only arrive in the middle of the page and be taken for an unexpected frame.
    pub local_tcf_sends_cfr: bool,
    /// Shortest acceptable TCF in milliseconds at the negotiated rate.
    pub tcf_min_ms: u32,
    /// Minimum share of zero bits in the TCF, in percent.
    pub tcf_zero_percent: u32,
    /// A page with more than this share of bad lines (percent) is answered with RTN.
    pub rtn_bad_percent: u32,
    /// A page with more than this share of bad lines (percent) but not enough for RTN is
    /// answered with RTP (accept, retrain).
    pub rtp_bad_percent: u32,
    /// A run of consecutive bad lines longer than this many tenths of a millimetre of paper is
    /// answered with RTN (25 = 2.5 mm: 10 lines at standard, 19 at fine resolution).
    pub max_bad_run_tenth_mm: u32,
    /// Timers.
    pub timers: Timers,
    /// Number of times the DIS is retransmitted after T4 expiries before giving up.
    pub dis_retries: u32,
    /// Row limit handed to the T.4 decoder.
    pub max_rows: u32,
    /// Answer a frame with a bad FCS with CRP (command repeat). Off by default: the sender's T4
    /// repeats the command anyway, which is also what SpanDSP does without the CRP feature.
    pub crp_on_bad_fcs: bool,
    /// Unexpected final frames tolerated before the call is dropped with DCN.
    pub max_unexpected_frames: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            local_id: String::new(),
            modems: Modems::default(),
            fine: true,
            mr: true,
            mmr: false,
            ecm: false,
            ecm_max_ppr: 12,
            max_width: PaperWidth::A4,
            max_length: PaperLength::Unlimited,
            min_scan: MinScanTime::Ms0,
            t38_iaf: false,
            tcf: TcfMode::Transferred,
            local_tcf_sends_cfr: false,
            tcf_min_ms: 1_200,
            tcf_zero_percent: 95,
            rtn_bad_percent: 10,
            rtp_bad_percent: 5,
            max_bad_run_tenth_mm: 25,
            timers: Timers::default(),
            dis_retries: 3,
            max_rows: t4::DEFAULT_MAX_ROWS,
            crp_on_bad_fcs: false,
            max_unexpected_frames: 3,
        }
    }
}
