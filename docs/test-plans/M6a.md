# M6a 測試計畫：串接介面（API 金鑰、`fax_records` view、增量查詢、文件與範例）

- 撰寫：Opus（2026-10-27）
- 執行：Sonnet
- 審核：Opus
- 需求與設計：ADR-070。專案同事的 Java／Node.js 程式從別台機器讀傳真的狀態與資訊、取 TIFF／PDF 與 PNG 縮圖，可以讀資料庫或呼叫 API。

## 0. 設計摘要

- 資料表 `api_keys`（migration 0004）：`id`、`name`（唯一，1–48 字元，`[A-Za-z0-9._-]`）、`token_hash`（SHA-256 hex，唯一）、`prefix`（金鑰前 12 字元）、`scopes`（`fax:read` 或 `fax:read,fax:write`）、`created_by`、`created_at`、`expires_at`、`revoked_at`、`revoked_by`、`last_used_at`、`last_used_ip`。`db copy` 一併複製。
- 金鑰格式 `sfx_` ＋ 43 個 base64url 字元（32 個隨機 byte）。`Authorization: Bearer <金鑰>`。
- 能用金鑰的只有 `/api/v1/faxes` 底下：`fax:read` → 列表、`changes`、`counts`、單筆、檔案、頁面 PNG；`fax:write` → `PUT read`、`PUT note`。`redeliver` 與單筆中的投遞紀錄不開放給金鑰。其他路由只接受 session。
- 管理：`GET/POST /api/v1/api-keys`、`POST /api/v1/api-keys/{id}/revoke`（只限管理員的 session）；Web「API 金鑰」頁；CLI `sip-faxd apikey create --name <名稱> [--write] [--expires <日期>]`、`list`、`revoke <名稱>`。
- `GET /api/v1/faxes/changes?after=&since=&limit=`：依 `(updated_at, id)` 遞增，只回 `updated_at <= now − 2 s` 的列，回 `{faxes, next}`。
- view `fax_records` 與 `faxes (updated_at, id)` 索引（migration 0004）。
- `docs/openapi.yaml`，服務在 `/api/v1/openapi.yaml` 提供；`docs/integration.md`；`examples/java`、`examples/node`。

## 1. 共通規則

沿用 `docs/test-plans/M4.md` §0：DB 相關案例用 `db_case!` 或同等矩陣（本機 SQLite，CI 的 `db-matrix` 跑四種伺服器 DB）；Web 案例用真的 HTTP（`crates/web/tests` 的做法），不 mock store；每個「拒絕」案例都要有反向斷言（資料沒有被改、沒有被讀到）。金鑰明文不得出現在 log、稽核、錯誤訊息、`config export` 與 DB 中（以搜尋斷言）。

## 2. API 金鑰（K）

| ID | 案例 | 預期 |
|---|---|---|
| K01 | 建立（Web、CLI 各一）：唯讀與可寫各一把，有無到期日 | 回傳的金鑰符合格式，只出現這一次；DB 只有雜湊與前綴；列表不含金鑰、含前綴、權限、建立者、時間 |
| K02 | 唯讀金鑰呼叫列表、`changes`、`counts`、單筆、TIFF、PDF、頁面 PNG（full、thumb） | 都 200，內容與以 session 呼叫相同（逐欄位）；單筆不含 `deliveries` |
| K03 | 唯讀金鑰 `PUT read`、`PUT note` | 403 `forbidden`，傳真不變；可寫金鑰成功，`read_changed_by`／`note_updated_by` 為 `api:<名稱>`，WebSocket 收到對應事件 |
| K04 | 無效金鑰：格式錯、不存在、已撤銷、已過期、`Bearer` 大小寫、多餘空白、`Basic` | 全部 401 `invalid_api_key`（格式錯或其他 scheme 也是 401），訊息相同；撤銷與過期在下一個請求立即生效 |
| K05 | 金鑰呼叫非傳真路由：`/config`、`/users`、`/status`、`/auth/me`、`/auth/password`、`/events`（WebSocket）、`/api-keys`、`redeliver` | 都被拒（401 或 403），沒有任何副作用（例如密碼沒變） |
| K06 | 同時帶 cookie 與金鑰 | 以金鑰為準（不會因為 cookie 拿到管理員權限）；只帶 cookie 的行為與以前完全相同（M4 的 Web 測試全部照過） |
| K07 | 管理權限：agent 的 session 呼叫 `/api-keys` 的三個操作 | 403；名稱重複（含已撤銷）409；名稱格式錯、到期日在過去 422 |
| K08 | 稽核 | 建立、撤銷各一列 `api_key:<名稱>`，含操作者，不含金鑰 |
| K09 | 最後使用時間 | 第一次使用就記錄時間與 IP；一分鐘內再用不再寫 DB（以計數或時間斷言）；之後再寫 |
| K10 | CLI：`apikey create`（印出金鑰一次）、`list`（表格，含已撤銷）、`revoke`（不存在時非 0）；服務沒在跑也能用 | 與 Web 建立的金鑰互通 |
| K11 | `db copy` | `api_keys` 逐列複製，複製後的金鑰在目標可用 |
| K12 | 四種 DB | K01、K03、K04、K09、K11 在矩陣中通過 |

## 3. 增量查詢（C）

| ID | 案例 | 預期 |
|---|---|---|
| C01 | 300 筆傳真、各種狀態；以 `limit` 1、7、200 從頭翻到底 | 每筆恰好出現一次，順序依 `(updated_at, id)`；最後一頁之後 `next` 不變、`faxes` 為空 |
| C02 | 翻頁期間有傳真被更新（已讀、備註、接收中完成）與新增 | 被更新與新增的在後面的頁再出現一次；沒有任何一筆漏掉（以注入時鐘驗證 2 秒的安定期） |
| C03 | `updated_at` 相同的多筆跨頁 | 依 `id` 不漏不重 |
| C04 | `since` 與 `after` 一起、cursor 壞掉、`limit` 0 或 201、未知參數 | 400 |
| C05 | 四種 DB | C01–C03 通過（毫秒精度相同） |

## 4. view `fax_records`（V）

| ID | 案例 | 預期 |
|---|---|---|
| V01 | migration 0004 在四種 DB；從 0003 升級、重跑（MySQL 的重跑容忍） | view 的欄位名稱、順序、型別與 `docs/integration.md` 的表一致（測試讀文件的表格或共用常數比對） |
| V02 | 各種傳真（含檔案已清除、接收中、G.711） | view 每列與 `faxes` 對應欄位相同；`has_tiff`／`has_pdf` 在檔案清除後為 false；不含任何路徑欄位 |
| V03 | 文件中的「唯讀帳號」SQL（PostgreSQL、MySQL／MariaDB、MSSQL）在 CI 的 DB 中照抄執行 | 該帳號能 `SELECT` view；對 `faxes`、`users`、`api_keys` 的 `SELECT` 與任何寫入都被拒 |

## 5. 文件與範例（D）

| ID | 案例 | 預期 |
|---|---|---|
| D01 | `docs/openapi.yaml` | 是合法的 OpenAPI 3.1（以解析器驗證）；`/api/v1/faxes` 底下每個路由與方法都有文件，文件中沒有不存在的路由（與 router 比對）；`/api/v1/openapi.yaml` 回傳同一份內容、不需登入 |
| D02 | Node 範例：對 Docker 中真的 `sip-faxd`（收過一份 SpanDSP 傳真）以唯讀金鑰執行 | 列出傳真、以 `changes` 翻頁、下載 PDF 與第 1 頁縮圖，檔案與 API 相同（位元組比對）；自簽憑證以 `NODE_EXTRA_CA_CERTS` 信任 |
| D03 | Java 範例：同 D02（CI 以 `setup-java` 的 JDK 17） | 同 D02；truststore 依文件步驟建立 |
| D04 | `docs/integration.md` 的每一段 SQL、curl 指令 | 在測試中逐段執行（或抽出成測試的共用檔），不會與實作不符 |

## 6. Web 介面（U）

| ID | 案例 | 預期 |
|---|---|---|
| U01 | Playwright：管理員建立唯讀金鑰 | 金鑰顯示一次（可複製），關閉後不再出現；列表有名稱、前綴、權限、到期、最後使用 |
| U02 | 撤銷 | 確認後列表標示已撤銷；以該金鑰呼叫 401 |
| U03 | agent 登入 | 選單沒有「API 金鑰」，直接開網址也看不到資料 |
| U04 | 繁中／英文 | 文字都有翻譯（沿用 i18n 檢查） |

## 7. 驗收

- 上列案例通過；`web`、`store`、`accounts` 涵蓋率維持門檻（web ≥ 80%、store ≥ 80%、其餘 ≥ 70%）。
- 安全：搜尋所有測試產生的 log 與 DB，沒有任何金鑰明文。
- 報告：`docs/test-reports/M6a.md`。
