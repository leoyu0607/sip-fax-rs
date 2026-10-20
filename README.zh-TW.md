# sip-fax-rs

[English](README.md) | 繁體中文

[![CI](https://github.com/leoyu0607/sip-fax-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/leoyu0607/sip-fax-rs/actions/workflows/ci.yml)

以 Rust 開發的 SIP 傳真伺服器，主要功能：
- 接收來自 VoIP gateway（Sonus、AudioCodes 等）的傳真
- 預設使用 **T.38**，以 **G.711 passthrough** 為備援
- 結果存入資料庫，並可透過 Email、Webhook 或資料夾投遞

> **狀態：** 早期開發中（里程碑 M0），尚未可用。進度請見 [docs/ROADMAP.md](docs/ROADMAP.md)。

## 功能（規劃中）

- **先做 fax-in**，fax-out 之後再做。
- 預設使用 **T.38**（UDPTL，含冗餘與 FEC），備援方案為 **G.711 passthrough**。
- 傳真協定層以**純 Rust** 實作，涵蓋：
  - T.38 IFP/UDPTL
  - T.30
  - T.4/T.6（MH/MR/MMR）
  - TIFF/PDF 輸出
- **資料庫：** 預設 SQLite，另外支援 MySQL/MariaDB、PostgreSQL、Microsoft SQL Server。
- **投遞：** 存檔加 DB 紀錄、Email（SMTP）、HTTP Webhook、資料夾。
- **介面：** 簡易 CLI、TUI、Web 管理介面（內嵌 SPA）。
- **部署：** 單一靜態編譯（musl）執行檔。在 RHEL 9/10 解壓即可執行，不需要另外安裝相依套件。

## 架構

專案分成三大模組：

| 模組 | Crate | 說明 |
|---|---|---|
| SIP 核心引擎 | `sip-core` | 獨立的 SIP 協定堆疊，涵蓋訊息、UDP/TCP 傳輸、RFC 3261 transaction、dialog、含 T.38 的 SDP。未來會拆成獨立專案。部分程式碼抽取自 [Sipress](https://github.com/leoyu0607/Sipress)。 |
| Fax-in | `fax-in`、`t38`、`t30`、`t4`、`fax-modem` | 進線呼叫控制與傳真協定層 |
| Fax-out | — | 未來 |

支援用的 crate：
- `store`：資料庫層
- `delivery`：Email、Webhook、資料夾投遞
- `web`：管理 API 與介面

最後由執行檔 `sip-faxd` 把以上元件整合起來。

詳細內容請見 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)。

## 快速開始（正式發佈後）

```sh
tar xzf sip-fax-rs-<version>-linux-x86_64.tar.gz
cd sip-fax-rs-<version>-linux-x86_64
./sip-faxd serve
```

## 開發

```sh
cargo build
cargo test --workspace
scripts/ci-local.sh        # 與 CI 相同的檢查，每次 push 前都要執行
```

測試時使用 [SpanDSP](https://github.com/freeswitch/spandsp) 模擬 gateway 進線傳真。SpanDSP 只用在測試工具（`tools/fax-sim`）中，不會連結進正式發佈的執行檔。每次 push 都必須通過 CI。

延伸閱讀：
- [CLAUDE.md](CLAUDE.md)：給 AI agent 和貢獻者的入口
- [docs/](docs/)：目標、架構、路線圖、待辦事項與決策紀錄

## 授權

[MIT](LICENSE)
