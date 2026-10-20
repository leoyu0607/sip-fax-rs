# sip-core 設計規格（M1）

- 撰寫：Opus
- 實作者請依本規格實作；需要偏離時，先在 `docs/DECISIONS.md` 追加 ADR 說明理由。

## 0. 原則
- `sip-core` 是**通用 SIP 堆疊**，同時支援 UAS 和 UAC，不得出現任何與傳真有關的概念。T.38 的 SDP 屬性屬於 SDP 標準的一部分，例外允許。
- 依賴限制在 `tokio`、`bytes`、`thiserror`、`tracing`、`rand`。新增其他依賴前要先寫 ADR。
- 解析和序列化函式都是**純函式**，不做 I/O，方便 proptest 和 fuzz。
- 所有計時器都使用 `tokio::time`。測試以 `tokio::time::pause()` 和 `advance()` 驅動，不允許真的 sleep。
- 協定層之間以 channel 串接，每一層都可以單獨餵入訊息並觀察輸出來測試。

## 1. 模組
```
sip-core/src/
  lib.rs
  net.rs              # 抽取自 sipress（host/IPv6 工具）
  auth.rs             # 抽取自 sipress（Digest，UAC 用）
  message/
    mod.rs            # Request / Response / SipMessage / Method / StatusCode
    headers.rs        # Headers：保留順序、名稱不分大小寫、compact form 正規化
    uri.rs            # SipUri（sip/sips/tel 以外拒絕）、參數、escape
    name_addr.rs      # NameAddr（display name、<uri>、params，包含 tag）
    via.rs            # Via（transport、sent-by、branch、received、rport、maddr、ttl）
    cseq.rs
    parse.rs          # parse_message(&[u8]) -> Result<(SipMessage, usize), ParseError>
    serialize.rs      # 序列化時自動重算 Content-Length
  sdp/
    mod.rs            # SessionDescription、MediaDescription、Attribute
    t38.rs            # T38Params 與屬性互轉、answer 協商
    negotiate.rs      # audio answer（PCMU/PCMA/telephone-event）、t38 answer
  transport/
    mod.rs            # TransportLayer、Incoming、Outgoing、TransportKind
    udp.rs
    tcp.rs            # listener 與連線池；framing.rs 抽取自 sipress take_frame
    framing.rs
  transaction/
    mod.rs            # TransactionLayer、TransactionKey（RFC 3261 17.1.3 / 17.2.3）
    timers.rs         # T1=500ms、T2=4s、T4=5s，可設定
    server_invite.rs
    server_non_invite.rs
    client_invite.rs
    client_non_invite.rs
  dialog/
    mod.rs            # Dialog、DialogId、route set、CSeq 管理
    uas.rs            # 2xx 重送直到 ACK（13.3.1.4）、ACK 逾時
    session_timer.rs  # RFC 4028
  endpoint.rs         # 對外高階 API
```

## 2. 訊息層要求
- **解析**
  - 支援 header folding（LWS），會在解析時展開。
  - 逗號分隔的多值 header 拆成多筆，但要正確略過引號字串和 `<...>` 裡的逗號。
  - 認得 compact form：`i f t v m l c k s e o u r b x y`。
  - header 名稱不分大小寫。冒號前允許空白（`Via :`）。
- **Content-Length**
  - 串流（TCP）模式下必須有 Content-Length，回傳值是已消耗的位元組數。
  - 資料報（UDP）模式下若缺少 Content-Length，以剩餘位元組為 body。若 Content-Length 大於剩餘長度，視為錯誤；若小於，截斷多餘的部分（RFC 3261 18.3）。
- **必要 header**：To、From、CSeq、Call-ID、Max-Forwards（只檢查 request）、Via，任何一個缺少就回 `ParseError`。上層收到 request 時，對可以回應的情況回 400。
- **序列化**
  - 保留未知 header 的原文。
  - 輸出統一使用 CRLF。
  - 自動設定 Content-Length。
- **大小上限**：單一訊息最大 64 KiB，header 數量最多 256，超過就拒絕。
- **測試向量**：RFC 4475 的 torture 訊息存放在 `crates/sip-core/tests/vectors/rfc4475/*.sip`（原始 bytes），合法和不合法分開放。

## 3. Transport
- UDP 使用一個綁定的 socket，從同一個 socket 回應。TCP 使用 listener 加上主動連線，連線池以 `(peer addr)` 為 key，閒置逾時可設定。
- 收到 request 時（RFC 3261 18.2.1 / RFC 3581）：
  - 若 sent-by 與來源 IP 不同，或 Via 帶有 `rport`，就補上 `received=`。
  - 若 Via 帶有 `rport`，填入來源 port。
- 回應的路由規則（RFC 3261 18.2.2 + RFC 3581）：
  - 可靠傳輸：走原本的連線。如果連線已經斷了，依 sent-by 重新連線。
  - UDP：若有 `rport`，送到 received 加 rport；否則若有 `received`，送到 received 加 sent-by port；否則送到 sent-by。
- 送出 request 時，自動填入頂端 Via 的 transport 和 sent-by。sent-by 使用設定的對外位址，可以處理 NAT 或多 IP。
- 收到 CRLF keep-alive 要忽略，雙 CRLF 要回單 CRLF（RFC 5626）。

## 4. Transaction 層
- **Server transaction**
  - Key 是頂端 Via 的 branch（magic cookie `z9hG4bK`）、sent-by 和 method。ACK 和 CANCEL 比對 INVITE 時另行處理。
  - 沒有 magic cookie 的舊式比對（RFC 2543）**不支援**，這類訊息直接交給 TU，並記錄 warn。
- **INVITE server transaction**
  - 若 TU 在 200ms 內沒有送出任何 provisional，自動送 100 Trying。
  - 收到重送的 INVITE 時，重送最後一個回應。
  - 送出 2xx 後依 **RFC 6026** 進入 Accepted 狀態（Timer L = 64·T1），在這段期間吸收重送的 INVITE，並把 ACK 往上交給 TU。
  - 送出 3xx–6xx 後進入 Completed：以 Timer G 重送（上限 T2），等 ACK；Timer H 逾時就通知 TU；收到 ACK 後進入 Confirmed，Timer I 結束後終止。
- **non-INVITE server transaction**：收到重送時重送最後一個回應。Timer J 為 64·T1（UDP）或 0（TCP）。
- **INVITE client transaction**（re-INVITE、UAC INVITE 使用）
  - Timer A 和 Timer B。TCP 不使用 Timer A。
  - 收到非 2xx 回應時由 transaction 自己產生 ACK，進入 Completed，等 Timer D。
  - 2xx 依 RFC 6026 交給 TU，由 TU 送 ACK，並吸收重送的 2xx，不重複上報。
- **non-INVITE client transaction**：Timer E、F、K。
- **CANCEL**：UAS 收到 CANCEL 時，若對應的 INVITE 還沒送出最終回應，對 CANCEL 回 200，對 INVITE 回 487。找不到對應交易時回 481。
- 時間常數可以設定。測試一律以 paused time 驗證計時器的時間點，誤差 0ms。

## 5. Dialog 與 UAS core
- 從 INVITE 加上我方的 2xx 建立 dialog：
  - route set 取 INVITE 中 Record-Route 的原順序。
  - remote target 取 INVITE 的 Contact。
  - remote seq 取 INVITE 的 CSeq。
  - local seq 從亂數起始，不超過 2^31。
- **2xx 可靠性**（13.3.1.4）：UAS core 以 T1 起始、每次加倍、上限 T2 重送 2xx，直到收到 ACK。超過 64·T1 還沒收到 ACK，就上報 `AckTimeout`，由上層決定是否發 BYE。
- **in-dialog request 建構**
  - Request-URI 和 Route 依 loose routing（`;lr`）規則產生。如果 route set 第一個項目沒有 `lr`（strict routing），也要支援。
  - CSeq 遞增。
  - From 和 To 帶上雙方的 tag。
- **in-dialog request 處理**
  - CSeq 比 remote seq 小，回 500。
  - 收到不屬於任何 dialog 的 request，回 481。
  - 收到 BYE，回 200 並結束 dialog。
  - 收到 re-INVITE：如果我方的 re-INVITE 還在進行中，回 **491**；否則把 offer 交給上層。上層可以回 200 加 answer，或回 488。
  - 收到 OPTIONS，回 200，並帶 Allow、Accept、Supported。
  - 收到 INFO 或 UPDATE（沒有 SDP 時），回 200。UPDATE 帶 SDP 的情況交給上層處理（M1 先回 488 加 TODO）。
- **我方發出的 re-INVITE**：收到 491 時，在 0–2s 內隨機延遲後重試（我方不是 Call-ID 的擁有者），重試次數上限可以設定。
- **Session timer（RFC 4028，UAS 角色）**
  - 對方在 INVITE 帶 `Session-Expires` 和 `Supported: timer` 時，在 200 中回覆 `Session-Expires` 和 refresher。預設由 refresher=uac 負責更新。
  - 如果 refresher 是我方，在一半時間時發 re-INVITE，沿用相同 SDP 並遞增 o= 版本。
  - 如果 refresher 是對方，在逾時前沒收到更新，就發 BYE。
  - 對方的 Min-SE 比我方要求的小時，回 422。

## 6. SDP
- **模型**：v/o/s/c/t/m/a，未知的行原樣保留。c 可以出現在 session 層，也可以出現在 media 層。
- **o= 版本**：同一個 dialog 內，每次送出新的 offer 或 answer 時版本號加一；內容沒有改變時，版本號不變。
- **audio answer**：依偏好順序（預設 PCMU、PCMA），從 offer 中取出交集。若 offer 有 telephone-event 就保留。交集為空時，回傳 `NoCommonCodec`，上層會回 488。
- **T.38（`m=image <port> udptl t38`）**
  - 屬性要支援：`T38FaxVersion`、`T38MaxBitRate`、`T38FaxRateManagement`（localTCF|transferredTCF）、`T38FaxMaxBuffer`、`T38FaxMaxDatagram`、`T38FaxUdpEC`（t38UDPFEC|t38UDPRedundancy|t38UDPNoEC）、`T38FaxFillBitRemoval`、`T38FaxTranscodingMMR`、`T38FaxTranscodingJBIG`、`T38VendorInfo`。
  - 解析時要容忍以下變體：
    - 屬性名稱大小寫不同（例如 `T38faxVersion`）。
    - 布林屬性有兩種寫法：只有名稱，或帶 `:0` / `:1`。
- **T.38 answer 協商**
  - version 取雙方的最小值。
  - MaxBitRate 取雙方的最小值。
  - RateManagement 必須沿用 offer 的值。
  - UdpEC：若 offer 是 FEC 但我方不支援，降為 Redundancy。
  - MaxDatagram 和 MaxBuffer 填入我方的值。
  - 布林屬性取雙方都支援的交集。
- **拒絕某個 m-line**：保留該 m-line，但 port 設為 0（RFC 3264 6）。
- **audio 和 image 同時出現**：如果同一個 offer 同時有 audio 和 image，可以設定要接受哪一個，另一個以 port 0 拒絕。

## 7. Endpoint API（草案，實作時可微調，但要寫進 rustdoc）
```rust
let (endpoint, mut incoming) = Endpoint::bind(EndpointConfig { .. }).await?;
// UAS
while let Some(call) = incoming.recv().await {          // IncomingCall
    call.request();                                     // &Request
    call.ringing().await?;                              // 選用的 180
    let mut dlg = call.accept(answer_sdp).await?;       // DialogHandle：送 200，等 ACK
    // 或者 call.reject(StatusCode::BUSY_HERE).await?;
}
// DialogHandle
dlg.reinvite(offer_sdp).await?   -> ReinviteOutcome { Accepted(Sdp) | Rejected(StatusCode) | Timeout }
dlg.bye().await?
dlg.next_event().await           -> DialogEvent {
    ReInvite { offer: Option<Sdp>, responder: ReinviteResponder },
    Bye, AckTimeout, SessionExpired, Terminated(reason) }
// UAC（fax-sim 和未來的 fax-out 使用）
let mut dlg = endpoint.invite(InviteOptions { target, from, to, sdp, .. }).await?; // 自動送 ACK
```
- 以上所有 API 都必須是 `Send` 的，能在 multi-thread runtime 上使用。
- `Endpoint::shutdown()` 要結束所有任務，不能留下殘餘任務。

## 8. 從 sipress 抽取
細節見 `docs/sipress-extraction.md`。M1 抽取 `net.rs`、`take_frame`（含它的測試）、`DigestChallenge` 和 `authorization_header`。RTP 相關的部分延到 M2/M5。
