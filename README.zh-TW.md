# sip-fax-rs

[English](README.md) | 繁體中文

[![CI](https://github.com/leoyu0607/sip-fax-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/leoyu0607/sip-fax-rs/actions/workflows/ci.yml)

以 Rust 開發的 SIP 傳真伺服器，主要功能：
- 接收來自 VoIP gateway（Sonus、AudioCodes 等）的傳真
- 預設使用 **T.38**，以 **G.711 passthrough** 為備援
- 結果存入資料庫，並可透過 Email、Webhook 或資料夾投遞

> **狀態：** alpha，現場試接中（最新版：[v0.4.1-alpha](https://github.com/leoyu0607/sip-fax-rs/releases/tag/v0.4.1-alpha)）。**已能透過 T.38 接收傳真**，並已用真實的 AudioCodes gateway 驗證：`sip-faxd serve` 接聽來電、切換到 T.38、接收多頁傳真（MH/MR/MMR、最高 14400 bit/s；ECM 已支援但預設關閉），並輸出成 TIFF 與 PDF。每通電話都會**記錄到資料庫**（預設 SQLite；也支援 PostgreSQL、MySQL/MariaDB、MSSQL；`sip-faxd db check` 測試連線，`sip-faxd db copy` 把全部資料搬到另一個資料庫），包含 ANI/DNIS、掛線原因與狀態；檔案與紀錄依設定的天數自動清整；傳真可以**投遞**到資料夾、Email 或 Webhook，失敗會持久化重試。執行檔內建 **Web 介面**（HTTPS，port 8443，繁體中文與英文）：即時列出收進來的傳真、預覽與下載、已讀與備註，管理員可以修改設定（存在資料庫，不必重啟就套用；設定檔只留啟動設定）與帳號；也可以用 `sip-faxd start|stop|status|logs` 與 TUI 在終端機管理服務。尚未提供：G.711 passthrough（M5）。詳見 [CHANGELOG.md](CHANGELOG.md) 與 [docs/ROADMAP.md](docs/ROADMAP.md)。

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

設定檔（預設為執行檔旁的 `conf/sip-faxd.toml`，可用 `--config` 指定）**只放啟動設定**：`[database]`、`[web]`、`[storage]`、`[log]`、`[service]`（[`conf/sip-faxd.toml.example`](conf/sip-faxd.toml.example)）。使用內建的 SQLite 時什麼都不用改。其他資料庫分欄位填寫，密碼原樣填，不用百分比編碼，也可以從檔案讀：

```toml
[database]
type = "mssql"                  # sqlite | postgres | mysql | mariadb | mssql
host = "10.1.2.3"
name = "faxdb"
user = "fax"
password = "p@ss:w/rd#1"        # 或 password_file = "conf/db-password"
trust_server_certificate = true
```

```sh
./sip-faxd config check     # 驗證設定檔（密碼以 *** 顯示）
./sip-faxd db check         # 測試資料庫連線：伺服器版本、資料表版本；不做任何修改
./sip-faxd serve            # 在前景執行（或用下面的 start／stop／status）
```

**其餘設定都存在資料庫，在 Web 介面的「設定」修改**：SIP 監聽與對外位址、每個上游的中繼（內建 preset `generic` 與 `cht-cs210`、來源 IP 白名單、T.38 策略、註冊、傳真能力、號碼來源）、通話處理、每通來電的擷取、投遞路由與保存期限（ADR-051、ADR-060）。第一次啟動以預設值建立，沒有任何中繼：新增中繼之前所有來電都會被拒絕（Web 介面與 log 都會提示）。修改後不必重啟，下一通來電就套用；只有監聽位址、UDPTL port 範圍與擷取需要重啟。秘密資料（註冊、Email、Webhook 的密碼）以 `conf/secret.key` 加密後存放。舊的設定檔裡若還有這些區段照樣可用：資料庫還沒有設定時匯入一次，之後可以刪除。

```sh
./sip-faxd config export [--include-secrets]      # 把資料庫中的設定輸出成 TOML（備份、範本）
./sip-faxd config import [--force] --file PATH    # 以腳本設定（conf/runtime.toml.example 說明每個選項）
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

- **`run/` 必須在本機檔案系統上**：`flock` 在某些網路檔案系統（以及 Docker Desktop 的 bind mount）上不會真的上鎖，這時執行中的服務會被誤判為沒有在執行。

### TUI

```sh
./sip-faxd tui                # 在伺服器本機執行
```

顯示最新的傳真列表（即時更新）、傳真詳細資料與投遞紀錄、進行中的通話，以及服務狀態。按鍵：`Tab` 在傳真、通話、狀態之間切換；`↑`／`↓` 或 `j`／`k` 移動；`Enter` 開啟詳細資料，`Esc` 返回；`u` 只顯示未讀；`r` 把選取的傳真設為已讀或未讀；`n` 編輯備註（`Enter` 儲存、`Esc` 取消）；`q` 離開。傳真資料直接讀寫 DB（操作者記為 `cli:<作業系統帳號>`，與 `fax` 指令相同），通話與狀態來自控制 socket；服務沒有執行時，TUI 會顯示提示，但仍可操作 DB。介面語言跟隨 `LANG`（`zh_TW` 開頭用繁中，其他用英文）。

這些指令需要 unix 系統（Linux；macOS 可用於測試），因為用到 flock、setsid 與 unix socket；其他系統只能使用 `serve`。

## Web 介面

預設開啟（設定檔沒有 `[web]` 也會啟動）：HTTPS，監聽 `0.0.0.0:8443`。沒有提供自己的憑證時，第一次啟動會在 `conf/tls/` 產生自簽憑證（有效期 825 天，到期前 30 天自動更新）；啟動 log 會印出 SHA-256 指紋，可以和瀏覽器的警告比對。第一次啟動時 `admin` 的密碼在 `conf/initial-admin-password`，第一次登入時必須修改。要關閉請設定 `[web] enabled = false`；位址、`http`、自己的憑證與 session 時間見 `conf/sip-faxd.toml.example` 的 `[web]`。以瀏覽器開啟 `https://<伺服器>:8443/`：傳真列表（即時顯示接收進度）、傳真內容（預覽、下載 PDF／TIFF、已讀、備註、投遞），管理員另有設定、帳號與稽核紀錄；介面提供繁體中文與英文。API 在 `/api/v1`。各頁面的說明見 [docs/web.md](docs/web.md)。從原始碼建置時，請先在 `web-ui/` 執行 `npm ci && npm run build` 再 `cargo build`，否則執行檔只會顯示一頁說明，沒有介面。

收到傳真時，每頁的 PNG 預覽會寫在 TIFF、PDF 旁邊（`<名稱>-p1.png`、`<名稱>-p1-thumb.png`……），清整時一起刪除。
## 傳真紀錄與資料庫

每通來電在 `[database]` 指定的資料庫的 `faxes` 資料表有一筆紀錄（預設為 SQLite 檔案 `data/sip-faxd.db`；也可用 PostgreSQL、MySQL／MariaDB、SQL Server，見 [docs/database.md](docs/database.md)）。通話開始時就建立紀錄（`fax_status = receiving`），對方每送完一頁，`pages` 就加一，通話結束時補齊：狀態（`received`、`partial`、`failed`、`no_fax`）、誰掛斷與原因（`hangup_direction`、`hangup_code`）、進線號碼（ANI）與撥打號碼（DNIS）、檔案路徑與大小，以及協商出來的解析度、速率、編碼和 ECM。來源不在任何 trunk 白名單的電話會被擋掉，不寫入紀錄。

**資料庫出問題不會害傳真收不到。** 傳真檔和 `result.json` 照常寫出；寫資料庫的動作放在記憶體佇列，以指數退避重試（1 秒、2 秒、4 秒…最長 60 秒）。仍然失敗的寫入會記成 error，之後可用 `sip-faxd db backfill` 從 `logs/calls/*/result.json` 補回。啟動時資料庫連不上，伺服器照樣啟動；但資料庫版本**比程式新**（或 `auto_migrate = false` 而版本過舊）時會拒絕啟動。

清整（Web 介面的「設定 → 保存期限」）：每天在 `run_at`（伺服器本地時間）執行：超過 `files_days` 的 TIFF／PDF 檔案刪除（紀錄保留，標記「檔案已清除」），超過 `records_days` 的紀錄刪除。`0` 代表永遠不刪。

| 指令 | 作用 |
|---|---|
| `sip-faxd db migrate` | 建立或升級資料表結構（多台同時執行也安全；MySQL/MariaDB 的 migration 失敗後可以重跑） |
| `sip-faxd db backfill [--dir DIR]` | 把 `logs/calls/*/result.json` 裡資料庫沒有的通話補進去 |
| `sip-faxd purge [--dry-run]` | 立刻執行清整；`--dry-run` 只計數、不刪除 |
| `sip-faxd db check [--url URL [--password-stdin]]` | 測試連線：伺服器版本、資料表版本；不做任何修改 |
| `sip-faxd db copy --to URL`（或 `--to-config 檔案`） | 換資料庫，也可以換成另一種（例如 SQLite 改用 SQL Server）：全部資料表連同 id 複製到空的資料庫；服務必須先停止 |

## 投遞

設定了投遞路由時（Web 介面的「設定 → 投遞路由」；下方是 `config import` 的檔案格式），每份結果為 `received` 的傳真，在紀錄寫入資料庫後立刻投遞（中途斷線的 `partial` 只有路由設 `deliver_partial = true` 才投遞；`failed`、`no_fax` 永不投遞）。路由依序比對，**第一條符合的**生效；條件為 `dnis`（完全相同）、`dnis_prefix`、`trunk`（寫了幾個就要全部符合），或 `default = true`（其餘全部，必須是最後一條）。每條路由有一個以上的投遞：

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
