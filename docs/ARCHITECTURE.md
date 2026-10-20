# 架構

## Workspace 配置
```
crates/
  sip-core/     獨立 SIP 引擎（不依賴任何 fax crate，日後可拆成獨立專案）
  t38/          T.38 IFP（ASN.1 PER，v0–v3）+ UDPTL（redundancy / parity FEC）
  t30/          T.30 狀態機（先做接收端 Phase A–E，之後加 ECM）
  t4/           MH/MR/MMR 解碼驗證、TIFF Class F 寫出、PDF（CCITTFaxDecode 直接嵌入）
  fax-modem/    G.711 passthrough：CNG/CED、V.21 HDLC、V.27ter/V.29/V.17（後期）
  fax-in/       進線呼叫控制：SIP dialog ↔ T.38/音訊 ↔ T.30 ↔ 文件產出
  store/        DB 抽象（Repository trait）+ 各 DB 實作 + migrations
  delivery/     folder / email（lettre+rustls）/ webhook（reqwest+rustls），含重試佇列
  web/          axum REST + WebSocket 即時狀態 + rust-embed 內嵌 SPA
apps/sip-faxd/  單一執行檔：serve / tui / CLI 子命令
web-ui/         SPA 原始碼（Svelte + Vite）           ← M4 建立
tools/          獨立的 cargo workspace（不在根 workspace 內，因為需要 SpanDSP C 函式庫；根 Cargo.toml 以 exclude 排除）
  spandsp-sys/  手寫的最小 FFI（opaque pointer，pkg-config 連結 SpanDSP >= 3.1）            ← M0
  fax-sim/      T.38/音訊 loopback 執行器、測試頁 TIFF 產生器與獨立讀取器；`sip_uac` 模式（模擬 gateway 撥入 sip-faxd）← M0/M1d
  interop/      啟動真實 `sip-faxd` 並以 SIPp 3.8.0 驅動的測試 harness；場景在 `tools/sipp/*.xml`   ← M1d
                只在開發和 CI 使用，需在 Rocky 9 容器內以 scripts/e2e-docker.sh 執行
tests/e2e/      端到端整合測試                           ← M1 起
```

相依方向：
- `sip-core`、`t38`、`t30`、`t4`、`fax-modem` 是互相獨立的底層 crate。
- `fax-in` 使用上述底層 crate。
- `apps/sip-faxd` 整合 `fax-in`、`store`、`delivery`、`web`。

## sip-core 分層
| 層 | 內容 |
|---|---|
| Message | typed header map；compact form、多值與逗號分割；Via/Route/Record-Route；folding；嚴格 Content-Length |
| Transport | UDP 綁定監聽；TCP listener 加連線池（以 peer addr 為 key，閒置逾時）、`StreamDecoder`（沿用 sipress `take_frame`，含 RFC 5626 CRLF keep-alive）；收到 request 補 `received`/`rport`（RFC 3581）；回應依 Via 路由（RFC 3261 18.2.2）；送出 request 自動填頂端 Via 的 transport/sent-by；來源 IP 白名單（`transport/policy.rs`，drop / 403 / log-only）；以 channel（`Incoming`/`Outgoing`，`TransportHandle::detached`）與上層串接，可不開 socket 測試；TLS 預留介面 |
| Transaction | 四個純狀態機（`transaction/{server_invite,server_non_invite,client_invite,client_non_invite}.rs`，輸入事件、輸出 `Action` 與計時器；含 RFC 6026 Accepted 狀態）加 tokio driver；`TransactionLayer` 負責比對（17.1.3/17.2.3）、CANCEL（200/487/481）與 TU 介面（`TuInput`）。2xx 的重送與 ACK 由 TU（Dialog 層）負責，不在 transaction 內；ICT 在 Accepted 狀態把重送的 2xx 以 `TuEvent::Retransmitted2xx` 上報，Dialog 層據此重送快取的 ACK（ADR-011）|
| Registration | `registration.rs`：REGISTER client 任務（digest、到期前 `max(10%, 30s)` 刷新、指數退避、`watch` 狀態）|
| Dialog | `dialog/`：每個 dialog 一個 tokio task（`task.rs`）。UAS 建立 dialog 與 2xx 重送／AckTimeout（`uas.rs`）；in-dialog request 建構（loose／strict routing）；BYE、re-INVITE（glare 491、CSeq 檢查 500、未知 dialog 481）、OPTIONS／INFO／UPDATE；我方 re-INVITE（491 重試、應用層逾時 CANCEL、401/407 帶憑證重送一次）；session timer（`session_timer.rs`，RFC 4028）|
| SDP | 完整模型：多個 m-line、`m=image <port> udptl t38` 和全部 `T38Fax*` 屬性、port 0 拒絕規則、o= 版本號遞增 |
| API | `endpoint.rs`：`Endpoint::bind`／`detached`、`IncomingCall`（ringing／accept／reject）、`DialogHandle`／`DialogControl`（reinvite／bye／next_event）、`Endpoint::invite`（UAC，自動 ACK）、`shutdown()` 結束所有 task。Transport 層另提供選用的訊息 tap（`MessageTap`，預設關閉，不阻塞協定堆疊），是 M1d per-call 擷取的基礎。層與層之間以 tokio channel 串接（transport → transaction → dialog task → 應用層）|

## fax-in（M1d 骨架）
| 模組 | 內容 |
|---|---|
| `config` | TOML 設定（`[sip]`、`[media]`、`[call]`、`[capture]`、`[[trunks]]`）；trunk 先套 preset（`generic`、`cht-cs210`）；未知鍵一律報錯。範例見 `conf/sip-faxd.toml.example` |
| `trunk` | 來源 IP → trunk 對應（第一個符合者）、傳給 transport 的來源過濾（所有 trunk 聯集，`unknown_source` = drop / reject / log）|
| `controller` | `FaxIn`：每通來電一個 task。依 `t38_policy`（auto / wait-peer / initiate / audio-only）回 G.711 或 T.38、接受對方 re-INVITE 或在 ACK 後 `initiate_after_secs` 發我方 re-INVITE；488/415/606 → BYE 並記錄 `g711_passthrough_unsupported`。per-trunk 相容旗標（未知 `Require`、`min_max_datagram`）|
| `media` | 以 trunk 政策產生 SDP answer／T.38 offer；SDP 寬容度（缺 RateManagement、c= 層級、MaxDatagram 下限）與警告 |
| `session` | `FaxSession`／`FaxSessionFactory` trait；M1d 為只維持通話的 stub，M2 換成 T.38/T.30 |
| `event` | `CallEvent`、`CallResult`（trunk、T.38 協商參數、時間戳、結果／失敗原因），M3 寫入 DB |
| `capture` | 以訊息 tap 寫 `logs/calls/<call-id>/sip.log` 與 `result.json`；`keep = "failed"` 只留失敗通話 |

`sip-faxd serve --config <path>` 啟動 `FaxIn`，`sip-faxd config check` 驗證設定檔。

## fax-in 呼叫流程
1. 收到 INVITE（通常是 audio），回 100，再回 200 並帶 G.711 SDP（PCMU/PCMA）。
2. 等待 gateway 主動發 re-INVITE 切到 T.38。Sonus 和 AudioCodes 常見的做法就是由 gateway 發起。
3. 如果在設定的秒數內沒收到，或偵測到 CNG（1100Hz，以 Goertzel 偵測），由我方發 re-INVITE 切到 T.38。
4. 如果對方回 488 或 415，或逾時，改走 G.711 passthrough。在 fax-modem 完成之前，這種情況記錄為失敗並註明原因。
5. T.30 接收完成後，依序執行：
   1. 寫出 TIFF/PDF
   2. 寫入 DB
   3. 依 DID 路由規則投遞
   4. 發送 BYE，或處理對方送來的 BYE
6. 以下 gateway 相容參數都可以設定：
   - `T38FaxRateManagement`（transferredTCF / localTCF）
   - `T38FaxUdpEC`
   - `T38FaxMaxDatagram`
   - `T38FaxMaxBuffer`
   - IFP 冗餘深度
   - 是否由我方主動發起 re-INVITE，以及等待秒數

## store
- 以 async 的 `trait FaxRepository` 和 `ConfigRepository` 隔離各 DB 實作：

  | DB | 實作 |
  |---|---|
  | SQLite（bundled）、MySQL/MariaDB、PostgreSQL | sqlx（rustls） |
  | MSSQL | tiberius（rustls） |

- 各方言各有一套 migrations，由 `sip-faxd db migrate` 執行。
- 資料表：

  | 資料表 | 內容 |
  |---|---|
  | `inbound_faxes` | 狀態、caller/callee、頁數、速率、ECM、錯誤碼、檔案路徑、時間 |
  | `fax_routes` | DID 對應的投遞方式 |
  | `delivery_attempts` | 投遞嘗試紀錄 |
  | `settings` | 系統設定 |
  | `users` | 使用者帳號（密碼以 argon2 雜湊）|

## 應用程式 sip-faxd
- 子命令（clap）：

  | 子命令 | 說明 |
  |---|---|
  | `serve` | daemon，包含 SIP、Web、投遞 worker |
  | `tui` | ratatui 介面，透過本機 HTTP API 連到 daemon |
  | `status` | 查看狀態 |
  | `faxes list/show/export` | 查詢與匯出傳真 |
  | `routes ...` | 管理路由 |
  | `user add` | 新增使用者 |
  | `config check` | 檢查設定 |
  | `db migrate` | 執行 migrations |

- 目錄以執行檔所在位置為基準：
  - `./conf/sip-faxd.toml`
  - `./data/`：SQLite 和傳真檔
  - `./logs/`
- 日誌使用 `tracing`，包含 SIP 訊息 trace。

## 建置與部署
- 目標平台是 `x86_64-unknown-linux-musl` 和 `aarch64-unknown-linux-musl`，以 `cargo-zigbuild` 建置。
- 全面使用 rustls（禁止 OpenSSL），SQLite 使用 bundled 版本，在 musl 上改用 mimalloc。
- `scripts/package.sh` 會產生 `sip-fax-rs-<ver>-linux-<arch>.tar.gz`，內容如下：
  - 執行檔
  - `conf/`、`data/`、`logs/`
  - LICENSE 和 README
  - M6 會再加入 systemd unit 範例
- 打 tag `v*` 時，CI 會自動建立 GitHub Release。

## 測試架構
- **第 1 層，單元測試：**
  - parser 使用 RFC 4475 torture 測試向量
  - IFP PER、UDPTL FEC、T.30 狀態機（所有轉移的表格驅動測試）、T.4
  - 搭配 proptest 和 cargo-fuzz
- **第 2 層，互通測試：** 自製的 t38/t30 與 SpanDSP `t38_terminal` 直接以 IFP 互打，不經過 SIP。
- **第 3 層，端到端測試：** 由 `fax-sim` 撥進 `sip-faxd`。
  - 驗證收到的頁數和影像與來源 TIFF 一致。
  - 測試矩陣涵蓋：gateway 發起或我方發起 re-INVITE、ECM 開或關、各速率、封包遺失和亂序、UDP 和 TCP。
- **執行環境：** 在 Rocky Linux 9 的 Docker 中安裝 spandsp-devel 和 libtiff。CI 使用同一份 image。
