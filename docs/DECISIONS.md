# 決策紀錄（ADR）

新增決策時，往檔案最後追加一則，格式為：背景 / 決策 / 影響。

## ADR-001：sipress 只抽取葉節點模組，SIP 堆疊重寫（2026-10-20）
**背景**

sipress-core 是為壓力測試設計的字串模板式 UAC 工具，有以下限制：
- 沒有訊息模型、transaction layer，也沒有監聽端 transport
- UAS 不會重送 2xx
- 發 BYE 時忽略 Record-Route
- 無法主動發出 re-INVITE
- 完全沒有 T.38

**決策**

| 處理方式 | 項目 |
|---|---|
| 抽取 | `net.rs`、`take_frame`、`DigestChallenge`、G.711 編解碼、`RtpPacket`（修正 decode） |
| 只作參考 | `message.rs` 的 UAS 回應鏡像、`ack_for_non2xx` |
| 重寫 | 其餘部分 |

詳見 `sipress-extraction.md`。

**影響**

M1 的工作量增加，但換來可以獨立成專案的乾淨 SIP 核心。

## ADR-002：傳真協定層以純 Rust 實作（2026-10-20）
**決策**

T.38 IFP/UDPTL、T.30、T.4/T.6 和 G.711 數據機全部自行實作。SpanDSP 只用在測試端。

**影響**
- 正式執行檔沒有 C 相依，靜態編譯單純。
- 數據機（M5）工程量大，排到最後。
- 以 SpanDSP 互通測試確保行為正確。

## ADR-003：DB 層採 sqlx + tiberius（2026-10-20）
**背景**

sqlx 0.7 之後不支援 MSSQL。

**決策**

| DB | 實作 |
|---|---|
| SQLite、MySQL/MariaDB、PostgreSQL | sqlx |
| MSSQL | tiberius |

兩者都以 Repository trait 隔離，各方言各有一套 migrations，並共用整合測試案例。

## ADR-004：靜態編譯採 musl + cargo-zigbuild + rustls（2026-10-20）
**決策**
- 目標平台：`x86_64`/`aarch64-unknown-linux-musl`
- 禁用 OpenSSL 和 native-tls，由 `deny.toml` 強制檢查
- SQLite 使用 bundled 版本
- 在 musl 上改用 mimalloc 彌補配置器效能

**驗證**

CI 的 `build-musl` 檢查執行檔是否 statically linked；`smoke-rhel` 在 Rocky 9 和 Alma 10 容器中解壓後執行。

## ADR-005：Web 採 axum + 內嵌 Svelte SPA（2026-10-20）
**決策**

SPA build 的產物以 rust-embed 打包進執行檔，部署時仍然只有一個 binary。

## ADR-006：測試治理與 CI 閘門（2026-10-20）
**決策**
- 每個里程碑都由 Opus 撰寫測試計畫，Sonnet 實作並執行，Opus 審核。
- 每次 push 都必須通過 CI。
- 必要檢查會隨里程碑逐步增加：e2e-spandsp、涵蓋率門檻、db-matrix、web-ui。
- fuzz 以 nightly 排程執行，不阻擋 push。

## ADR-007：測試用 SpanDSP 從原始碼建置，FFI 手寫不用 bindgen（2026-10-20）
**背景**

Rocky 9 的 EPEL 只提供 spandsp 0.0.6，沒有 T.38 terminal 與現行 T.30 API（需要 3.x）。

**決策**
- `docker/test-rocky9.Dockerfile` 從 upstream tag `v3.1.1` 建置 SpanDSP 並安裝到 `/usr/local`（共用函式庫在 `/usr/local/lib64`）。SpanDSP 只用於測試工具，不進入正式執行檔。
- `tools/spandsp-sys` 以手寫 FFI 取代 bindgen：所有 SpanDSP 狀態型別都當 opaque pointer，`*_init` 傳 NULL 讓 SpanDSP 自行配置，需要的函式只有約 30 個，因此不需要 libclang，也不必處理標頭版本差異。build.rs 以 `pkg-config` 探測 `spandsp >= 3.1`，找不到時指向 `scripts/e2e-docker.sh`。
- `tools/` 是獨立 cargo workspace（根 `Cargo.toml` 以 `exclude` 排除），使沒有 SpanDSP 的主機仍可建置主專案。

**代價**

簽名需人工對照標頭（已在容器內 grep 驗證 v3.1.1）；升級 SpanDSP 版本時要重新核對。S01–S10 的實際呼叫會在簽名不符時失敗。
