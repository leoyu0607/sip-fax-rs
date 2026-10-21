# M3e 測試報告：PNG 預覽（t4::png、TIFF 讀取、store::preview）

- 實作與執行：Sonnet（M3e）
- 計畫：`docs/test-plans/M3.md` 第 3 節 P01–P04；規格：`docs/design/m3-store.md` §1.1、§6、§7
- 狀態：**待 Opus 審核**。已在 worktree 分支 commit，沒有 push。沒有改 `crates/fax-in`、`apps/sip-faxd`、`crates/delivery`、`docs/DECISIONS.md`、`docs/TODO.md`、`docs/ROADMAP.md`。
- 本機驗證（Windows 11）：`cargo fmt --all`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace --all-targets`（`T4_ORACLE=skip`；660 個測試通過、0 失敗）、`cargo deny check`（advisories／bans／licenses／sources 全 ok）。獨立比對在 Docker（Rocky 9）內實跑通過。

## 1. 交付內容
| 項目 | 位置 |
|---|---|
| TIFF Class F 讀取器 `t4::TiffReader`（多頁、小端／大端、未壓縮／G3 1-D（MH）／G3 2-D（MR）／G4（MMR）、多 strip、FillOrder 1／2、Photometric 0／1、解析度單位 inch／cm；所有偏移與計數都做邊界檢查，不會 panic）。輸出 `Bitmap`（1 = 黑，行末填充位清 0）；中途損壞的頁面以白行補齊並計入 `bad_lines`，完全解不出任何一行才回錯誤 | `crates/t4/src/tiff_read.rs` |
| `t4::png`：`encode_full`（1-bit 灰階，standard 每行複製兩次）、`thumbnail`／`encode_thumb`（寬 300、8-bit、面積平均含非整數比例的部分像素權重、高度依修正後長寬比）、`row_repeat`、`thumb_size`、`encode_bilevel` | `crates/t4/src/png.rs` |
| `t4::Error` 新增 `InvalidTiff`、`UnsupportedTiff`、`PageOutOfRange`、`Png` | `crates/t4/src/error.rs` |
| `store::preview::Preview`：`page_png(id, page, PngKind::{Full,Thumb}) -> Result<PathBuf, PreviewError>`；快取 `data_dir/cache/png/<id>/<page>-<full\|thumb>.png`；`.part` 加 rename 的原子寫入（含 `sync_all`；`Drop` 守衛在錯誤或 panic 時刪掉 `.part`）；以快取檔為單位的 single-flight（`tokio::sync::Mutex` 表，用完清除）；轉檔在 `spawn_blocking`；`conversions()` 計數器；`with_fault_hook`（`#[doc(hidden)]`，測試用的注入點） | `crates/store/src/preview.rs` |
| `PreviewError`：`NotFound`、`Purged{id, at}`、`NoFile`、`PageOutOfRange{page, pages}`、`TiffMissing`、`UnsafePath`、`BadTiff`、`Io`、`Task`（conversion panic）、`Store` | 同上 |
| 清整時刪除 PNG 快取目錄：**M3a 的 `Purger` 早就有**（`remove_row_files` 刪 `cache/png/<id>`，R02／R03 已測）。本次補一條端到端測試：先產生快取，再跑 `Purger`，確認目錄消失、之後 `page_png` 回 `Purged` 且不會重建目錄 | `crates/store/src/tests/preview_tests.rs::p04_purged_files_and_cache_cleanup` |
| 測試 | `crates/t4/tests/preview.rs`（主機端）、`crates/t4/tests/png_oracle.rs`（Docker，libtiff＋libpng）、`crates/store/src/tests/preview_tests.rs` |
| 模糊測試目標 | `crates/t4/fuzz/fuzz_targets/tiff_read.rs`（TIFF 讀取器＋PNG 編碼器；**只做了編譯檢查，沒有實跑**，見 §8） |
| Docker／CI | `docker/test-rocky9.Dockerfile` 加 `netpbm-progs`；`scripts/e2e-docker.sh` 與 `.github/workflows/ci.yml` 的 t4 oracle 步驟加上 `--test png_oracle` |

## 2. 指令
```sh
cargo test -p t4 --test preview                          # 主機端：reader、PNG、縮圖、屬性測試
cargo test -p store preview                              # P01/P03/P04 store 層級（SQLite 檔案與 :memory: 各一次）
T4_ORACLE=skip cargo test --workspace --all-targets      # 主機（沒有 libtiff/netpbm）整體
# 獨立比對（Docker，Rocky 9，libtiff + netpbm）；等同 scripts/e2e-docker.sh 的最後一步
docker build -f docker/test-rocky9.Dockerfile -t sip-fax-rs-test:rocky9 docker
docker run --rm --platform linux/amd64 -v "$PWD":/work -v sip-fax-rs-cargo-registry:/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR=/work/target/docker -w /work sip-fax-rs-test:rocky9 \
  cargo test -p t4 --test oracle --test png_oracle
cargo llvm-cov -p t4 --summary-only && cargo llvm-cov -p store --summary-only
cargo deny check
```

## 3. 案例結果
| 案例 | 測試 | 結果 |
|---|---|---|
| **P01** 整頁 PNG（獨立解碼器） | `png_oracle.rs`（Docker）：tools = `tiffcp`、`tifftopnm`（libtiff）與 `pngtopnm`（libpng），**非 Rust、與本專案無關**。(a) `p01_png_of_our_g4_tiff_matches_libtiff_pixel_for_pixel_standard_and_fine`：t4 寫的 G4 TIFF，寬 1728／2048／2432／1001（非 8 的倍數）× 5 種點陣圖 × standard(98)／fine(196) = 40 頁。(b) `p01_png_of_libtiff_encoded_mh_mr_mmr_matches_libtiff_pixel_for_pixel`：由 `tiffcp -c none／g3:1d／g3:1d:fill／g3:2d／g3:2d:fill／g4`（含 `-r 30` 多 strip）產生 360 頁，我們的 reader → PNG，與 libtiff 解的原始點陣比對。(c) `p01_png_of_every_page_of_a_multipage_tiff_matches_libtiff`：5 頁、解析度交錯。每頁驗證：reader 輸出 = libtiff 輸出；libpng 解 PNG 後高度 = TIFF 高 × (standard 2／fine 1)；每一像素相等且只有 0／255；standard 時 PNG 第 2k 行與第 2k+1 行完全相同；縮圖經 libpng 解碼後與 `thumbnail()` 的像素逐一相等 | 通過（Docker，約 61 秒） |
| P01（主機端補強） | `preview.rs`：`p01_full_png_standard_repeats_every_row_twice_and_fine_does_not`（4 種寬度 × 5 種點陣圖，用 `png` crate 解碼；注意：這和編碼用同一個 crate，**不算獨立**，獨立性由上一列提供）、`p01_tiff_to_png_end_to_end_for_both_resolutions`、`p01_row_repeat_rule`、`p01_reader_*`（G3／未壓縮／大小端／FillOrder／Photometric／strip 共 320 種組合；cm 解析度換算）、`p01_prop_png_roundtrip_any_size`、`p01_prop_tiff_reader_png_pipeline`（proptest：任意大小點陣圖編碼後解碼相等、TIFF→reader→PNG 管線） | 通過 |
| P01（store 層級） | `p01_page_png_layout_and_content`：真 SQLite、真 TIFF；快取路徑 `cache/png/<id>/1-full.png`；第 1 頁（98 dpi）高度 400 且逐行重複、第 2 頁（196 dpi）原樣；縮圖 300 寬 8-bit | 通過 |
| **P02** 縮圖 | `p02_thumbnail_matches_independent_area_average_within_one_gray_level`：參考實作是測試裡自己寫的直白二維重疊面積加權（先建修正後的影像，再對每個輸出像素迴圈累加 `重疊寬 × 重疊高 × 值`；不共用被測程式的程式碼）。7 種尺寸（1728、2048、2432、1000、301、299、64 寬；比例皆為非整數）× 5 種點陣圖 × standard／fine；**最大誤差 ≤ 1 灰階**。另有 `p02_thumbnail_size_follows_corrected_aspect_ratio`（A4：1728×1143 standard → 300×397 與 1728×2286 fine → 300×397；一行高的頁面至少 1 行）、`p02_thumbnail_of_solid_and_half_pages_has_exact_values`、`p02_prop_thumbnail_close_to_reference`（proptest） | 通過 |
| **P03** 快取 | `p03_second_request_reads_the_cache`：第二次 `conversions()` 不增加、檔案位元組與 mtime 不變；**刪掉 TIFF 後仍能取得快取頁**（證明沒有讀 TIFF）；新 `Preview` 實例（等於重啟）也直接用快取。`p03_concurrent_requests_convert_once`：16 個並行（8 Full＋8 Thumb，barrier 同時放行，2432×3000 的大頁面）→ `conversions() == 2`；另一輪 8 個並行 Full → `conversions() == 1`；single-flight 表最後為空。`p03_interrupted_conversion_leaves_no_png`：注入點在 `.part` 寫完、rename 前，分別注入 I/O 錯誤與 panic，目錄裡**沒有 `.png` 也沒有 `.part`**；另模擬行程被殺而殘留的垃圾 `.part`：不會被當成結果，下一次轉檔會覆蓋它並產生完整可解碼的 PNG | 通過 |
| **P04** 錯誤 | `p04_page_out_of_range`：頁碼 0、3、4、1000、`u32::MAX`（Full／Thumb）→ `PageOutOfRange{page, pages: 2}`，訊息含頁碼與頁數，快取目錄沒有任何檔案；紀錄頁數不符時（紀錄寫 5 頁、檔案 2 頁）以 TIFF 為準。`p04_purged_files_and_cache_cleanup`：真 `Purger` 清整後 → `Purged{id, at}`（`at` = `files_purged_at`），PNG 快取目錄被刪除且不會重建。`p04_damaged_missing_and_unusable_tiff`：不存在的 id → `NotFound`；垃圾、空檔、只有檔頭、截斷（中段、IFD 前）、全 0 → `BadTiff`（且不留 PNG）；TIFF 被刪 → `TiffMissing`；沒有 TIFF 的紀錄 → `NoFile`；`../secret.tif` → `UnsafePath`；某頁的 strip 長度被改壞 → `BadTiff`。t4 層級：`p04_reader_rejects_malformed_and_unsupported_files_with_errors`、`p04_reader_never_panics_on_truncation_or_bit_flips_and_reports_garbled_data`（每個長度的前綴、每個位元組換成 0x00／0xFF／0x55，都不 panic）、`p04_reader_pads_a_truncated_strip_with_white_and_counts_missing_rows`、`p04_png_encoders_reject_inconsistent_bitmaps`、`p04_prop_reader_never_panics_on_random_bytes` | 通過 |

測試數量：`t4/tests/preview.rs` 17 個（含 4 個屬性測試）、`t4/tests/png_oracle.rs` 3 個（Docker）、`store` preview 7 個（每個跑 SQLite 檔案與 :memory: 兩次）。

### 3.1 測試是否有牙齒（人工變異，全數被抓到並已還原）
| 變異 | 被誰抓到 |
|---|---|
| 拿掉 single-flight 的鎖（`let _turn = ();`） | `p03_concurrent_requests_convert_once` 失敗 |
| `.part` 守衛不清理（`armed: false`） | `p03_interrupted_conversion_leaves_no_png`：`leftovers after a failed conversion: ["1-full.png.part"]` |
| `row_repeat` 一律回 1（standard 不複製行） | 7 個 `preview.rs` 測試失敗（P01、P02、屬性測試） |

## 4. 涵蓋率
| crate | 行 | 區域 | 門檻 |
|---|---|---|---|
| t4（`cargo llvm-cov -p t4`，`T4_ORACLE=skip`） | **98.31 %**（`png.rs` 100 %、`tiff_read.rs` 98.09 %） | 97.34 % | ≥ 85 |
| store（`cargo llvm-cov -p store`，SQLite） | **84.30 %**（`preview.rs` 96.60 %）；與 M3a 相同，伺服器驅動檔（mssql／mysql／postgres）在 SQLite 執行時未涵蓋，不在 80 % gate 的意義內（見 M3a §10-3） | 82.04 % | ≥ 80 |

## 5. 過程中的失敗與根因
| # | 現象 | 根因 | 處置 |
|---|---|---|---|
| 1 | Docker 內 `tifftopnm: No such file` | 我們與另外兩個 agent 共用映像標籤 `sip-fax-rs-test:rocky9`；別人以未含 `netpbm-progs` 的 Dockerfile 重建，把我剛建好的映像蓋掉 | 改用專屬標籤 `sip-fax-rs-test:rocky9-m3e` 跑比對；Dockerfile 已加 `netpbm-progs`，合併後映像會自然更新 |
| 2 | 測試 `p01_reader_converts_centimetre_resolution_to_dpi` 失敗（得到 20 dpi） | **測試寫錯**：8.03 點／cm 是 20 dpi；204 dpi 是 80.3 點／cm | 修正測試資料 |
| 3 | `p01_reader_handles_…photometric`：Photometric 1 的 G3 頁面像素全反 | **reader 的錯**：我對所有壓縮方式都在解碼後反相。T.4／T.6 的碼字本身就指明黑／白游程，Photometric 只影響未壓縮資料的位元極性 | `invert` 只用於未壓縮資料；測試涵蓋 Photometric 0／1 × 所有編碼 |
| 4 | `p04_reader_never_panics…`：「全 1 的 MMR 資料必須解碼失敗」不成立 | **測試想錯**：全 1 = 每行 V0 模式（複製上一行，即全白頁），是合法資料 | 改用全 0 資料（沒有任何一行可解 → 錯誤） |
| 5 | 同一測試的案例數斷言 240 ≠ 320 | 算式漏算「G3 且沒有 T4Options 標籤」這一組 | 修正期望值（5 × 4 × 16 = 320） |
| 6 | Docker：`tiffcp -c none` 讀 libtiff 自己用 `g3:1d` 寫的 1728 寬棋盤格檔時報 `Fax3Decode1D: Buffer overflow at line 0 of strip 0` | **libtiff 的限制**：一行 1728 個 1-pixel 游程會超過它的 1-D 游程緩衝。我們的解碼器可以解（F01 早已驗證） | 比對時，libtiff 的「真值」取自**未壓縮的來源 TIFF**（`check_page(tiff_path, truth_path, …)`）；我們的 reader 仍讀壓縮檔 |
| 7 | clippy `type_complexity`（`Option<&(dyn Fn…)>`） | — | 抽出型別別名 |
| 8 | **未查明**：開發過程中產生了 `crates/t4/tests/preview.proptest-regressions`，記錄 2 筆 `width = 1, height = 1, seed = 0, std_res = true` 的失敗 | 我沒有抓到當時的失敗輸出（那幾次的輸出被截斷）。現行程式碼對這個案例通過；之後對 pipeline 與縮圖兩個屬性各跑 20 000 案例（`PROPTEST_CASES=20000`）都通過。最可能是開發中途程式碼／測試的暫態（前述 #3、#4 的修正期間） | 保留該檔（重放時通過）；列入 §8 |

## 6. 對規格的偏離與補充
| 項目 | 偏離／補充 | 理由 |
|---|---|---|
| 呼叫介面 | 不是自由函式 `page_png(store, id, page, kind)`，而是 `Preview::new(store, data_dir).page_png(id, page, kind)` | 需要 `data_dir`，且 single-flight 表、轉檔計數器必須有共用狀態；放在 store crate 的 `store::preview`（它已有 `data_dir`、`resolve_in_data_dir`、`Purger`，且依賴 t4 單向） |
| 頁碼 | **從 1 開始**（0 和超過頁數都是 `PageOutOfRange`） | 使用者看到的頁碼；快取檔名 `1-full.png` |
| 頁數判斷 | 紀錄的 `pages` > 0 且頁碼超過時，直接拒絕，不讀 TIFF；否則以 TIFF 實際頁數為準 | 惡意請求不碰磁碟；紀錄頁數不可信時仍正確 |
| 錯誤型別 | 自有的 `PreviewError`（不是 `store::Error`），多出 `Purged`／`NoFile`／`UnsafePath`／`TiffMissing`／`BadTiff`／`Task` | 讓 M4 能對應 HTTP 狀態（404／410／500）；`Error` 的語意是資料庫 |
| TIFF 壓縮 2（CCITT RLE，無 EOL、列對齊位元組） | **不支援**，回 `UnsupportedTiff("compression")` | 本專案只寫 G4，libtiff 的 `tiffcp` 也不產生；解碼器需新增「列對齊」選項。沒有已知來源會送出 |
| 解析度判斷 | `x_dpi ≥ 1.5 × y_dpi` 才複製行（204/98 → 2；204/196 → 1；任一為 0／缺少 → 1） | 以解析度而非字串；規格只說 standard／fine |
| 縮圖比例 | 縮圖高度 = round(修正後高度 × 300 ÷ 寬度)，至少 1；寬度不足 300 也放大到 300 | 規格「寬 300」 |
| 紀錄清整時的 PNG 目錄 | 沒有改 `purge.rs`（已實作） | 見 §1 |
| 先寫測試再實作 | **沒有嚴格照做**：t4 與 store 的實作先寫，測試隨後補上（測試以變異驗證有牙齒，見 §3.1） | 誠實列出；Opus 可要求重做某部分 |

## 7. 擬議 ADR（供 Opus 合併到 `docs/DECISIONS.md`）
- **ADR-F：引入 `png` crate（0.18，純 Rust，MIT／Apache-2.0）做 PNG 編碼**，只用在 `t4::png`；傳遞相依為 `flate2`／`miniz_oxide`／`fdeflate`／`crc32fast`／`simd-adler32`／`adler2`／`bitflags`（皆純 Rust；`cargo deny check` 無新增例外；多了 `miniz_oxide` 的第二個版本，屬 `multiple-versions` 警告）。替代方案：自己寫未壓縮／固定 Huffman 的 deflate（約 150 行，檔案較大、要自己維護）。選擇 `png` 是因為它是事實標準、測試完整，且我們的正確性由 libpng 獨立比對。
- **ADR-G：預覽以「第一次開啟才產生並快取」實作，快取檔不入 DB**。`Preview`（store crate）以快取檔為單位做 single-flight，`.part`＋rename 原子寫入，清整時整個 `cache/png/<id>` 目錄連同 TIFF／PDF 一併刪除。快取沒有容量上限（受保留天數約束）；若 M4 要限制容量需另議。
- **ADR-H：TIFF 讀取在 t4（`TiffReader`）**，而不是在 store 或 fax-in。t4 因此第一次「讀取」檔案格式（仍無 I/O，吃 `&[u8]`）。

## 8. 未解事項（誠實清單）
1. **模糊測試目標 `tiff_read` 只編譯檢查（Docker 內 `cargo check --bins` 成功），沒有實跑**：本機沒有 nightly／libFuzzer。CI 沒有 fuzz job；請在有 nightly 的環境跑一輪。以其他方式替代的覆蓋：前綴截斷、逐位元組替換與隨機位元組屬性測試。
2. **proptest regressions 兩筆原因未查明**（§5 #8）。
3. **四種伺服器 DB 沒有實跑**：本次沒有修改任何 SQL，preview 測試只用 `get`／`upsert_finished`／`Purger`（皆為 M3a 已通過四種 DB 的路徑），但測試本身用 `db_case!`，在 `SIP_FAXD_TEST_DB` 下會對伺服器執行；我只跑了 SQLite（檔案＋記憶體）。
4. **`scripts/e2e-docker.sh` 整個沒有執行**（tools workspace、SpanDSP、SIPp 部分約數分鐘，且與本變更無關）；只跑了其中 t4 oracle 這一步（含新的 `png_oracle`），使用專屬映像標籤。`scripts/ci-local.sh` 整體也沒有完整執行（musl 的 zigbuild 本機有 M3a 已記錄的 zig 0.16 問題）。
5. 與 purge 的競態：轉檔進行中若 `Purger` 刪除了 `cache/png/<id>`，`rename` 會失敗並回 `Io`（不會留下檔案）；或轉檔在清整標記之前讀到 TIFF，之後才寫入快取，會留下孤兒 PNG，直到紀錄被清整（紀錄清整只在 `files_purged` 為 false 時才刪檔案）。機率極低、影響僅是磁碟空間，未處理。
6. 快取沒有淘汰與容量上限；一份大傳真每頁最多兩個 PNG。
7. 不支援 TIFF 壓縮 2 與 tiled／多位元影像（回 `UnsupportedTiff`）。
8. 縮圖大頁面用 `f32` 中間值（每行 300 個）；對 16384 行最大頁面約 20 MB 暫存。沒有做記憶體上限壓力測試。
9. `Preview` 還沒有接到任何呼叫端（M4 的 Web 介面會用）；沒有 HTTP 層測試。
10. 在 `docker/test-rocky9.Dockerfile` 加了 `netpbm-progs`，這會使所有人的映像快取失效一次（約 3 分鐘重建）。

## 9. 異動檔案
- 新增：`crates/t4/src/png.rs`、`crates/t4/src/tiff_read.rs`、`crates/t4/tests/preview.rs`、`crates/t4/tests/preview.proptest-regressions`、`crates/t4/tests/png_oracle.rs`、`crates/t4/fuzz/fuzz_targets/tiff_read.rs`、`crates/store/src/preview.rs`、`crates/store/src/tests/preview_tests.rs`、`docs/test-reports/M3e.md`
- 修改：`Cargo.toml`（`[workspace.dependencies]` 加 `png = "0.18"`）、`Cargo.lock`、`crates/t4/Cargo.toml`、`crates/t4/src/lib.rs`、`crates/t4/src/error.rs`、`crates/t4/fuzz/Cargo.toml`、`crates/store/Cargo.toml`（加 `t4` 路徑相依；dev 加 `png`）、`crates/store/src/lib.rs`、`crates/store/src/tests/mod.rs`、`docker/test-rocky9.Dockerfile`、`scripts/e2e-docker.sh`、`.github/workflows/ci.yml`
