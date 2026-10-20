# RFC 4475 SIP torture test messages

Files are the **bit-exact** messages from the RFC 4475 appendix A archive (the base64 gzip/tar block
in the RFC text, extracted with the RFC's own algorithm: lines between `-- BEGIN MESSAGE ARCHIVE --`
and `-- END MESSAGE ARCHIVE --`, base64-decoded, `tar xzf`). No hand transcription is involved, so the
`<hex>`, `<repeat>`, `<allOneLine>` markup of RFC section 2 is already resolved and the CRLF line
endings are the original ones. `<name>.dat` in the archive is stored here as `<name>.sip`.

Layout follows the RFC classification where it exists:

* `valid/`: RFC 3.1.1 (valid) plus the well-formed messages of 3.3 (application-layer semantics:
  unknown schemes, unknown Content-Type, ...) which a parser must accept.
* `invalid/`: RFC 3.1.2 (invalid) plus 3.2.1 `badbranch`, the malformed 3.3 messages (`insuf`,
  `multi01`, `mcl01`) and 3.4.1 `inv2543`. NOTE: "invalid" is the RFC's label; whether
  `parse_message` rejects or tolerates each one is asserted per message in
  `tests/rfc4475.rs` (table `INVALID_EXPECTATIONS`) with the RFC's reasoning.

Not included: `test.dat` (present in the archive but not described in the RFC text).
