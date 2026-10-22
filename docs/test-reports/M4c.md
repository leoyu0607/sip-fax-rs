# M4c 測試報告：web-ui（Svelte SPA、i18n、內嵌進執行檔）

- 實作：Opus（M4c ①–⑤）；測試與本報告：M4c ⑥（Claude，依 Opus 的測試計畫）
- 計畫：`docs/test-plans/M4.md` §0、§6（U01–U19）、§6.3（W12、W13、A14、U05–U08）；規格：`docs/design/m4-web.md` §3–§6；ADR-058
- 分支：`m4c-web-ui`
- 狀態：**Opus 審核通過**（§9）

## 1. 環境

| 項目 | 內容 |
|---|---|
| 主機 | macOS（arm64），Docker Desktop，Node 26（CI 用 Node 24），Playwright 1.64.1、Chromium |
| Rust 測試 | 本機直接跑（`web` 的 HTTP 測試對真的 axum 伺服器與真的 SQLite，不 mock store） |
| E2E | `scripts/e2e-web.sh`：主機建置 `web-ui`，Rocky 9 測試映像（`docker/test-rocky9.Dockerfile`，linux/amd64）內以 `CARGO_TARGET_DIR=/work/target/docker` 建置 `sip-faxd`（內嵌這次的 `web-ui/dist`）與 `tools/` 的 `sip_uac`、`make_tiff`；容器內起兩個 `sip-faxd`：**a**（HTTP，web 8080、SIP 5060）與 **b**（HTTPS 自簽，web 8443、SIP 5061），各有自己的 SQLite 與 `conf/`；帳號以 `sip-faxd user add … --password-stdin` 建立（`root` admin、`amy`／`ben` agent、`newbie` 首次登入要改密碼）；主機上的 Playwright（Chromium，`zh-TW`、`Asia/Taipei`）連到發佈的埠；傳真由 SpanDSP（`docker exec … sip_uac --mode gateway --fax …`）真的以 T.38 送進容器內的 `sip-faxd` |
| DB 比對 | 測試以 `docker exec … python3`（sqlite3，唯讀）直接查容器內的 `fax.db`（`users`、`faxes`、`config_sections`、`config_audit`） |

## 2. 案例對照表

測試函式或 Playwright 案例都以案例編號開頭。

| ID | 測試位置 | 驗證內容 | 結果 |
|---|---|---|---|
| W12 | `crates/web/tests/m4c.rs::w12_q_matches_anywhere_ignoring_case_takes_wildcards_literally_and_ands_with_filters`、`w12_q_needs_a_session` | HTTP `GET /faxes?q=`：ANI／DNIS／TSI／備註任何位置、大小寫不分；`_`、`%`、`[` 是字面字元（`0911_11` 與 `0911x11` 分得開）；與 `ani`、`is_read`、`status` 篩選 AND；與 store 的結果相同；分頁帶著 `q`；沒有 session 回 401 | 通過 |
| W13 | `m4c.rs::w13_restart_fields_are_the_backends_list_and_a_restart_change_is_a_subset` | `GET /config` 的 `restart_fields` 等於 `fax_in::config::RESTART_FIELDS`；改 `sip.bind` 的 `restart_required` 含 `sip.bind` 且每一項都在清單內；agent 取 `/config` 回 403 | 通過 |
| A14 | `m4c.rs::a14_the_account_sets_its_own_language_and_only_zh_tw_en_or_null`、`a14_the_administrator_may_only_give_the_supported_languages_too` | `PUT /auth/me`：`zh-TW`／`en`／`null` 成功並反映在 `/auth/me` 與 DB；`fr`、`EN`、`zh-tw`、空字串、`en `、`zh_TW` 回 422 `bad_locale` 且 DB 不變；錯型別、多餘欄位、空 body 回 400／422 且 DB 不變、角色不會被改；缺 CSRF、錯 CSRF、非 JSON 回 403 且 DB 不變；未登入 401；`POST /users` 與 `PUT /users/{id}` 的 `locale` 同樣檢查（被拒的請求沒有建立帳號、也沒有改其他欄位） | 通過 |
| U01 | `web-ui/src/lib/i18n.test.ts`（M4c ② 既有） | 兩份語系 key 相同、沒有空字串、占位符相同 | 通過 |
| U02 | 後端：`crates/web/tests/codes.rs`（2 個）；前端：`web-ui/src/lib/codes.test.ts`（4 個） | 後端測試列出所有 API 錯誤代碼（32）、欄位錯誤代碼（14）、`hangup_code`（13，`HangupCode::ALL`）、`fax_status`（5）、投遞狀態（4），寫入／比對 `web-ui/src/lib/api-codes.json`（`UPDATE_API_CODES=1` 重寫）；另掃描 `web`、`accounts`、`runtime-config` 的原始碼（`ApiError::new`、`fn code`、`FieldError::new`、`let code = if …`），原始碼裡出現但不在清單的代碼會讓測試失敗；vitest 檢查每個代碼在兩個語系都有非空翻譯、且兩語系文字不同 | 通過；**找到 1 個缺口並修正**（§3 第 1 點） |
| U03 | `i18n.test.ts`（既有） | 日期時間依時區（UTC+8、UTC） | 通過 |
| U04 | `web-ui/src/lib/events.test.ts`（既有） | `since` 重連、`resync`、退避、4001、4008 | 通過 |
| U05 | `crates/web/src/ui.rs` 的單元測試（3 個，以自備的資產表驗證「沒有建置」說明頁、content type、`immutable`、HEAD、404）；`m4c.rs::u05_the_root_and_the_assets_over_http_with_type_cache_and_security_headers`、`u05_a_path_that_is_not_a_file_and_a_post_get_the_api_404` | `/` 為 `text/html`、`no-cache`、CSP 與 `nosniff`；有嵌入時 `assets/*.js|css` 的 content type 正確且 `public, max-age=31536000, immutable`、`/favicon.svg` 為 `no-cache`、`/index.html` 與 `/` 相同；沒有嵌入時（`SIPFAX_WEB_UI_DIST=/nonexistent` 手動確認過）是說明頁；`/no-such-page`、`/assets/` 、編碼過的 `..`、`POST /`、`DELETE /index.html`、`/api/v1/無此路由` 都是 API 格式 404 | 通過 |
| U06 | `scripts/test-package.sh`（6 個案例；`ci-local.sh` 與 CI 的 `web-ui` job 都會執行） | 在暫存目錄複製 `package.sh` 與假的 repo 配置（假執行檔、假 `dist`）：沒有 `web-ui/dist`、`dist/assets` 沒有入口檔、執行檔含另一個入口檔名、執行檔完全沒有介面、沒有執行檔，都回非 0、訊息指出原因、沒有產生 tarball；執行檔含這次入口檔名時成功並產生含 `sip-faxd` 的 tarball | 通過 |
| U07 | `faxlist.test.ts`、`schema.test.ts`、`api.test.ts`（既有，標題加上 U07）；新增 `session.test.ts`（6 個） | 列表即時事件（建立、進度、完成、已讀、備註）、捲動時暫存、篩選不符時移除；`schema` 路徑與秘密欄位；`api` 的 CSRF 與錯誤格式；`session`：`restore`、登入、改密碼、登出（含伺服器失敗仍結束）、儲存空間被封鎖 | 通過 |
| U08 | `web-ui/e2e/admin.spec.ts`（4 個） | ① 路由表單的 422：`routes[0]`、`routes[0].deliveries[0]` 的錯誤顯示在各自的卡片，文字由代碼翻譯，DB 的 `version`／`body` 不變，修正後版本 +1；② 一般表單的 422：`port_range` 的 `reversed` 顯示在欄位旁；③ 409：兩個瀏覽器，後存的看到衝突與重新載入，載入後可再存，另一方的修改沒被蓋掉；④ 秘密：新 trunk 輸入密碼 → DB 是 `enc:v1:…` 且不含明文；改別的欄位時請求內容的密碼是 `{"set": true, "ref": …}`，DB 的密文**位元組相同**；輸入新值 → 密文改變；勾選清除 → 清空；刪除 trunk | 通過；**找到 2 個產品問題並修正**（§3 第 2、4 點） |
| U10 | `access.spec.ts` | `newbie` 首次登入：只有改密碼頁，API 回 403 `password_change_required`；太短、兩次不同的提示且按鈕停用；目前密碼錯由伺服器拒絕且 DB 的 `must_change_password` 仍為 1；成功後進列表、旗標為 0、舊密碼 401、新密碼 200 | 通過 |
| U11 | `fax-live.spec.ts` | 真的 SpanDSP 以 T.38 送 3 頁（每頁 1500 行以上的精細頁）時列表開著：頁內 MutationObserver 記錄該列狀態徽章，依序出現「接收中，第 1／2／3 頁」，最後「完成」、頁數 3；文件從未重新載入（標記變數還在、沒有 `framenavigated`）；DB 為 `received`、3 頁、TSI 正確。**關閉 M4b 審核留下的項目**（V01 以真的 `Recorder` 驅動；這裡是真的通話） | 通過 |
| U12 | `fax-live.spec.ts` | 縮圖 3 張與整頁都載入（`naturalWidth`）、PNG 簽名；按鈕、方向鍵、縮圖換頁；開啟後 API、DB、列表的徽章都仍是未讀；PDF／TIFF 下載的位元組開頭與大小等於 API 的 `pdf_size`／`tiff_size`；已讀切換記錄 `amy`；備註儲存、重新載入仍在、列表顯示 | 通過 |
| U13 | `fax-live.spec.ts` | amy 與 ben 兩個瀏覽器 context：ben 輸入中，amy 先存，ben 的文字不被取代；ben 存時 409，顯示「備註已被 amy 修改」與對方版本，DB 仍是 amy 的；「改用對方的版本」；第二輪「用我的版本覆蓋」後 DB 為 ben 的、`note_updated_by = ben`，且沒在輸入的 amy 不用重新載入就看到新備註 | 通過；**找到 1 個產品問題並修正**（§3 第 3 點） |
| U14 | `access.spec.ts` | agent 沒有設定／帳號／稽核的選單；四個網址直接輸入（含重新載入）顯示「沒有權限」且頁面完全沒有送 `/config`、`/users` 請求；API：GET 都是 403、`/status` 是 200；`PUT /config/sip`、`POST/PUT/DELETE /users` 在帶正確 CSRF 時仍 403，`users` 筆數與 `sip` 的 `version` 不變、沒有 `evil` 帳號 | 通過 |
| U15 | `admin.spec.ts` | 先以 `+886912345678` 送一份（ANI 原樣含 `+886`）；admin 在介面把 trunk `gw` 的「發話號碼移除前綴」設為 `+886` 並儲存（沒有「需要重啟」提示、`config_audit` 記 `root`）；**不重啟**再送同一個來電，新紀錄 ANI 為 `912345678`，舊紀錄不變；回到列表時新傳真在「有 1 份新傳真」後面出現 | 通過 |
| U16 | `admin.spec.ts` | `sip.bind` 欄位在修改前就標示需要重啟（`unknown_source` 沒有）；改值並儲存後顯示「需要重新啟動服務才會生效：sip.bind」與頁面頂端提示；另一個 admin 開著的狀態頁不用重新整理就出現提示（`status.changed`）；DB 已存新值，但舊位址仍能收傳真（沒有套用）；改回原值後提示消失 | 通過 |
| U17 | `web-ui/e2e/language.spec.ts`（2 個） | 登入頁與 15 個頁面（列表、傳真、狀態、帳號、稽核、我的帳號、9 個設定區段）在兩種語言各掃一遍：英文頁沒有 CJK 字元（語言名稱「繁體中文」除外），中文頁沒有字典裡任何英文句子（以單字邊界比對，中文文字也含該字的除外，如 T.38、TIFF）；掃描包含 `aria-label`、`placeholder`、`title`、`alt`、`option`；另有對照：把中文頁當英文掃、英文頁當中文掃都必須找得到（確認檢查會失敗）；帳號語言存在伺服器、新瀏覽器登入後即為該語言，改回「依瀏覽器」為 `null` | 通過 |
| U18 | `access.spec.ts` | 只用 Tab、Enter、打字：登入、Tab 到該傳真的連結 Enter 開啟、Tab 到「標為已讀」Enter（焦點留在按鈕）、再 Enter 切回未讀、Tab 回列表連結；每一步以 API 確認 | 通過 |
| U19 | `web-ui/e2e/https.spec.ts`（2 個） | HTTPS 自簽（`ignoreHTTPSErrors`）：cookie `Secure`、`HttpOnly`、`SameSite=Strict`；HSTS 與 CSP；wss 即時更新（傳真送進實例 b，列出現並變「完成」）；PNG、PDF 下載、已讀、備註、缺 CSRF 的請求 403、登出後 cookie 消失且 API 401；頁面沒有 console error（CSP、混合內容、TLS）。對照：同一份程式的 HTTP 實例（a）的 `Set-Cookie` 沒有 `Secure`、沒有 HSTS | 通過 |

## 3. 發現並修正的產品問題

1. **U02：`reversed` 沒有翻譯。** 設定的 `port_range` 起點大於終點時，後端回欄位錯誤代碼 `reversed`，但 `validation.reversed` 兩個語系都沒有，使用者會看到「Not valid (reversed)」。補上兩個語系（`web-ui/src/locales/*.json`、`scripts/gen-locales.mjs`）。後端的代碼清單測試也因此改為能抓到「以 `let code = if … { "x" }` 選出的代碼」。
2. **U08：路由與投遞的 422 沒有顯示在畫面上。** 伺服器對路由只回 `routes[i]` 與 `routes[i].deliveries[j]` 這兩層的錯誤（不是個別欄位），前端只在欄位旁找 `routes[i].deliveries[j].dir` 這種路徑，所以「資料夾目錄是空的」之類的錯誤完全不顯示（只有一句泛稱）。`RoutesEditor.svelte` 現在把這兩層的錯誤（依代碼翻譯）顯示在對應的路由卡片與投遞卡片。
3. **U13：沒在輸入的人看不到別人存的備註。** `FaxDetail.svelte` 收到 `fax.note_changed` 時，先把 `fax` 改成新備註，再用已經改變的 `fax` 判斷「使用者是否在輸入」，結果永遠判為「在輸入」，不取代文字框，要重新載入才看到。改為先判斷再更新。
4. **U08：保留的秘密每次儲存都被重新加密。** 設計文件說「未輸入時送回 `{"set": true, "ref"}`，DB 中的密文不變」，但 `runtime-config/src/codec.rs` 的 `Keep` 會解密後以新的 nonce 重新加密，所以改別的欄位時密文也變。現在 `ref` 與路徑相同時保留原來的密文（仍會先解密以確認金鑰可用），只有秘密移動位置時才重新加密。
5. **（U15 中發現）列表在離開頁面期間錯過新傳真。** `FaxList` 的事件訂閱只在頁面開著時存在；使用者去設定頁再回來，列表是舊的，也沒有提示。事件訂閱移到 `App.svelte`（登入期間一直訂閱），不在列表頁時新傳真進「有 N 份新傳真」；登入時重置列表狀態。
6. **`PUT /auth/me` 的 body 少了 `locale` 時會把語言清掉**（`{}` 被當成 `null`）。改為 400 `bad_request`（`locale` 必須明確給，`null` 才是清除）。

另外觀察到、**未修改**：新 trunk 第一次儲存後表單會重建，「設定已儲存」提示一閃而過（測試改為等 DB 的資料列）；秘密欄位勾「清除」並儲存後，該表單在重新載入前仍顯示「已設定」。兩者都不影響資料，留給 ⑦ 或審核決定。

## 4. 與計畫不同之處

- U06 是 shell 腳本（`scripts/test-package.sh`），不是 Rust 測試：它要複製並執行 `package.sh`，用假的 repo 配置就能涵蓋，不需要真的 musl 建置。
- U05 的「有嵌入」分支依本機是否有 `web-ui/dist` 決定（沒有時整合測試檢查說明頁）；兩種建置的行為都由 `ui.rs` 的單元測試（自備資產表）固定下來，所以 CI 的 `test` job（沒有 `dist`）也完整。E2E 對真正嵌入的建置再驗證一次。
- U19 用第二個 `sip-faxd` 實例（HTTPS），其餘案例用 HTTP 實例；沒有把所有案例都跑在 HTTPS 上。
- U17 的「另一種語言的字串」以語系字典比對（不用人工 allowlist）：專有名詞若同時出現在中文文字裡就不算；字典以外的資料（帳號、號碼、備註）不檢查。
- W13 的「`fax_in::config::RESTART_FIELDS`」在 `web` 裡以 `runtime_config::RESTART_FIELDS`（再匯出）使用，測試直接和 `fax_in` 的比。
- U08 的「DB 中的密文不變」以 `config_sections.body` 內 `register.password` 的 `enc:v1:…` 字串逐字比對。

## 5. 涵蓋率

| 項目 | 數字 | 門檻 |
|---|---|---|
| `web` crate（`cargo llvm-cov -p web`，行） | **95.31%**（`ui.rs` 100%、`api/auth.rs` 95.45%、`api/users.rs` 98.03%、`error.rs` 86.67%） | ≥ 80% |
| 前端（vitest v8，`npm run coverage`） | statements **91.65%**（483/527）、branches 約 84%；41 個測試、10 個檔 | ≥ 70% |

前端的覆蓋範圍只含 `src/lib/**`（設定於 `vite.config.ts`）；`.svelte` 元件由 Playwright E2E 驗證。

## 6. 執行指令

```sh
cargo test -p web --test m4c --test codes && cargo test -p web --lib ui::
cargo test -p fax-in summary
UPDATE_API_CODES=1 cargo test -p web --test codes     # 後端代碼清單變動後重寫 web-ui/src/lib/api-codes.json
scripts/test-package.sh                                # U06
cd web-ui && npm ci && npm run lint && npm run check && npm run coverage && npm run build
cd web-ui && npx playwright install chromium           # 第一次
scripts/e2e-web.sh                                     # U08、U10–U19（約 4 分鐘，另加第一次建置）
scripts/e2e-web.sh -g U11                              # 只跑某個案例（Playwright 參數）
E2E_KEEP=1 scripts/e2e-web.sh                          # 不刪容器（docker rm -f sipfax-e2e-web）
```

`scripts/e2e-web.sh` 已放進 `scripts/ci-local.sh`（有 Docker 與 npm 時）與 CI 的 `e2e-web` job（`.github/workflows/ci.yml`；ubuntu、Node 24、`npx playwright install --with-deps chromium`），`e2e-web` 已加進 `scripts/branch-protection.json`（需管理員執行 `scripts/set-branch-protection.sh`）與 `release` 的 `needs`。失敗時上傳 `playwright-report`、`test-results` 與伺服器 log。

測試工具的改動（`tools/fax-sim`）：`sip_uac` 新增 `--from`、`--to`、`--ident`（來電號碼、受話號碼、TSI）；新增 `make_tiff`（`make_tiff OUT PAGES HEIGHT [standard|fine]`）。

## 7. 本機驗證結果

| 指令 | 結果 |
|---|---|
| `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`（根 workspace） | 通過 |
| `tools/`：`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`（Rocky 9 容器內） | 通過 |
| `cargo test --workspace --all-targets` | 通過（第一次有 1 個既有的沙盒暫存目錄測試偶發失敗，重跑全綠，與本次無關） |
| `cargo llvm-cov -p web --fail-under-lines 80` | 95.31%，通過 |
| `npm run lint`、`npm run check`、`npm run coverage`、`npm run build` | 通過（41 個測試） |
| `scripts/test-package.sh` | 6 個案例通過 |
| `scripts/e2e-web.sh`（全新容器、全新資料庫） | 16 個 Playwright 案例通過（3.9 分鐘） |

**沒有執行**：完整的 `scripts/ci-local.sh`（約 30 分鐘）；其中的 `cargo llvm-cov` 其他 crate 的門檻、`cargo deny`、musl 建置、`scripts/e2e-docker.sh`、`scripts/web-openssl-docker.sh` 未在本次重跑（本次沒有改動這些 crate 的程式，只有 `fax-in` 多一個常數與測試、`runtime-config` 的 `codec.rs`、`web` 的 `auth.rs`／`ui.rs`）。CI 上的 `e2e-web` job 也尚未實際跑過（ubuntu runner 上 Playwright 與 Docker 的組合只在本機 macOS 驗證）。

## 8. 已知限制

- 兩個即時案例（U11、U15、U19）依賴 SpanDSP 的真實時間（U11 約 1.4 分鐘，3 頁 × 約 25 秒）。
- E2E 的案例共用一組伺服器狀態，依檔名順序執行（`workers: 1`）；案例結束時把改過的設定改回（`sip.bind`、`retention`、`routes`、trunk `sec`），U15 的 `ani_strip_prefix` 在案例開頭歸零，所以重複執行同一組伺服器也可以通過。

## 9. Opus 審核

**結論：通過。**（2026-10-22）

- **空斷言**：逐一檢查 `toBeDefined`／`toBe(true)` 的用法，都在更強的斷言之前（例如先確認 cookie 存在，再比對 `secure`、`httpOnly`、`sameSite`），沒有只斷言「沒有拋錯」的案例。
- **過度 mock**：Rust 的 HTTP 測試對真的伺服器與 SQLite；E2E 對 Docker 裡真的 `sip-faxd`，傳真由 SpanDSP 真的以 T.38 送進來，DB 以唯讀查詢比對。前端單元測試只 stub `fetch`（`faxlist`、`session`、`api`），合理。
- **反向斷言**：A14、U14、U08 都檢查被拒的請求沒有改到 DB（版本、帳號數、語言、密文）。
- **涵蓋率**：`web` 95.31%（門檻 80%）；前端 statements 91.65%（門檻 70%）。
- **U11** 以頁內 MutationObserver 記錄徽章的每個狀態，確認第 1／2／3 頁依序出現且沒有重新載入，關閉 M4b 審核留下的項目。
- **產品修正**（§3 的 6 項）都看過：codec 的 `Keep` 只在路徑相同時沿用原密文、仍先解密確認金鑰，正確；其他為前端與 `PUT /auth/me` 的小修正。決策補進 ADR-058。
- **審核時修正**：§3 最後兩個未修的介面問題由 Opus 修正——新 trunk 第一次儲存後表單不再重建（`Settings.svelte` 不重新載入全部設定）、儲存後重讀該區段讓秘密欄位立刻顯示新狀態（`SectionEditor.svelte`、`FieldInput.svelte`）；U08 改為斷言這兩點（不再以重新載入繞過）。
- **執行檔大小**（計畫 §9）：x86_64 musl（含介面）19.46 MB，v0.2.0-alpha 14.54 MB，增加 4.9 MB。
- **待辦**：CI 的 `e2e-web` job 在 GitHub 上第一次執行的結果要確認；`e2e-web` 需由管理員以 `scripts/set-branch-protection.sh` 設為必要檢查。
