# sip-fax-rs

[English](README.md) | 繁體中文

[![CI](https://github.com/leoyu0607/sip-fax-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/leoyu0607/sip-fax-rs/actions/workflows/ci.yml)

以 Rust 開發的 SIP 傳真伺服器，主要功能：
- 接收來自 VoIP gateway（Sonus、AudioCodes 等）的傳真
- 預設使用 **T.38**，以 **G.711 passthrough** 為備援
- 結果存入資料庫，並可透過 Email、Webhook 或資料夾投遞

> **狀態：** alpha，現場試接中（最新版：[v0.2.0-alpha](https://github.com/leoyu0607/sip-fax-rs/releases/tag/v0.2.0-alpha)）。**已能透過 T.38 接收傳真**，並已用真實的 AudioCodes gateway 驗證：`sip-faxd serve` 接聽來電、切換到 T.38、接收多頁傳真（MH/MR/MMR、最高 14400 bit/s；ECM 已支援但預設關閉），並輸出成 TIFF 與 PDF。每通電話都會**記錄到資料庫**（預設 SQLite；也支援 PostgreSQL、MySQL/MariaDB、MSSQL），包含 ANI/DNIS、掛線原因與狀態；檔案與紀錄依設定的天數自動清整；傳真可以**投遞**到資料夾、Email 或 Webhook，失敗會持久化重試。尚未提供：管理介面（M4）、G.711 passthrough（M5）。詳見 [CHANGELOG.md](CHANGELOG.md) 與 [docs/ROADMAP.md](docs/ROADMAP.md)。

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
./sip-faxd config check --config conf/sip-faxd.toml   # 驗證設定（會顯示資料庫、清整、傳真與號碼設定）
./sip-faxd serve --config conf/sip-faxd.toml          # 執行
```

除了 SIP 相關設定，設定檔還有 `[database]`（`url`、`auto_migrate`、`max_connections`）、`[retention]`（`files_days`、`records_days`、`run_at`）、`[[routes]]` 與 `[delivery]`（見[投遞](#投遞)），以及每個 trunk 的 `[trunks.fax]`（`resolutions`、`ecm`）和 `[trunks.numbers]`（進線號碼與撥打號碼取自哪個 header：`ani_sources`、`dnis_sources`、`strip_plus`、`ani_strip_prefix`、`dnis_strip_prefix`）。

**執行設定改存資料庫（M4a、ADR-051）。** 只有 `[log]`、`[database]`、`[storage]`（之後還有 `[web]`、`[service]`）永遠從設定檔讀取。`[sip]`、`[media]`、`[call]`、`[capture]`、`[[trunks]]`、`[[routes]]`、`[delivery]`、`[retention]` 在第一次啟動時匯入資料庫，之後以資料庫為準（設定檔中不再生效的區段，啟動時會以 warning 列出）。修改後不必重啟，下一通來電就套用；只有監聽位址、UDPTL port 範圍與 `[capture]` 需要重啟。秘密資料（註冊、Email、Webhook 的密碼）以 `conf/secret.key` 加密後存放。

```sh
./sip-faxd config export [--include-secrets]      # 把資料庫中的執行設定輸出成 TOML（備份用）
./sip-faxd config import [--force] [--file PATH]  # 把設定檔的執行區段寫入資料庫
./sip-faxd user add alice --role agent            # 帳號：add | passwd | disable | enable | list
./sip-faxd fax list --unread                      # 傳真：list | show | read | note | redeliver
```

第一次啟動會建立管理員 `admin`，隨機密碼寫在 `conf/initial-admin-password`（第一次登入必須改密碼）。

## 以服務方式執行（Linux）

```sh
./sip-faxd start              # 在背景啟動，等服務就緒後顯示 log
./sip-faxd start -d           # 只啟動，不顯示 log
./sip-faxd status             # pid、版本、運行時間、監聽位址、進行中的通話數、trunk 註冊狀態
./sip-faxd logs -f -n 100     # 最後 100 行 log，之後持續顯示（log 輪替後也會跟上新檔）
./sip-faxd stop               # 等進行中的傳真收完再停止
./sip-faxd stop --force       # 立刻結束通話
./sip-faxd restart            # 先 stop（等通話結束），再 start
```

- **`start`**：以 `setsid` 在獨立的 session 執行 `sip-faxd serve --daemonized`，寫入 `run/sip-faxd.pid`（以 `flock` 鎖定，重複啟動會被拒絕並指出正在執行的 pid），並等到控制 socket `run/sip-faxd.sock` 回應才算就緒。Ctrl+C 只停止顯示 log（提示「服務仍在背景執行」），不會停止服務。若 `[service] start_timeout_secs`（30 秒）內沒有就緒，或啟動途中結束，`start` 會顯示 log 最後 50 行、回傳非 0，並且不留下 PID 檔。
- **`stop`**：優雅停止。新的 INVITE **立刻回 503**，**等進行中的傳真收完**（最多 `[service] stop_timeout_secs`，預設 300 秒；每 5 秒顯示一次進度），逾時才結束剩下的通話：已收到的頁面照樣寫出，紀錄標記為 `partial`、`hangup_code = shutdown`。`--force` 不等待，`--timeout 秒數` 暫時覆蓋設定值。接著依原本的流程關閉投遞服務（見[投遞](#投遞)）。對 `serve` 送 `SIGTERM` 與 `stop` 相同；再送一次 `SIGTERM` 或 Ctrl+C 就是強制。
- **`status`**：服務在執行時回傳 `0`，沒有在執行時回傳 `3`。
- **systemd**：unit 裡請使用前景的 `serve`（`ExecStart=/opt/sip-faxd/sip-faxd serve`，`TimeoutStopSec=` 要大於 `stop_timeout_secs`）。偵測到由 systemd 管理（有 `INVOCATION_ID`，或 PID 檔指向的程序是 PID 1 的子程序且有 `NOTIFY_SOCKET`）時，`start`、`stop`、`restart` 會提示改用 `systemctl ...` 並回傳非 0；加 `--ignore-systemd` 可略過。
- **Log 檔**：`logs/sip-faxd.log`，依大小（`[log] max_size_mb`，預設 50）或每天輪替，保留 `[log] keep`（14）份舊檔並以 gzip 壓縮。前景的 `serve` 同時輸出到檔案與 stderr。
- **控制 socket**：一行一個 JSON（`status`、`calls`、`stop`、`reload`、`subscribe`），權限 0600；能開啟它的人就等同管理員。TUI 與修改設定後的 `reload` 都使用它。

這些指令需要 Linux（flock、setsid、unix socket）；其他系統只能使用 `serve`。

## 傳真紀錄與資料庫

每通來電在 `[database] url` 指定的資料庫的 `faxes` 資料表有一筆紀錄（預設為 SQLite 檔案 `data/sip-faxd.db`；也可用 `postgres://`、`mysql://`／`mariadb://`、`mssql://`，見 [docs/database.md](docs/database.md)）。通話開始時就建立紀錄（`fax_status = receiving`），對方每送完一頁，`pages` 就加一，通話結束時補齊：狀態（`received`、`partial`、`failed`、`no_fax`）、誰掛斷與原因（`hangup_direction`、`hangup_code`）、進線號碼（ANI）與撥打號碼（DNIS）、檔案路徑與大小，以及協商出來的解析度、速率、編碼和 ECM。來源不在任何 trunk 白名單的電話會被擋掉，不寫入紀錄。

**資料庫出問題不會害傳真收不到。** 傳真檔和 `result.json` 照常寫出；寫資料庫的動作放在記憶體佇列，以指數退避重試（1 秒、2 秒、4 秒…最長 60 秒）。仍然失敗的寫入會記成 error，之後可用 `sip-faxd db backfill` 從 `logs/calls/*/result.json` 補回。啟動時資料庫連不上，伺服器照樣啟動；但資料庫版本**比程式新**（或 `auto_migrate = false` 而版本過舊）時會拒絕啟動。

清整：`[retention]` 只在**第一次**啟動時寫入資料庫，之後以資料庫為準（之後版本的 Web 介面會在資料庫修改）。每天在 `run_at`（伺服器本地時間）執行：超過 `files_days` 的 TIFF／PDF 檔案刪除（紀錄保留，標記「檔案已清除」），超過 `records_days` 的紀錄刪除。`0` 代表永遠不刪。

| 指令 | 作用 |
|---|---|
| `sip-faxd db migrate` | 建立或升級資料表結構（多台同時執行也安全；MySQL/MariaDB 的 migration 失敗後可以重跑） |
| `sip-faxd db backfill [--dir DIR]` | 把 `logs/calls/*/result.json` 裡資料庫沒有的通話補進去 |
| `sip-faxd purge [--dry-run]` | 立刻執行清整；`--dry-run` 只計數、不刪除 |

## 投遞

設定檔有 `[[routes]]` 時，每份結果為 `received` 的傳真，在紀錄寫入資料庫後立刻投遞（中途斷線的 `partial` 只有路由設 `deliver_partial = true` 才投遞；`failed`、`no_fax` 永不投遞）。路由依序比對，**第一條符合的**生效；條件為 `dnis`（完全相同）、`dnis_prefix`、`trunk`（寫了幾個就要全部符合），或 `default = true`（其餘全部，必須是最後一條）。每條路由有一個以上的投遞：

```toml
[[routes]]
dnis_prefix = "2557"
[[routes.deliveries]]
kind = "folder"                     # PDF 與 TIFF，原子性寫入
dir = "/srv/fax-inbox"
filename = "{date}_{ani}_{id}"
[[routes.deliveries]]
kind = "email"                      # 附 PDF；STARTTLS（預設）、SMTPS 或 none；支援 AUTH
host = "smtp.example.com"
username = "fax"
password = "secret"
from = "fax@example.com"
to = ["ops@example.com"]
[[routes.deliveries]]
kind = "webhook"                    # POST JSON；可選 HMAC-SHA256 簽章與上傳 PDF
url = "https://example.com/fax-hook"
secret = "hmac-key"                 # X-Sipfax-Signature: sha256=<body 的 hex>
```

* 投遞失敗會在 1、5、15、60 分鐘後重試，之後每 4 小時一次，最多 `[delivery] max_attempts`（預設 10）次，用盡就放棄。佇列就是 `delivery_attempts` 資料表（每次嘗試一列），所以重新啟動會從中斷處繼續。投遞出問題、或資料庫連不上，都不影響收傳真，也不改傳真紀錄。投遞是 at-least-once（至少一次）。
* `[delivery] catch_up_hours`（預設 24）：啟動時，把這段時間內已結束、還沒有投遞的傳真補排進佇列。用來修復「已存檔、還沒排入佇列」之間當機的情形，也會投遞 `sip-faxd db backfill` 補回的通話（backfill 本身從不投遞，所以補回舊紀錄不會灌爆信箱；超過這個時間範圍的通話不會投遞）。`0` 代表關閉。
* `sip-faxd config check` 會列出路由；密碼、Webhook secret、URL 裡的帳密與 query 值一律顯示為 `***`。
* 永久性失敗（HTTP 4xx 但不含 408/425/429、SMTP 5xx、目的地設定錯誤）不會重試；排除原因後需由管理員重新投遞（之後的版本會提供按鈕）。

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
scripts/e2e-docker.sh      # 在 Rocky 9 容器內跑 SpanDSP 端到端測試（Linux、macOS、Windows 皆可）
scripts/db-matrix.sh       # 在容器內以 PostgreSQL、MySQL、MariaDB、MSSQL 跑 store 測試
scripts/musl-docker.sh     # 在容器內做靜態 musl 建置（zig 0.14.1）；ci-local.sh 需要時會自動呼叫
```

測試時使用 [SpanDSP](https://github.com/freeswitch/spandsp) 模擬 gateway 進線傳真。SpanDSP 只用在測試工具（`tools/fax-sim`）中，不會連結進正式發佈的執行檔。每次 push 都必須通過 CI。

延伸閱讀：
- [CLAUDE.md](CLAUDE.md)：給 AI agent 和貢獻者的入口
- [docs/](docs/)：目標、架構、路線圖、待辦事項與決策紀錄

## 授權

[MIT](LICENSE)
