# 從 Sipress 抽取的程式碼

來源：https://github.com/leoyu0607/Sipress（MIT，Copyright (c) 2025 LeoYu）

| 狀態 | sipress 原始路徑 | 目的地 | 修改內容 |
|---|---|---|---|
| ✅ M1a | `core/src/net.rs` | `crates/sip-core/src/net.rs` | 原樣搬移（只在檔頭加來源註解）；4 個原有測試保留且全數通過 |
| ✅ M1a | `core/src/sip/transport.rs` 的 `take_frame` | `crates/sip-core/src/transport/framing.rs` | 只取 framing 函式和 2 個原有測試（`tcp_framing_follows_content_length`、`tcp_framing_rejects_oversized_messages`（測試名加 `p11_` 前綴以符合測試計畫 P11 的命名規則），改為 bytes 比對）。修改：回傳 `Result<Option<Vec<u8>>, FrameError>`（原為 `anyhow::Result<Option<String>>`，避免有損 UTF-8 轉換、移除 anyhow 依賴）；`MAX_MESSAGE` 改用 `message::MAX_MESSAGE_SIZE`；`Content-Length` 存在但不是純十進位數字時回 `FrameError::InvalidContentLength`（原本悄悄當成 0，會讓串流失去同步；RFC 4475 3.1.2.3 要求關閉連線）；長度加法改用 saturating 避免溢位。新增 4 個測試 |
| ✅ M1a | `core/src/sip/register.rs` 的 `DigestChallenge`、`authorization_header` | `crates/sip-core/src/auth.rs` | 移除 REGISTER 字串模板（`build`）與其測試。保留 `parse`、`header_name`、`authorization_header`、`build_authorization`、`md5_hex`、`split_quoted_csv` 和 4 個原有 Digest 測試。修改：新增 `from_header_value`（直接吃 header 值，不必拿整段原始回應）；新增 `build_authorization_with_cnonce`（讓 RFC 2617 已知答案測試可決定性重現；`build_authorization` 內部呼叫它並用 `rand` 產生 cnonce）；`Digest` scheme 比對改為不分大小寫；原測試 `build_auth_response` 補上獨立計算的 response 比對；衍生 `PartialEq, Eq`。新增 4 個測試 |
| ⬜ 待抽取（M1/M5） | `core/src/rtp/audio.rs` 的 G.711 函式 | `crates/sip-core/src/rtp/g711.rs` | 只取 codec |
| ⬜ 待抽取（M1/M5） | `core/src/rtp/packet.rs` 的 `RtpPacket` | `crates/sip-core/src/rtp/packet.rs` | 修正 decode 的 CSRC、extension、padding、marker |

只作參考、不直接搬移：`core/src/sip/message.rs`（UAS 回應鏡像、`ack_for_non2xx`）。
