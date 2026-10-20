# SpanDSP golden IFP vectors (test case I01)

Every IFP packet that real SpanDSP emitted during complete T.38 fax sessions. They are the
independent reference for the PER codec in `src/ifp.rs`: `tests/golden.rs` decodes each packet
and re-encodes it, and the result must be byte-identical.

* SpanDSP: **v3.1.1** (`git clone --branch v3.1.1 https://github.com/freeswitch/spandsp`, built
  by `docker/test-rocky9.Dockerfile`).
* Generator: `tools/fax-sim/src/bin/gen_t38_vectors.rs`, which runs the SpanDSP `t38_terminal`
  loopback (`tools/fax-sim/src/t38_loopback.rs`, caller transmits a 2-page TIFF to the answerer)
  and records what the `t38_core` transmit handler is given, before any transport (UDPTL) framing.
* Regenerate (inside the test image, from the repository root):

  ```sh
  scripts/gen-t38-vectors.sh
  ```

## File format (`*.ifpb`)

A plain sequence of records, no header:

| field | size | meaning |
|---|---|---|
| `dir` | 1 octet | 0 = emitted by the calling terminal, 1 = by the answering terminal |
| `len` | 4 octets, big endian | length of the IFP packet |
| `ifp` | `len` octets | the IFP packet exactly as SpanDSP produced it |

## Scenarios

The input is two pages (1728x300 at 204x98 dpi and 1728x400 at 204x196 dpi), so the sessions
contain a DCS change, MPS and EOP. All eight sessions complete with T.30 code 0 on both sides.

| file | ECM | `t38_set_t38_version` | modems offered |
|---|---|---|---|
| `non_ecm_v0` | no | 0 | V.27ter, V.29, V.17 (negotiates V.17 14400) |
| `non_ecm_v3` | no | 3 | same |
| `ecm_v0` | yes | 0 | same |
| `ecm_v3` | yes | 3 | same |
| `non_ecm_v0_v29` | no | 0 | V.29 only |
| `ecm_v3_v29` | yes | 3 | V.29 only |
| `non_ecm_v0_v27ter` | no | 0 | V.27ter only |
| `ecm_v3_v27ter` | yes | 3 | V.27ter only |

SpanDSP never emits the T.38 version 2+ additions (V.8 / V.34 / V.33, cm/jm/ci/v34rate) in a
fax-only session, so those code points are covered by the round-trip property tests and by
hand-derived byte expectations in `src/ifp.rs` instead.
