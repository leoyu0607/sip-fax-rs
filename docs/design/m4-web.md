# M4 設計規格：設定入 DB、帳號、Web 介面、TUI、CLI、服務管理

- 撰寫：Opus（2026-10-21）
- 需求來源：`docs/GOALS.md`（原始需求 1、服務管理）、`docs/TODO.md` M4 段，以及 2026-10-21 使用者對下列問題的回答：
  - 帳號分「管理員」與「座席」兩種角色。
  - Web 由使用者選 HTTP 或 HTTPS；選 HTTPS 且沒有提供憑證時，自動產生自簽憑證（2026-10-22 改為**有效期 825 天、到期前 30 天自動更新**，因為 macOS／iOS 拒絕超過 825 天的憑證；ADR-056）。
  - 介面提供**繁體中文與英文**。
  - 清整天數、帳號、trunk／SIP 設定、投遞路由，以及原本設定檔中的大部分設定，都改存在 DB，可以在 Web 上修改。
- 實作者依本規格施作。要偏離時，先在報告中提出擬議 ADR，由 Opus 合併。

## 0. 原則
- **改設定不得影響進行中的通話。** 新設定只套用到之後的來電；需要重啟才能生效的項目，介面要明確標示。
- **DB 故障不得影響收傳真**（沿用 M3 §0）。設定在記憶體中有一份生效中的快照；DB 連不上時照舊使用快照，只是暫時無法修改。
- **後端是唯一的驗證者。** Web、TUI、CLI 都呼叫同一套驗證；前端的檢查只是為了提示。
- **秘密資料只寫不讀。** API 永遠不回傳密碼、HMAC secret、SIP digest 密碼，只回傳「已設定／未設定」。

## 1. 設定的分層

### 1.1 只留在設定檔的「啟動設定」
沒有這些設定就無法連 DB 或開 Web，所以仍放在 `conf/sip-faxd.toml`：

| 區段 | 內容 |
|---|---|
| `[log]` | level、log 檔與輪替（§7.4） |
| `[database]` | url、auto_migrate、max_connections |
| `[storage]` | data_dir |
| `[web]`（新增） | 見 §5.1 |
| `[service]`（新增） | pid 檔、控制 socket、`stop_timeout_secs`（§7） |

### 1.2 改存 DB 的「執行設定」
`[sip]`、`[media]`、`[call]`、`[capture]`、`[[trunks]]`（含 `t38`、`fax`、`numbers`、`compat`、`register`）、`[[routes]]`、`[delivery]`、`[retention]`。

### 1.3 生效方式
| 設定 | 生效 |
|---|---|
| `[[trunks]]` 的 profile、白名單、`t38`、`fax`、`numbers`、`compat` | **立即**，套用到下一通來電 |
| `[call]`、`[capture]` | 立即，下一通來電 |
| `[[routes]]`、`[delivery]` | 立即，下一份 finalize 的傳真；已建立的投遞嘗試沿用建立時的 target |
| `[retention]` | 立即，下一次清整 |
| `[trunks.register]` | 立即：該 trunk 的註冊重新開始（先 un-REGISTER 舊的） |
| `[sip]`（bind、advertised、transport）、`[media]`（port_range） | **需要重啟**。介面顯示「已儲存，重新啟動後生效」，`status` 也會列出待生效的變更 |

實作方式：執行設定在記憶體中是 `Arc<RuntimeConfig>`，以 `ArcSwap`（或 `tokio::sync::watch`）整份替換。每通來電在 INVITE 時取一次快照，整通電話都使用那份快照。

### 1.4 首次匯入與之後的優先順序
- 第一次以 M4 版本啟動、DB 中沒有任何執行設定時，把設定檔的執行設定**匯入 DB**，並記一筆稽核紀錄（`by = "config-file"`）。
- 之後**以 DB 為準**。設定檔中如果仍有這些區段，啟動時記 warning：「以下區段已改由資料庫管理，設定檔中的值不會生效」。
- `sip-faxd config import [--force]`：把設定檔的執行設定匯入 DB。沒有 `--force` 時，DB 已有設定就拒絕。
- `sip-faxd config export [--include-secrets]`：把 DB 中的執行設定輸出成 TOML。預設不含秘密資料，可以當作備份，也方便人工檢視。
- `sip-faxd config check` 同時檢查設定檔和 DB 中的執行設定。DB 連不上時只檢查設定檔，並明確說明。

### 1.5 資料表（migration 0002，四種 DB）
- `config_sections(name VARCHAR(64) PK, body TEXT, version INT, updated_by, updated_at)`
  - `body` 是該區段的 JSON，由 Rust 的 serde 型別定義與驗證。
  - 區段名稱：`sip`、`media`、`call`、`capture`、`delivery`、`retention`、`routes`（整份有序清單）、`trunk:<name>`（每個 trunk 一筆）。
  - `version` 做樂觀鎖：儲存時帶上讀到的 version，不一致就回 `Conflict`。
- `config_audit(id, section, action, before_json, after_json, by, at)`
  - 每次修改都記錄。秘密欄位以 `"***"` 取代後才寫入。
- M3 的 `settings` 表中的清整天數搬進 `retention` 區段（migration 搬資料），`settings` 表保留給其他零散設定。

### 1.6 秘密資料加密
- 第一次啟動時產生 `conf/secret.key`（32 bytes 隨機值，權限 0600）。
- DB 中的秘密欄位以 AES-256-GCM 加密，格式 `enc:v1:<base64(nonce||ciphertext)>`，使用 `ring`（已在依賴樹中）。
- 遺失 `secret.key` 時，秘密欄位無法解密：啟動時記 error，受影響的 trunk 註冊、Email 或 Webhook 會失敗，直到在介面重新輸入。其他功能照常。
- `config export --include-secrets` 輸出明文，只允許在本機 CLI 使用。

## 2. 帳號與權限

### 2.1 角色
| 能力 | 座席 `agent` | 管理員 `admin` |
|---|---|---|
| 看傳真列表、預覽、下載 | ✅ | ✅ |
| 切換已讀／未讀、編輯備註 | ✅ | ✅ |
| 看投遞狀態 | ✅ | ✅ |
| 手動重新投遞 | ❌ | ✅ |
| 修改任何設定 | ❌ | ✅ |
| 帳號管理、看稽核紀錄 | ❌ | ✅ |
| 看系統狀態（進行中的通話、註冊狀態） | ✅（唯讀） | ✅ |

### 2.2 資料表（migration 0002）
- `users(id, username VARCHAR(64) UNIQUE, display_name, role VARCHAR(8), password_hash, locale VARCHAR(8), disabled BOOL, must_change_password BOOL, created_at, updated_at, last_login_at)`
- `sessions(id, token_hash CHAR(64) UNIQUE, user_id FK, created_at, last_seen_at, expires_at, ip, user_agent)`
  - 只存 token 的 SHA-256。
  - 重新啟動後 session 仍然有效。
- 已讀／備註的 `*_by` 欄位改存 `username`（M3 已是字串）。

### 2.3 密碼與登入
- argon2id（`argon2` crate，參數：m=19 MiB、t=2、p=1，OWASP 建議值）。
- 密碼至少 10 個字元，不得與帳號相同。
- **登入失敗限制**：同一帳號連續失敗 5 次鎖 15 分鐘；同一來源 IP 每分鐘最多 20 次嘗試。帳號不存在時，回應時間與錯誤訊息都要和密碼錯誤相同。
- **Session**：cookie `sipfax_session`，`HttpOnly`、`SameSite=Strict`、HTTPS 時加 `Secure`。閒置 8 小時過期（`[web] session_idle_hours`），絕對上限 7 天。
- 登出、停用帳號、重設密碼時，刪除該使用者所有的 session。
- **CSRF**：所有會改變狀態的請求必須是 `Content-Type: application/json`，並帶 `X-Sipfax-Csrf` header（值等於登入時發給前端的 token），再加上 `SameSite=Strict`。

### 2.4 第一個管理員
- 啟動時如果 `users` 表是空的：
  - 建立 `admin`（角色 admin、`must_change_password = true`），密碼隨機產生 16 個字元；
  - 寫入 `conf/initial-admin-password`（0600），並在 log 中提示這個檔案的位置（**不把密碼寫進 log**）；
  - 第一次登入後強制改密碼，改完刪除該檔。
- 也可以用 CLI：`sip-faxd user add <name> --role admin|agent`（互動輸入密碼，或 `--password-stdin`）、`user passwd`、`user disable`、`user enable`、`user list`。

## 3. REST API（`/api/v1`）
- JSON；時間一律 RFC 3339 UTC；錯誤格式 `{"error": {"code": "<固定代碼>", "message": "<英文說明>", "fields": {...}}}`。前端依 `code` 翻譯，不顯示後端的英文訊息。
- 分頁沿用 M3 的 keyset（`cursor`、上限 200）。

| 方法與路徑 | 權限 | 說明 |
|---|---|---|
| `POST /auth/login`、`POST /auth/logout`、`GET /auth/me`、`POST /auth/password` | 公開／登入 | |
| `GET /faxes` | 登入 | 篩選：`is_read`、`status`、`from`、`to`、`ani`、`dnis`（前綴）、`trunk`、`q` |
| `GET /faxes/{id}` | 登入 | 含投遞嘗試列表 |
| `PUT /faxes/{id}/read` | 登入 | `{ "read": true }` |
| `PUT /faxes/{id}/note` | 登入 | `{ "text": "...", "expected_updated_at": "..." }`；衝突回 409 |
| `GET /faxes/{id}/pages/{n}.png?kind=full\|thumb` | 登入 | M3e 的 `Preview`；有 `ETag`、`Cache-Control: private` |
| `GET /faxes/{id}/file.pdf`、`file.tif` | 登入 | `Content-Disposition: attachment`；檔案已清整回 410 |
| `POST /faxes/{id}/redeliver` | admin | 對 `failed`／`giving_up` 的投遞重新排程 |
| `GET /config`、`GET /config/{section}` | admin | 秘密欄位以 `{"set": true}` 表示 |
| `PUT /config/{section}` | admin | body `{"version", "body"}`；回傳驗證錯誤（422，`fields` 以欄位路徑為 key）或新的 version；回傳 `restart_required`（本區段需要重啟的欄位）與 `pending_restart`；版本不符回 409（`current_version`） |
| `DELETE /config/trunk:{name}?version=N` | admin | 其他區段不能刪（405） |
| `PUT /config/trunks/order` | admin | `{"order": [trunk 名稱…]}`，必須是現有 trunk 的排列（M4b 新增：trunk 依此順序比對來源） |
| `GET /config/audit?limit=&before=` | admin | 新到舊；`next` 是下一頁的 `before` |
| `GET/POST /users`、`PUT/DELETE /users/{id}` | admin | 不能停用、降級或刪除最後一個啟用中的 admin（409 `last_admin`）；`PUT` 可帶 `password` 重設密碼（該帳號所有 session 失效，預設下次登入要改密碼） |
| `GET /status` | 登入 | `service`（版本、uptime、進行中的通話、各 trunk 註冊狀態）、`database`（`ok`／`unavailable`）、`config`（來源、待重啟的設定；admin 另有金鑰狀態與無法解密的秘密欄位） |

- 每個請求最多 30 秒（含讀 body），逾時回 408 `timeout`；串流中的下載不受限。header 30 秒內沒送完就關閉連線。

## 4. WebSocket（`/api/v1/events`）
- 以 session cookie 驗證；連線後伺服器推送 JSON 事件：
  - `fax.created`（`receiving`）、`fax.progress`（頁數）、`fax.finalized`、`fax.read_changed`、`fax.note_changed`
  - `delivery.updated`
  - `status.changed`（通話數、註冊狀態）
- 每個事件帶單調遞增的 `seq`。斷線重連時前端帶 `since=<seq>`，伺服器從記憶體中的環狀緩衝（最近 1000 筆）補送；超出範圍時送 `resync`，前端重新查詢列表。
- 伺服器每 30 秒送 ping；60 秒沒有 pong 就關閉。
- 帳號被停用或登出時，伺服器關閉該使用者的所有連線。

## 5. HTTP / HTTPS

### 5.1 `[web]`
```toml
[web]
enabled = true
bind = "0.0.0.0:8443"
scheme = "https"                 # "http" 或 "https"
# cert = "conf/tls/web.crt"      # 不填時自動產生自簽憑證
# key  = "conf/tls/web.key"
tls_names = []                   # 自簽憑證額外的 SAN（主機名稱或 IP）
session_idle_hours = 8
```
- `scheme = "http"` 時，bind 不是 loopback 位址會在啟動時記 warning（密碼會以明文傳送）。
- 設定檔沒有 `[web]` 時 `enabled` 預設為 `false`（升級時不會突然多開一個 port）；`conf/sip-faxd.toml.example` 寫 `enabled = true`（ADR-056）。

### 5.2 自簽憑證
- `scheme = "https"` 且沒有設定 `cert`／`key`：如果 `conf/tls/self-signed.{crt,key}` 不存在，就用 `rcgen`（純 Rust）產生：
  - ECDSA P-256；**有效期 825 天**（macOS／iOS 拒絕更長的伺服器憑證，使用者信任了也一樣；ADR-056）；EKU `serverAuth`；
  - SAN：本機主機名稱、`localhost`、`127.0.0.1`、bind 的 IP（`0.0.0.0` 時列出本機所有非 loopback 的 IPv4/IPv6 位址）、`tls_names`；
  - 私鑰檔權限 0600。
- 已存在就沿用，不會每次重產。**自簽憑證**距離到期不到 30 天（或已過期、檔案損壞）時，啟動時自動重新產生，並記 warning（指紋改變，瀏覽器要重新信任）；**使用者提供的憑證**不會被更換，只記 warning。
- 啟動 log 印出憑證的 SHA-256 指紋，讓使用者在瀏覽器出現警告時比對。
- TLS 使用 rustls（`tokio-rustls` / `axum-server` 的 rustls 支援），只允許 TLS 1.2 以上。

### 5.3 安全 header
`Content-Security-Policy: default-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'`、`X-Content-Type-Options: nosniff`、`Referrer-Policy: same-origin`；HTTPS 時加 `Strict-Transport-Security: max-age=31536000`。

## 6. Web 前端（`web-ui/`）
- Svelte 5 + Vite + TypeScript（ADR-005）。產出的檔案以 `rust-embed` 嵌入 `web` crate。
- **建置**：`web-ui/dist` 不放進 repo。
  - `web` crate 在找不到 `dist` 時嵌入一頁「介面沒有建置」的提示頁，讓沒有 Node 的開發者仍然可以 `cargo build`、跑 API 測試。
  - 發佈用的建置（CI 的 `build-musl`、`scripts/package.sh`）**必須**先建置 `web-ui`；找不到 `dist` 就失敗。
- **i18n**：`web-ui/src/locales/{zh-TW,en}.json`。依使用者設定（`users.locale`）選語言，未設定時依瀏覽器語言，預設 zh-TW。
  - 測試檢查兩份語系檔的 key 完全相同。
  - 掛線原因依 `hangup_code` 翻譯，不顯示 DB 中的 `hangup_text`（那是 M3 寫入的中文說明）。
- **頁面**：
  1. 登入（含強制改密碼）。
  2. 傳真列表：未讀加粗、篩選、keyset 無限捲動；接收中的列即時顯示「接收中，第 N 頁」；新傳真出現時不打斷使用者目前的捲動位置（顯示「有 N 份新傳真」）。
  3. 傳真內容：縮圖列＋整頁預覽（可縮放）、下載 PDF／TIFF、已讀切換、備註編輯（衝突時提示「備註已被 X 修改」，顯示對方的版本）、投遞狀態；檔案已清整時顯示說明。**開啟傳真時不自動標記已讀**（沿用 M3 §5；由使用者按鈕切換）。
  4. 設定（admin）：SIP／媒體／通話／擷取、trunks（列表＋編輯）、投遞路由（可排序）、投遞參數、清整。每個表單顯示欄位驗證錯誤；需要重啟的欄位標示出來。
  5. 帳號（admin）、稽核紀錄（admin）、系統狀態。
- **無障礙**：可以只用鍵盤操作；表單有 label；顏色以外另有文字或圖示表示狀態。
- **CI**：新增 `web-ui` job（`npm ci`、`npm run lint`、`npm run check`（svelte-check）、`npm test`（vitest）、`npm run build`），並設為必要檢查。

## 7. 服務管理、TUI、CLI

### 7.1 本機控制介面
- Unix domain socket `run/sip-faxd.sock`（權限 0600，只有執行服務的帳號可以存取），協定為一行一個 JSON 的請求／回應。
- 提供：`status`、`stop {force, timeout}`、`calls`、`reload`（重新從 DB 載入執行設定）、`subscribe`（事件串流，給 TUI 使用）。
- 能存取 socket 的人就等同管理員，所以不需要登入。

### 7.2 `start` / `stop` / `restart` / `status` / `logs`
依 `docs/TODO.md` M4 段的需求，補充：
- `start`：以 `setsid` 重新執行自己（`serve --daemonized`），PID 檔 `run/sip-faxd.pid` 加 `flock`。等控制 socket 回應 `status` 才算就緒（逾時 30 秒則顯示 log 最後 50 行並回非 0）。之後在前景 tail log；Ctrl+C 只停止顯示，並提示「服務仍在背景執行，用 `sip-faxd stop` 停止」。`-d` 只啟動、不顯示 log。
- `stop`：透過控制 socket 要求停止。服務立即對新的 INVITE 回 503，等進行中的通話結束，最多 `stop_timeout_secs`（預設 300）；期間每 5 秒顯示剩下幾通。`--force` 立即結束通話，已收到的頁面照樣寫出並標記 partial（`hangup_code = shutdown`）。投遞服務依 M3f 的流程關閉。
- `restart`：stop 後 start。
- `status`：顯示 §3 `/status` 的內容。服務沒有在執行時回非 0。
- `logs [-f] [-n N]`。
- 偵測到由 systemd 管理（`INVOCATION_ID` 環境變數，或 PID 檔指向的程序的父程序是 PID 1 且有 `NOTIFY_SOCKET`）時，`start`／`stop`／`restart` 提示改用 `systemctl`，並回非 0（`--ignore-systemd` 可略過）。
- `serve` 收到 SIGTERM 時，走和 `stop` 相同的流程。

### 7.3 TUI（`sip-faxd tui`）
- ratatui；透過控制 socket 連線（所以只能在伺服器本機使用）。
- 畫面：進行中的通話、最近的傳真（即時更新、可篩選未讀）、傳真詳細資料（欄位、投遞狀態）、切換已讀、編輯備註、系統狀態。不顯示影像。
- 介面語言跟隨 `LANG`（zh_TW 開頭用繁中，其他用英文），共用 Rust 端的語系字串。

### 7.4 CLI 補充與 log
- `sip-faxd fax list [--unread] [--status …] [--limit N]`、`fax show <id>`、`fax read <id> [--unread]`、`fax note <id> <text>`、`fax redeliver <id>`。
- `sip-faxd user …`、`config import/export`（§1.4、§2.4）。
- CLI 直接連 DB（不經過服務），操作者記為 `cli:<作業系統帳號>`。
- **Log 檔**：`logs/sip-faxd.log`，依大小（預設 50 MiB）或每天輪替，保留 `[log] keep`（預設 14）份，舊檔以 gzip 壓縮。前景 `serve` 仍然同時輸出到 stderr。

## 8. M3 留下來的項目（納入 M4）
- 手動重新投遞（§3 `redeliver`、CLI）。
- 預覽快取：與清整的競態（清整標記 `files_purged_at` 之後，`Preview` 不再寫入快取；寫完快取後再確認一次，若已清整就刪除）；容量上限 `[storage] preview_cache_mb`（預設 2048），超過時依最後存取時間淘汰。
- 投遞服務關閉時的寬限逾時、執行中 DB 重啟、非 UTC 時區的檔名範本。

## 9. 新增依賴（需要 ADR）
`axum`、`axum-server`（或 `tokio-rustls` 自行接 hyper）、`tower-http`、`rust-embed`、`argon2`、`rcgen`、`arc-swap`、`ratatui`＋`crossterm`、`flate2`（log 壓縮，M3e 已間接引入）。全部必須純 Rust 或只依賴 ring／aws-lc，`cargo deny` 通過，musl 靜態連結。

## 10. 分期
| 階段 | 內容 | 依賴 |
|---|---|---|
| M4a | 執行設定入 DB（schema、匯入／匯出、驗證、加密、熱套用）、帳號與 session、CLI（`config`、`user`、`fax`） | — |
| M4b | `web` crate：REST、WebSocket、HTTP/HTTPS 與自簽憑證、安全 header | M4a |
| M4c | `web-ui`：SPA、i18n、Playwright E2E | M4b 的 API（可先依 §3 同步開工） |
| M4d | 控制 socket、start/stop/restart/status/logs、log 輪替、TUI | M4a（TUI 的傳真操作） |
