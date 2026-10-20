#![no_main]
//! Feeds arbitrary bytes to the SDP parser and, for parsed sessions, exercises T.38 attribute
//! interpretation, offer/answer and re-serialisation.

use libfuzzer_sys::fuzz_target;
use sip_core::sdp::{
    AnswerPolicy, AudioCodec, OriginTracker, SessionDescription, T38Capabilities, T38Params,
    answer_audio, answer_offer, answer_t38, parse_sdp,
};

fuzz_target!(|data: &[u8]| {
    let Ok(sdp) = parse_sdp(data) else { return };
    // Serialisation is canonical (RFC 4566 line order), so the *text* must be a fixed point.
    // (The model may differ in the relative order of uninterpreted `extra` lines of different
    // kinds, which is normalised by the first serialisation.)
    let text = sdp.to_sdp_string();
    let again = SessionDescription::parse(&text).expect("serialised SDP must parse");
    assert_eq!(again.to_sdp_string(), text, "serialise/parse is not a fixed point");
    for m in &sdp.media {
        let _ = T38Params::from_media(m);
        let _ = answer_audio(m, &[AudioCodec::PCMU, AudioCodec::PCMA], 1);
        let _ = answer_t38(m, &T38Capabilities::default(), 1);
    }
    let policy = AnswerPolicy::new("127.0.0.1", 4000, 4002);
    let mut origin = OriginTracker::new("-", 1, 1, "127.0.0.1");
    let _ = answer_offer(&sdp, &policy, &mut origin);
});
