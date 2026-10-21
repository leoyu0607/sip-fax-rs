# sip-fax-rs

[English](README.md) | 繁體中文

[![CI](https://github.com/leoyu0607/sip-fax-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/leoyu0607/sip-fax-rs/actions/workflows/ci.yml)

以 Rust 開發的 SIP 傳真伺服器，主要功能：
- 接收來自 VoIP gateway（Sonus、AudioCodes 等）的傳真
- 預設使用 **T.38**，以 **G.711 passthrough** 為備援
- 結果存入資料庫，並可透過 Email、Webhook 或資料夾投遞

> **狀態：** 早期開發中。**已能透過 T.38 接收傳真**（里程碑 M2，待最後審核）：`sip-faxd serve` 接聽 gateway 的來電、切換到 T.38、接收多頁傳真（non-ECM、MH/MR、最高 14400 bit/s），並輸出成 TIFF 與 PDF。G.711 passthrough、ECM、資料庫、投遞與管理介面尚未完成。進度請見 [docs/ROADMAP.md](docs/ROADMAP.md)。

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

## 設定

`sip-faxd` 讀取 TOML 設定檔（預設為執行檔旁的 `conf/sip-faxd.toml`，可用 `--config` 指定）。[`conf/sip-faxd.toml.example`](conf/sip-faxd.toml.example) 說明所有選項：SIP 監聽、每個上游的 `[[trunks]]` 設定（內建 preset `generic` 與 `cht-cs210`、來源 IP 白名單、T.38 策略、選用的 REGISTER），以及每通來電的 SIP／UDPTL 擷取和傳真輸出目錄。

```sh
./sip-faxd config check --config conf/sip-faxd.toml   # 驗證設定
./sip-faxd serve --config conf/sip-faxd.toml          # 執行
```

## 接收傳真

每份收到的傳真會寫成：

```
data/faxes/<YYYY>/<MM>/<DD>/<call-id>.tif    多頁 TIFF Class F（CCITT G4）
data/faxes/<YYYY>/<MM>/<DD>/<call-id>.pdf    同樣頁面的 PDF
```

（日期為通話當天的 UTC 日期；目錄由 `[storage] data_dir` 設定，預設為執行檔旁的 `data/`）。傳到一半中斷的傳真，已收到的頁面仍會寫出，並在結果中標記 `"partial": true`。擷取功能開啟時（預設），`logs/calls/<call-id>/` 內有 `sip.log`（所有 SIP 訊息）、`udptl.log`（每個 T.38 封包一行：摘要，含十六進位的 T.30 frame 但不含影像資料；`[capture] udptl = "full"` 會記錄完整封包，也就是傳真內容）和 `result.json`（通話結果：T.30 結果碼、頁數、速率、解析度、編碼、對方 TSI、每頁 bad lines、檔案路徑、UDPTL 統計）。

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
