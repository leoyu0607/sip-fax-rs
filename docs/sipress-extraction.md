# 從 Sipress 抽取的程式碼

來源：https://github.com/leoyu0607/Sipress（MIT，Copyright (c) 2025 LeoYu）

| 狀態 | sipress 原始路徑 | 目的地 | 修改內容 |
|---|---|---|---|
| ⬜ 待抽取（M1） | `core/src/net.rs` | `crates/sip-core/src/net.rs` | 原樣搬移 |
| ⬜ 待抽取（M1） | `core/src/sip/transport.rs` 的 `take_frame` | `crates/sip-core/src/transport/framing.rs` | 只取 framing 函式和測試 |
| ⬜ 待抽取（M1） | `core/src/sip/register.rs` 的 `DigestChallenge`、`authorization_header` | `crates/sip-core/src/auth.rs` | 移除 REGISTER 字串模板 |
| ⬜ 待抽取（M1/M5） | `core/src/rtp/audio.rs` 的 G.711 函式 | `crates/sip-core/src/rtp/g711.rs` | 只取 codec |
| ⬜ 待抽取（M1/M5） | `core/src/rtp/packet.rs` 的 `RtpPacket` | `crates/sip-core/src/rtp/packet.rs` | 修正 decode 的 CSRC、extension、padding、marker |

只作參考、不直接搬移：`core/src/sip/message.rs`（UAS 回應鏡像、`ack_for_non2xx`）。
