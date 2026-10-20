# SDP test vectors

These files are **hand-written representative samples, not captures**. They follow the SDP layout
that Sonus (Ribbon) SBC and AudioCodes gateways document publicly (session name, `o=` naming, payload
type order, `a=ptime`, `telephone-event` with `fmtp 0-15`, the `T38Fax*` attribute set of ITU-T T.38
Annex D), but no byte of them comes from a real device trace. Real traces are scheduled for M6
(`Sonus/AudioCodes SIP trace` regression set).

All files use CRLF line endings, as SDP on the wire does.

| file | purpose |
|---|---|
| `sonus_audio_offer.sdp` | Sonus style G.711 audio offer with telephone-event |
| `audiocodes_audio_offer.sdp` | AudioCodes style audio offer, PCMA preferred |
| `audiocodes_t38_reinvite.sdp` | T.38 re-INVITE offer, bare boolean attributes |
| `sonus_t38_reinvite_mixed_case.sdp` | T.38 offer with mixed-case attribute names, `:0`/`:1` booleans, FEC, audio line already at port 0 |
| `t38_minimal_offer.sdp` | only one T.38 attribute; exercises defaults; media-level `c=` |
| `audio_and_t38_offer.sdp` | audio and image sections in one offer |
| `audio_no_common_codec_offer.sdp` | G.729/G.723 only (no PCMU/PCMA) |
