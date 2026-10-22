# M4f 測試計畫：資料庫分欄位設定、`db check`、設定檔只留啟動設定

- 撰寫：Opus（2026-10-22）
- 執行：Sonnet
- 審核：Opus
- 需求（2026-10-22 使用者決定）：
  1. 資料庫帳密不必自己做百分比編碼：`[database]` 可以分欄位填寫，密碼原樣填，或放在 `password_file`。
  2. 有一個單純測試資料庫連線的指令。
  3. 設定檔只放啟動設定；存在資料庫的設定不再出現在設定檔與範例中，在 Web 修改。舊設定檔中的這些區段**保留相容匯入**（資料庫還沒有執行設定時匯入一次）。

## 0. 設計摘要（ADR-060）

### 0.1 `[database]` 的兩種寫法

```toml
[database]
type = "mssql"            # sqlite | postgres | mysql | mariadb | mssql
host = "10.1.2.3"
port = 1433               # 省略時：postgres 5432、mysql/mariadb 3306、mssql 1433
name = "faxdb"
user = "sa"
password = "p@ss:w/rd#1"  # 原樣填寫
# password_file = "conf/db-password"   # 與 password 擇一；檔案第一行，去掉結尾換行
trust_server_certificate = true        # 只有 mssql
# SQLite：type = "sqlite"、path = "data/sip-faxd.db"
```

- `url = "…"` 照舊可用；`url` 與 `type` 同時出現是錯誤。兩者都沒有時預設 `sqlite://data/sip-faxd.db`。
- 程式自己組 URL：`user`、`password` 以百分比編碼放進 userinfo，`name` 以路徑片段編碼。
- 檢查：`type` 不認得、伺服器資料庫缺 `host` 或 `name`、`password` 與 `password_file` 同時出現、`password_file` 讀不到、`trust_server_certificate` 用在 mssql 以外、sqlite 填了 host／user／password，都在啟動（或 `config check`）時以 `[database] <欄位>: <原因>` 報錯。`password_file` 權限比 0600 寬時給 warning。
- 相對路徑（`path`、`password_file`）相對於執行檔所在目錄，與其他設定相同。
- 所有顯示 URL 的地方（log、`config check`、錯誤訊息、`db copy`）密碼都是 `***`。

### 0.2 `sip-faxd db check`

- 只連線、不建立或升級資料表。輸出：資料庫類型、主機（遮蔽密碼）、伺服器版本、schema 版本與執行檔需要的版本（相符／較舊要 `db migrate`／較新要升級執行檔）、連線耗時。
- 連不上時回非 0，並依原因給提示：連不到主機（網路、防火牆、port）、帳密錯誤、資料庫不存在、TLS 憑證不被信任（mssql 提示 `trust_server_certificate`）、逾時。
- 參數：`--config`（預設）；`--url <URL>` 測試另一個資料庫；`--password-stdin`（與 `--url` 一起，URL 不含密碼，從 stdin 讀一行，不必編碼）。

### 0.3 `db copy` 的目標

- `--to <URL>` 照舊；新增 `--to-password-stdin`（`--to` 的 URL 不含密碼）與 `--to-config <檔案>`（從另一個設定檔的 `[database]` 取目標，分欄位寫法可用）。三者的組合錯誤（同時給 `--to` 與 `--to-config` 等）要報錯。

### 0.4 設定檔只留啟動設定

- `conf/sip-faxd.toml.example` 只有 `[log]`、`[service]`、`[web]`、`[database]`、`[storage]`。
- 資料庫還沒有執行設定、設定檔也沒有執行設定區段時：以預設值建立執行設定（沒有中繼），服務照常啟動；啟動 log warning「沒有設定中繼，所有來電都會被拒絕；請在 Web 的 設定 → 中繼 新增」。
- 相容：設定檔仍有執行設定區段、資料庫還沒有時，照舊匯入一次。資料庫已有時，啟動 log 與 `config check` 提示「這些區段已不使用（設定在資料庫），可以從設定檔刪除」。
- Web：管理員登入後，沒有任何中繼時，傳真列表上方顯示提示與前往 設定 → 中繼 的連結；座席顯示「尚未設定線路，請聯絡管理員」。

## 1. 案例

| ID | 案例 | 預期 |
|---|---|---|
| D01 | 分欄位與 URL 等價：四種伺服器類型各一組（含 port 預設值、`trust_server_certificate`）與 sqlite `path` | 組出的 URL 與手寫的等價 URL 相同；以 `store::backend_for_url` 解析出正確類型 |
| D02 | 密碼含 `@ : / ? # % & + 空白 中文 emoji` 等字元，分欄位寫法 | 在 db-matrix 的伺服器資料庫上，以這樣的密碼建立的帳號真的可以連線（PostgreSQL、MySQL、MSSQL 各建一個帳號） |
| D03 | `password_file`：一般、結尾有 `\n`／`\r\n`、空檔、不存在、權限 0644 | 前兩者成功；空檔與不存在報錯並指出檔名；0644 成功但有 warning |
| D04 | 錯誤組合：`url`＋`type`、`password`＋`password_file`、未知 `type`、伺服器類型缺 `host`／`name`、mssql 以外有 `trust_server_certificate`、sqlite 有 `host`、未知欄位 | 每個都報錯，訊息指出欄位；服務不啟動 |
| D05 | 密碼不外洩：分欄位密碼在 `config check`、啟動 log、連線失敗的錯誤、`db copy`、`db check` 的輸出 | 只出現 `***`，原文與編碼後的密碼都不出現 |
| K01 | `db check` 成功：SQLite 與 db-matrix 的四種伺服器資料庫 | 回 0；輸出類型、版本、schema「相符」 |
| K02 | `db check` 的 schema：空資料庫（未建表）、較舊、較新 | 分別提示 `db migrate`／`db migrate`／升級執行檔；較新回非 0；**資料庫沒有被修改**（不建立 `schema_migrations`） |
| K03 | `db check` 失敗原因：埠沒開、主機名稱解析不到、密碼錯、資料庫不存在、mssql 自簽憑證未信任 | 回非 0；訊息分類正確並有提示；不卡住超過 15 秒 |
| K04 | `db check --url … --password-stdin` | 以 stdin 的密碼連線成功；URL 中已有密碼時報錯 |
| P01 | `db copy --to-password-stdin`、`--to-config`（分欄位）| 複製成功，與 `--to <完整 URL>` 結果相同 |
| P02 | `db copy` 參數組合錯誤 | 報錯，目標沒有被寫入 |
| F01 | 範例設定檔：`conf/sip-faxd.toml.example` | 只有啟動設定區段；`config check` 通過；以它第一次啟動成功 |
| F02 | 新安裝：沒有執行設定的設定檔＋空資料庫 | 服務啟動；資料庫有預設的執行設定、沒有中繼；log 有「沒有設定中繼」warning；來電被拒（依 `unknown_source`）；在 Web 新增中繼後，**不重啟**就能收傳真（Docker 內以 SpanDSP 送一份） |
| F03 | 相容：0.3.0 的設定檔（含執行設定）＋空資料庫 | 匯入一次，與 0.3.0 行為相同 |
| F04 | 相容：含執行設定的設定檔＋已有執行設定的資料庫 | 不匯入；log 與 `config check` 列出可刪除的區段 |
| F05 | Web 的「沒有中繼」提示：admin 與 agent 各看到對應的文字；新增中繼後提示消失（不重新整理）；兩種語言 | 正確（Playwright） |

## 2. 驗收

- 以上案例通過；D02、K01 在 db-matrix 的四種伺服器資料庫上通過。
- 涵蓋率維持：`store` ≥ 80%（排除伺服器驅動）、`web` ≥ 80%、前端 statements ≥ 70%。
- `conf/sip-faxd.toml.example`、`docs/database.md`、`docs/field-test.md`、`docs/manual/it.md`、`docs/web.md`、兩份 README、CHANGELOG（中英）同步更新。
