# M6a 測試報告：串接介面（API 金鑰、`fax_records` view、增量查詢、文件與範例）

- 實作：Opus（ADR-070）；測試與本報告：Sonnet（依 `docs/test-plans/M6a.md`）
- 分支：`m6a`
- 狀態：（待 Opus 審核）
- 範圍：K01–K12、C01–C05、V01–V03、D01–D04、U01–U04。全部有對應的測試並通過；與計畫字面不同之處見 §4。

## 1. 環境

| 項目 | 內容 |
|---|---|
| 主機 | Windows 11（Git Bash），Docker Desktop，Rust 1.99.1，Node 24，JDK 22（本機）／JDK 17（CI `setup-java`），`T4_ORACLE=skip` |
| 伺服器 DB | 本機把 PostgreSQL 16、MySQL 8.0、MariaDB 11、SQL Server 2022 都以 Docker 起來（`TZ=Asia/Taipei`），**四種都實際跑過** store 的 m6a 案例與 accounts 的 `m6a_keys`（見 §2）；CI 的 `db-matrix` 會再跑一次 |
| Web 案例 | `crates/web/tests`：真的 HTTP 伺服器＋真的 SQLite 檔＋`ManualClock`，不 mock store |
| Playwright | 真的 `sip-faxd`（Rocky 9 容器，`scripts/e2e-web-container.sh` 的 instance a／b），SpanDSP 送傳真。Windows 本機因 18081 在系統保留埠範圍，改用 `E2E_*_PORT=28xxx`；容器內以具名 volume 放 `target/docker`（bind mount 上的連結卡住），這些只影響本機執行，不影響腳本與 CI |

## 2. 案例對照表

### K：API 金鑰

| ID | 測試位置 | 做法與實測 | 結果 |
|---|---|---|---|
| K01 | `web/tests/m6a_k.rs::k01_web_create…`；`accounts/tests/m6a_keys.rs::k01_…`；`sip-faxd/tests/m6a_cli.rs::k10_…`（CLI 建立）；`web-ui/e2e/apikeys.spec.ts` U01（介面建立） | Web、CLI 各建唯讀與可寫、有無到期日（到期含 .250 毫秒）。金鑰 `sfx_`＋43 字元（共 47）、base64url；建立回應之外的任何回應（列表、稽核、`/config`）、DB（以 SHA-256 與前綴比對，並搜尋 SQLite 檔與 WAL 的位元組）、**log（全域 tracing 擷取，且先斷言 log 真的記到「API key created」）**都找不到金鑰。列表有前綴、權限、建立者、時間，沒有金鑰與雜湊 | 通過 |
| K02 | `m6a_k.rs::k02_…` | 3 份傳真（其中一份已讀＋備註、一份有投遞紀錄）：列表（含篩選、`limit`）、`changes`、`counts`、單筆 3 份、TIFF、PDF、PNG（full、thumb、預設）以 session 與金鑰各呼叫一次，**JSON 逐欄位相同**（session 多出的只有 `deliveries`；`counts` 的 `deliveries` 數字也只有 session 有），檔案的位元組與 `content-type`、`content-length`、`etag`、`content-disposition`、`cache-control` 相同；thumb 小於 full；`If-None-Match` 回 304；404 錯誤形狀相同；讀取沒有改變任何資料 | 通過 |
| K03 | `m6a_k.rs::k03_…`；store／accounts 層 K12 | 唯讀金鑰 `PUT read`、`PUT note` 都 403 `forbidden`，傳真逐欄位不變；可寫金鑰：非 JSON 內容型別 403 `csrf`，成功後 `read_changed_by`／`note_updated_by` = `api:erp`，**真的 WebSocket（session cookie）收到 `fax.read_changed`、`fax.note_changed`，`by` = `api:erp`**；樂觀鎖對金鑰也有效（409 `conflict`，備註不變）；不存在的傳真 404；多餘欄位被拒 | 通過 |
| K04 | `m6a_k.rs::k04_…`；`m6a_keys.rs::k04_…` | HTTP：16 種壞的 `Authorization`（尾字元錯、不存在但形狀正確、少／多字元、大寫、`SFX_`、`Bearer`／`Bearer `／`Bearer sfx_`、`Basic`、`Token`、沒有 scheme、key 後帶多餘字）全 401 `invalid_api_key`，**回應本文全部相同**；撤銷（下一個請求即 401，與「從沒存在」同一個回應）、到期（到期前 1 ms 仍可用、到期當下 401）。沒有 Authorization 時走 session（401 `unauthenticated`）；金鑰放在 query 或 `X-Api-Key` 不被讀 | 通過（`Bearer` 大小寫與空白見 §4 ①） |
| K05 | `m6a_k.rs::k05_…` | 最強的金鑰（可寫）呼叫 19 個非傳真操作（`/config`、`/config/audit`、`/config/trunks/order`、`PUT /config/sip`、`/users` 四種方法、`/status`、`/auth/me` GET／PUT、`/auth/password`、`/auth/logout`、`/api-keys` 三個操作、`redeliver`、`/events`）全 401 或 403；WebSocket 升級帶金鑰在握手就 401。**反向斷言**：`/config`、`/users` 的內容、金鑰數、稽核列數、投遞紀錄數都不變，兩個帳號仍可用原密碼登入，新密碼登入 401 | 通過 |
| K06 | `m6a_k.rs::k06_…`；M4 全部 Web 測試 | 管理員 cookie＋CSRF＋唯讀金鑰 → 403（cookie 沒有加權限）；好 cookie＋壞金鑰 → 401（不退回 session）；可寫金鑰＋cookie → 記為 `api:writer`；金鑰不需 CSRF；傳真路由之外 `Authorization` 不被看（cookie 單獨判斷，見 §4 ②）；只有 cookie 的行為：缺 CSRF 仍 403 `csrf`。**`cargo test -p web` web 套件所有既有測試檔照過（涵蓋率測量時全數通過）** | 通過 |
| K07 | `m6a_k.rs::k07_…`；`m6a_keys.rs::k07_…` | agent 的三個操作 403、無 session 401，金鑰沒被建立／撤銷；管理員：名稱重複（含大小寫、含已撤銷）409 `name_taken`；名稱格式錯（空、空白、`/`、中文、49 字）422 `bad_name`；到期在過去或剛好現在 422 `bad_expiry`；到期格式錯／多餘欄位／缺名稱 400 或 422；缺 CSRF、非 JSON 403；撤銷兩次 409 `already_revoked`、不存在／非數字／負數 id 404；拒絕之後 DB 的金鑰清單沒變。service 層另驗 48 字元可、名稱去空白與轉小寫 | 通過 |
| K08 | `m6a_k.rs::k08_…`；`m6a_keys.rs::k08_…` | 建立、撤銷各一列 `api_key:<名稱>`（`key_create`／`key_revoke`），`by` 是操作者（兩個不同管理員），`after` 有權限與前綴；失敗的建立／撤銷不寫；整個稽核回應沒有金鑰；CLI 的列為 `cli:tester` | 通過 |
| K09 | `m6a_k.rs::k09_…`；`m6a_keys.rs::k09_…`（四種 DB） | 第一次使用記時間（毫秒）與 IP（127.0.0.1）；1、5、30、59 秒內各用 3 次，`last_used_at` 不變（也不換 IP）；被拒的金鑰不算使用；滿 60 秒再寫；以時間值（而非計數）斷言 | 通過 |
| K10 | `sip-faxd/tests/m6a_cli.rs::k10_…` | 真的 `sip-faxd` 執行檔、沒有服務在跑：`create`（金鑰印在 stdout 一次、`--write`、`--expires 2099-12-31`／RFC 3339）、重複／格式錯／過去／無法解析的到期 → 非 0 且不印金鑰，`list` 表格（前綴、權限、狀態、到期、最後使用、建立者；含已撤銷；不含金鑰），`revoke`（未知、已撤銷 → 退出碼 1）；與 Web 同一個 `AuthService`：CLI 建的金鑰驗證成功、以 service 建的出現在 CLI 的列表；DB 檔與稽核都沒有金鑰 | 通過 |
| K11 | `store/src/tests/copy_tests.rs::k11_…`（四種 DB，SQLite→伺服器與伺服器→SQLite）；`m6a_cli.rs::k11_…`（真 CLI `db copy`） | `api_keys` 逐列相同（含 id、最後使用、撤銷、毫秒），目標以雜湊找得到每把金鑰，撤銷者仍撤銷、名稱仍被占用、新金鑰 id 接在後面；CLI：複製後在目標 `authenticate_api_key` 成功、已撤銷的 401 | 通過 |
| K12 | `store/src/tests/m6a_tests.rs::k12_…`（db_case）；`accounts/tests/m6a_keys.rs`（`SIP_FAXD_TEST_DB`） | 四種 DB：建立（欄位、毫秒）、雜湊精確查找、名稱／雜湊唯一、最後使用（時間、地址、超長地址截為 64）、撤銷一次、名稱撤銷後仍占用、長度上限；accounts 的 K01、K04、K07、K08、K09 在伺服器上跑。**CI：`db-matrix` 新增步驟**（`grep` 確認 5 passed）；`scripts/db-matrix.sh` 同步 | 通過（四種 DB 本機實測） |

### C：增量查詢

| ID | 測試位置 | 做法與實測 | 結果 |
|---|---|---|---|
| C01 | `web/tests/m6a_c.rs::c01_…`；`store/…/m6a_tests.rs::c01_…` | 300 筆（receiving／received／partial／failed／no_fax 各 60，每 10 筆同一個時間、組間 7 ms，毫秒尾數 123），`limit` 1、7、200 從頭翻到底：每筆恰好一次、依 `(updated_at, id)`、`next` = `<ms>.<id>`、最後一頁之後 `faxes` 空且 `next` 不變；`since` 含邊界；store 的 `limit` 被夾在 1–200。空的資料庫 `next` = `0.0` | 通過 |
| C02 | `m6a_c.rs::c02_…`；`m6a_tests.rs::c02_…` | 翻 100 筆後，中途：已看過的被標已讀（session）、未看過的被加備註（金鑰）、接收中的完成、新增一筆。2 秒安定期用注入時鐘驗：同一刻不回、1999 ms 不回、2000 ms 回。翻完後**每一筆傳真在 feed 中的最新 `updated_at` 都等於目前的狀態**（沒有漏），四筆變動各再出現一次且只一次；同一筆再變只出現一次、為最新狀態。**競態**：時間 t 呼叫 feed 時不回 t−1.5 s 的列且 `next` 不前進；之後才提交的「較早時間戳」列（t−1.8 s）仍被拿到，順序正確 | 通過 |
| C03 | `m6a_c.rs::c03_…`；`m6a_tests.rs::c03_…` | 25 筆同一毫秒＋5 筆晚 1 ms，`limit` 1、4、6、25、26、30、200：與依 id 排序的全集完全相同；cursor 落在同時間的中間只回其餘；`since` 含邊界；空的 `since` 回 `<ms>.0`，且以它當 `after` 仍為空 | 通過 |
| C04 | `m6a_c.rs::c04_…` | 26 種壞請求全 400 `bad_request`（`after`＋`since`、cursor 壞（非數字、缺點、多點、負號、溢位）、`since` 格式錯、`limit` 0／201／-1／文字／小數／溢位、未知參數、大小寫不同的參數、重複的 `limit`）；session 與金鑰結果相同；邊界（`limit` 1、200、`after=0.0`、負 cursor、1970 的 `since`、帶時區的 `since`）200；無憑證 401（先於參數檢查）。**空值（`?after=`、`?since=`、`?limit=`）與前後空白視為未給**，見 §4 ③ | 通過 |
| C05 | `m6a_tests.rs`（db_case，四種 DB 本機實測） | C01–C03 在 PostgreSQL、MySQL、MariaDB、SQL Server 都通過，毫秒尾數（`.123`）保持 | 通過 |

### V：view `fax_records`

| ID | 測試位置 | 做法與實測 | 結果 |
|---|---|---|---|
| V01 | `m6a_tests.rs::v01_…` | **讀 `docs/integration.md` 第 5 節的欄位表**（編譯時 `include_str!`，38 列）與資料庫實際的欄位（`information_schema` ／ SQLite `pragma_table_info`）比對：名稱、順序、型別（整數／字串／時間／布林對應各 DB 的型別族）。另：舊資料庫降回 0003（放一筆傳真）→ `migrate` 回報 3→4、套用 `[4]`，view 出現且看得到該筆；MySQL 把 `schema_migrations` 的 4 刪掉再跑一次，view 與欄位仍一致 | 通過 |
| V02 | `m6a_tests.rs::v02_…` | T.38（modem v17）、G.711（modem v29）、接收中、失敗且無檔案、檔案已清除共 5 筆：**36 個非衍生欄位逐列與 `faxes` 相同**（以同一組 `Kind` 解碼，含已讀者 `api:erp`、備註、毫秒）；`has_tiff`／`has_pdf`：有檔案 1、接收中 0、無檔案 0、清除後 0；view 與文件都沒有任何 `path` 欄位 | 通過 |
| V03 | `m6a_tests.rs::v03_…`（四種 DB 本機實測） | **直接取 `docs/integration.md` 的 SQL 區塊**（只把 `sipfax` 換成測試資料庫名稱）用管理員連線照抄執行，再以該帳號連線：`SELECT id FROM fax_records` 得到 2 列；對 `faxes`、`users`、`api_keys`、`sessions`、`config_sections` 的 `SELECT`，對 `api_keys`／`faxes`／`fax_records` 的 `INSERT`／`UPDATE`／`DELETE`，`DROP VIEW`、`DROP TABLE` 全被拒；反向斷言：傳真 2 筆、無備註、金鑰 1 把。結束時刪帳號（PostgreSQL 先 `DROP OWNED`） | 通過 |

### D：文件與範例

| ID | 測試位置 | 做法與實測 | 結果 |
|---|---|---|---|
| D01 | `web/tests/m6a_d.rs`（4 個測試） | `serde_norway` 解析、**`oas3`（OpenAPI 3.1 解析器）驗證**（並確認它會拒絕壞文件）；自己再查：operationId 唯一、每個操作有 summary 與 responses、路徑參數與宣告一致、所有 `$ref` 都存在、每個狀態有描述、security scheme；**與 router 比對**：讀 `api/faxes.rs` 的 `.route(...)` 原始碼取得所有 `(路徑, 方法)`，文件的操作集合必須完全相同（`{file}` 展開成 `file.pdf`／`file.tif`，`pages/{page}` → `{page}.png`，`redeliver` 必須**不在**文件中）；**實打**：每個文件中的操作以有效輸入呼叫都 200 且 `content-type` 在文件列的媒體型別內；20 個錯誤情境（400／401／403／404／409／410／422）的狀態與 `error.code` 正確、且文件列了該狀態、描述中寫到該 code；304；`Fax` schema 的欄位集合等於實際 JSON 的欄位（兩個方向）、每個欄位型別／enum 符合（完成、清除、接收中三種傳真）、必填欄位都在；列表、`changes`、`counts` 的最上層欄位等於文件；文件的查詢參數都被實作接受、未知參數 400；`/api/v1/openapi.yaml` 免登入、`application/yaml`、與 `docs/openapi.yaml` 及 `web::api::OPENAPI` 位元組相同、解析後相同、帶壞金鑰仍 200、POST 405 | 通過 |
| D02 | `web-ui/e2e/examples.spec.ts`（Playwright，接進 `e2e-web` job） | 真的 `sip-faxd`（HTTPS instance b，自簽憑證）、SpanDSP 送的傳真、**CLI 建立的唯讀金鑰**；憑證用**文件上的 `openssl s_client … \| openssl x509` 指令**取得，並與伺服器 log 的 SHA-256 指紋比對；`NODE_EXTRA_CA_CERTS` 信任。`fax-client.mjs --once`：印出傳真、下載 `<id>.pdf` 與 `<id>-p1.png`，**與 API 回傳逐位元組相同**、大小等於 `pdf_size`、沒有 `.part`；cursor 檔等於 feed 末端；第二次執行沒有輸出也不重載（mtime 不變）；管理員標已讀後 2.5 秒再跑：恰好印一行、不重載檔案；刪掉 cursor 重跑：全部歷史再來並補齊檔案。對照：不給憑證 → 非 0 且不寫 cursor；缺設定 → 退出碼 2 並點名變數 | 通過（本機實跑） |
| D03 | 同上 | Java：**文件上的 `keytool` 指令**建 truststore，`java -Djavax.net.ssl.trustStore…  FaxClient.java --once` 做與 D02 完全相同的 5 項檢查；對照：沒有 truststore → 非 0 且不寫 cursor；`FaxDbReader.java` 以 `javac` 編譯通過（執行需 JDBC driver，SQL 由 V03、D04 驗證）。CI 用 `actions/setup-java@v4`（Temurin 17） | 通過（本機 JDK 22 實跑；JDK 17 由 CI 驗證） |
| D04 | `m6a_tests.rs::d04_…`（四種 DB）；`web/tests/m6a_d.rs::d04_…`；D02／D03 的 openssl、keytool | **SQL**：取文件「增量查詢」三種 SQL（PostgreSQL、MySQL／MariaDB、SQL Server）在對應伺服器照抄執行（只代入參數）：從頭拿到 3 筆、38 欄、依 `(updated_at, id)`；從同一毫秒的中間續查只回其餘；**剛寫入的列被 5 秒安定期擋住（伺服器 `TZ=Asia/Taipei` 下仍正確），一分鐘前的列看得到**。**curl**：文件的 3 行 `curl`（只拿掉 `--cacert`，伺服器是 HTTP）與 HTTP 請求範例逐行實跑：列表有 42 號、`42.pdf` 內容正確、`42-1.png` 是 PNG；文件「方法與路徑」表的 8 列與 OpenAPI 的操作互相包含；錯誤表的 7 個 code 都在文件中。`openssl`、`keytool` 見 D02、D03 | 通過 |

### U：Web 介面

| ID | 測試位置 | 做法與實測 | 結果 |
|---|---|---|---|
| U01 | `web-ui/e2e/apikeys.spec.ts` | 管理員的選單有「API 金鑰」；錯誤名稱被伺服器拒絕且沒建立；建立唯讀金鑰：面板顯示一次（格式驗證）、**複製按鈕後剪貼簿等於金鑰**（授權剪貼簿權限）、列表有名稱／前綴／唯讀／啟用／永不到期／建立者／最後使用「—」；關閉面板後金鑰不在 DOM，**重新載入也不在**，localStorage／sessionStorage 沒有；DB 只有雜湊（64）與前綴；以金鑰呼叫 200、`PUT read` 403，重載後「最後使用」有時間與來源地址；第二把可寫＋到期日，列表顯示權限與到期；重複名稱被拒、不顯示新面板 | 通過 |
| U02 | 同上 | 按撤銷：確認對話框文字正確；取消→仍啟用、金鑰可用、DB 未變；確定→列表「已撤銷」、按鈕消失、金鑰 401 `invalid_api_key`、DB 有撤銷者；另一把不受影響；撤銷後同名建立被拒 | 通過 |
| U03 | 同上 | agent（amy）：選單沒有「API 金鑰」；直接開 `#/api-keys` 顯示「不允許」、頁面沒有任何金鑰名稱、**頁面根本沒有呼叫 `/api-keys`**；API 三個操作 403，DB 沒有 `agent-made`、未撤銷者仍只有 e2e-writer | 通過 |
| U04 | 同上；`language.spec.ts` 的整頁掃描加入 `#/api-keys` | 字典 `apikeys.*`（＋`nav.apikeys`）每個 key 都有繁中與英文，且兩者不同；兩種語言各開表單、建立面板，頁面可讀文字都含對應語言的每個字串；英文頁沒有 CJK，繁中頁沒有英文字串；撤銷確認對話框也隨語言。既有 vitest 的 key 對稱檢查（`i18n.test.ts`）沿用 | 通過 |

## 3. 發現的產品問題

沒有需要修的產品 bug；所有產品程式碼未改。觀察與建議見 §4 ①②③④。

## 4. 與計畫不同之處／觀察

1. **K04 `Bearer` 大小寫與多餘空白**：計畫把它們列在「全部 401」。產品依 RFC 6750 把 scheme 視為不分大小寫、並裁掉金鑰前後空白，所以 `bearer`、`BEARER`、`Bearer  key`、`Bearer key `（尾端空白）會成功（200），這是 `session.rs::bearer_key` 的明確設計。我把這些列為「成功」並測了，其餘（含 `Basic`、`Token`、無 scheme、金鑰大小寫被改、key 後帶多餘字）全 401。**請 Opus 確認這比計畫字面好或要求改成嚴格比對。**
2. **K06 傳真路由之外的 `Authorization`**：`/users` 等只接受 session 的路由不看 `Authorization`，cookie 有效時帶任何金鑰（含壞的）都照 cookie 的身分處理（200）。這不是提權（身分仍是 cookie 的本人），計畫的「不會因為 cookie 拿到管理員權限」指傳真路由，那裡金鑰為準，已測。
3. **C04 空值**：`?after=`、`?since=`、`?limit=` 與值前後的空白視為「沒給」（沿用列表路由 `opt_text` 的慣例，表單會留空白欄位），回 200；計畫只說 cursor「壞掉」要 400，空字串不算壞。
4. **計畫 §0 的回傳形狀**：計畫寫 `{faxes, next}`；實作與文件、OpenAPI 都是 `{items, next}`（與列表的 `items` 一致）。測試以實作與文件為準。
5. **到期日的顯示**：建立時填「2099-12-31」（最後一天），列表的「到期」欄顯示 `2100/01/01 00:00:00`（有效期限的結束時刻）。與欄位說明「最後一天（到那天結束）」一致但可能讓人困惑，建議 UI 顯示「到 2099/12/31」或加註；未改。
6. **D01 的 router 比對方式**：axum 不能列舉路由，所以測試讀 `api/faxes.rs` 的 `.route(` 原始碼（再加實打每個文件中的操作）。若日後路由改成不在該檔宣告，測試會因「找到的路由 < 8」而失敗而不是默默通過。
7. **D02／D03 接在 Playwright**（`examples.spec.ts`），沒有另寫 `scripts/examples-check.sh`：`e2e-web` job 已有真的 sip-faxd、SpanDSP 傳真與 Node，只需加 `actions/setup-java@v4`（已改 `ci.yml`）。
8. **K12／C05 的 DB 矩陣**：`db-matrix` 既有的 `cargo test -p store` 就會跑 `m6a_tests.rs`、`copy_tests.rs::k11_…`（db_case／copy_case）；accounts 的 `m6a_keys` 需要另一個步驟（已加到 `ci.yml` 與 `scripts/db-matrix.sh`）。
9. **新增 dev-dependency**（web）：`oas3`、`serde_norway`、`tracing-subscriber`；`cargo deny check licenses bans` 通過，無 OpenSSL／native-tls。
10. **安全搜尋**：K01（Web、accounts、CLI）搜尋了稽核、`/config` 輸出、DB 檔（含 WAL）、log（擷取的 tracing）與 CLI 的 stderr／list 輸出，都沒有金鑰明文（只有前 12 字元的前綴）。U01 另驗瀏覽器儲存與 DOM。

## 5. 涵蓋率

`cargo llvm-cov clean --workspace` 後，`cargo llvm-cov -p <crate>`（store 依 CI 的做法排除三個伺服器驅動檔），行涵蓋率：

| crate | 行涵蓋率 | 門檻 |
|---|---|---|
| accounts | **94.12 %** | 70 % |
| store（SQLite；排除 `backend/{mssql,mysql,postgres}.rs`） | **87.45 %** | 80 % |
| web | **95.86 %** | 80 % |

## 6. CI

已改：`.github/workflows/ci.yml`（`db-matrix` 加 accounts 步驟；`e2e-web` 加 `setup-java`）、`scripts/db-matrix.sh`。CI（分支 `m6a`，run 40419100087）全部 job 通過：四種 `db-matrix`、`coverage`、`e2e-web`（含 D02、D03、U01–U04）、`lint`、`test`、`build-musl`、`smoke-rhel`、`e2e-spandsp`。第一次 CI 只有 D02 失敗：傳真剛收完時 `changes` 的 2 秒安定期內讀不到，已在範例執行前等 2.5 秒（那是文件記載的行為，不是產品問題）。

## 7. 新增與修改的檔案

- 新增：`crates/store/src/tests/m6a_tests.rs`、`crates/accounts/tests/m6a_keys.rs`、`crates/web/tests/{m6a_k,m6a_c,m6a_d}.rs`、`apps/sip-faxd/tests/m6a_cli.rs`、`web-ui/e2e/{apikeys,examples}.spec.ts`
- 修改：`crates/store/src/tests/{mod,copy_tests}.rs`（註冊與 K11）、`crates/web/Cargo.toml`、`Cargo.lock`、`web-ui/e2e/language.spec.ts`、`.github/workflows/ci.yml`、`scripts/db-matrix.sh`
