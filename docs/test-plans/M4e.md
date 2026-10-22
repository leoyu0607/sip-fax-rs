# M4e 測試計畫：資料庫搬移 `sip-faxd db copy`

- 撰寫：Opus（2026-10-22）
- 執行：Sonnet
- 審核：Opus
- 需求：2026-10-22 使用者同意新增「從一種資料庫搬到另一種」的功能（例如 SQLite 改用 MSSQL）；在此之前只能用 `config export/import` 加 `db backfill`，已讀、備註、投遞紀錄、稽核與帳號會遺失。

## 0. 設計摘要（ADR-059）

- 指令：`sip-faxd db copy --to <目標 URL> [--config <設定檔>]`。來源是設定檔 `[database] url`。
- **前提**：
  - 服務必須停止（以 PID 檔的 flock 判斷；執行中就拒絕，避免複製期間有新資料）。
  - 來源的 schema 版本必須等於執行檔的版本（較舊：先 `db migrate`；較新：拒絕）。
  - 目標與來源不能是同一個 URL。
  - 目標建立或升級 schema 後，七個資料表（`faxes`、`delivery_attempts`、`settings`、`config_sections`、`config_audit`、`users`、`sessions`）**必須都是空的**，否則拒絕，不動目標。
- **複製**：在目標的 migration session（一條專用連線，同時持有 migration 鎖）中、**單一交易**內，依外鍵順序（`faxes` → `delivery_attempts`；`users` → `sessions`；其餘獨立）逐表複製，每列的 `id` 原樣保留。來源以主鍵分頁讀取；寫入以多列 `INSERT` 分批（每批參數數不超過 2000，配合 MSSQL 的 2100 上限）。
  - PostgreSQL：`INSERT … OVERRIDING SYSTEM VALUE`，結束前以 `setval` 把每個 identity 推到 `MAX(id)`。
  - MSSQL：每個多列 `INSERT` 前後在同一個 batch 內 `SET IDENTITY_INSERT … ON/OFF`（參數化的語句以 `sp_executesql` 執行，自成一個 batch，分開送的 SET 不會生效）。
  - MySQL／MariaDB：`AUTO_INCREMENT` 自動跟上；交易以 `SET autocommit = 0`／`COMMIT`／`ROLLBACK` 包住（`START TRANSACTION` 不能以 prepared statement 執行）。
  - SQLite：migration session 本身就是一個 exclusive 交易。
- 任何一步失敗都 rollback，目標保持空的（schema 留著）。成功後在目標以 `COUNT(*)` 與 `MAX(id)` 核對每個表都等於來源，不符就回報失敗。
- 輸出：每個表複製的列數、耗時；最後提示「把 `[database] url` 改成目標，再啟動服務」。URL 中的密碼一律遮蔽。
- `schema_migrations` 不複製（目標的 migration 自己寫）。秘密欄位（`config_sections` 中的 `enc:v1:…`）原樣複製，**沿用同一份 `conf/secret.key`** 就能解密。

## 1. 共通規則

沿用 `docs/test-plans/M4.md` §0：DB 相關案例用 `db_case!` 或同等的矩陣，本機跑 SQLite，CI 的 `db-matrix` 跑四種伺服器 DB；不 mock store；每個拒絕的案例都要有反向斷言（目標沒有被寫入）。

## 2. 案例

| ID | 案例 | 預期 |
|---|---|---|
| C01 | SQLite → SQLite，七個表都有資料（含接收中、各種狀態、已讀、備註、投遞各狀態、稽核、停用帳號、session） | 目標每個表逐列、逐欄位與來源相同（以兩邊的原始查詢比對，不經 `FaxStore` 的 API），`id` 相同 |
| C02 | 矩陣：SQLite → PostgreSQL／MySQL／MariaDB／MSSQL，以及各伺服器 DB → SQLite | 同 C01；含 NULL、布林、毫秒時間、emoji 與 CJK、各欄位長度上限的值 |
| C03 | 複製後在目標新增紀錄（傳真、投遞、帳號、session、稽核） | 新的 `id` 大於複製過來的最大值，不衝突（PostgreSQL 的 `setval`、MSSQL 的 identity） |
| C04 | 目標任一個表已有資料 | 拒絕，訊息指出哪個表；目標的資料不變 |
| C05 | 來源 schema 較舊、較新 | 較舊：提示先 `db migrate`；較新：拒絕；目標都沒有被寫入 |
| C06 | 複製途中失敗（例如來源有一個值超過目標欄位長度：SQLite 不限長度、MSSQL 的 `NVARCHAR(64)` 會拒絕） | 回非 0，訊息指出表與 `id`；目標七個表都是空的（rollback） |
| C07 | 服務執行中（PID 檔被鎖）、`--to` 與來源相同、`--to` 格式錯誤 | 拒絕，回非 0；目標沒有被寫入 |
| C08 | 秘密：來源有加密的 trunk 註冊密碼與 Email 密碼 | 目標的密文與來源相同；以同一份 `secret.key` 在目標 `config check` 沒有無法解密的欄位、`config export --include-secrets` 得到原來的明文 |
| C09 | 帳號與登入：複製後以目標啟動服務 | 原來的帳號密碼可以登入（Web API）；複製過來的 session cookie 仍然有效；已讀、備註在 `GET /faxes` 正確 |
| C10 | 分批邊界：傳真 0、1、批次大小 −1／＝／＋1、5,000 筆（含投遞） | 全部複製，順序與內容正確 |
| C11 | 輸出 | 每個表的列數正確；URL 中的密碼顯示為 `***`；最後有改設定檔的提示 |
| C13 | 欄位清單與 schema 一致：每種資料庫建立 schema 後，以 `information_schema`（SQLite 用 `pragma table_info`）列出每個表的欄位，與 `store::copy` 的清單比較；除了 `schema_migrations` 沒有遺漏的表 | 完全一致（之後的 migration 新增欄位而沒有更新清單時，這個測試會失敗） |
| C12 | 端到端（Docker）：以 SQLite 收一份真的 SpanDSP 傳真、標已讀、寫備註，`stop`，`db copy --to` PostgreSQL（或 MSSQL），改設定檔啟動，再收一份 | 舊傳真的已讀、備註、檔案預覽都在；新傳真 `id` 接續 |

## 3. 驗收

- 以上案例通過；C02、C03、C04、C06 在 CI 的 `db-matrix` 四種 DB 上都通過。
- `store` 涵蓋率維持 ≥ 80%（排除伺服器驅動，ADR-045）。
- `docs/manual/it.md` 的備份與還原、`docs/database.md` 補上換資料庫的步驟；CHANGELOG 中英。
