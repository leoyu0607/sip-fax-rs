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
  fax-sim/      T.38/音訊 loopback 執行器、測試頁 TIFF 產生器與獨立讀取器；M1 起加 SIP UAC   ← M0/M1
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
| Transport | UDP 綁定監聽（依 Via/rport 回應，RFC 3581）；TCP listener 多連線（沿用 sipress `take_frame`）；TLS 預留介面 |
| Transaction | INVITE server（100 Trying、2xx 以 T1/T2 重送直到 ACK、Timer H）；non-INVITE server（Timer J）；client transaction（我方發的 re-INVITE、BYE 使用，Timer A/B/E/F）|
| Dialog | UAS dialog：route set、remote target、雙向 CSeq、local tag；491 glare；session timer（RFC 4028）；OPTIONS 回應 |
| SDP | 完整模型：多個 m-line、`m=image <port> udptl t38` 和全部 `T38Fax*` 屬性、port 0 拒絕規則、o= 版本號遞增 |
| API | 以 tokio channel 分層（transport → transaction → TU），對外提供 `IncomingCall` 事件和 `Dialog` handle |

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
