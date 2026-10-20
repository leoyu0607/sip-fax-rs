# M2b：t4 crate（T.4/T.6 解碼器、MMR 編碼器、TIFF Class F、PDF）

- 執行：Sonnet subagent；日期：2026-10-21
- 環境：macOS arm64（主機，rustc 1.99.1、nightly + cargo-fuzz 0.13.2、cargo-llvm-cov 0.9.1；主機只有 anaconda 的 libtiff 工具，沒有 poppler）加 Rocky 9 容器 `sip-fax-rs-test:rocky9`（linux/amd64，LIBTIFF 4.4.0、poppler 21.01.0）。
- 新依賴：無（`proptest` 以 `workspace = true` 列為 dev-dependency）。
- 只動了 `crates/t4/**`。fuzz workspace 為 `crates/t4/fuzz`（見「給 Opus 的後續動作」）。

## 1. 執行指令
```sh
T4_ORACLE=skip cargo test -p t4                     # 主機：單元 8、codec 19、oracle 7（跳過）
cargo clippy -p t4 --all-targets -- -D warnings     # 無警告；cargo fmt -p t4 -- --check 乾淨
T4_ORACLE=skip cargo llvm-cov -p t4 --summary-only  # 行 98.52 %
docker run --rm --platform linux/amd64 -v "$PWD":/work -w /work \
  -e CARGO_TARGET_DIR=/work/target/docker-t4 sip-fax-rs-test:rocky9 cargo test -p t4   # oracle 真跑
cd crates/t4/fuzz && cargo +nightly fuzz run t4_decode -- -max_total_time=60 -max_len=4096
```
Docker 內 `cargo test -p t4`：單元 8、codec 19（1 個 ignored 的種子產生器）、oracle 7，全部通過；clippy 無警告。oracle 測試缺工具時會 **panic**（已驗證：主機沒有 `pdfinfo` 時 `f08` 失敗，訊息指示安裝或設 `T4_ORACLE=skip`）；設 `T4_ORACLE=skip` 才跳過。

## 2. 案例結果
| ID | 測試 | 結果 | 證據 |
|---|---|---|---|
| F01 | `oracle::f01_libtiff_g3_1d_is_decoded_pixel_exact` | 通過（Docker）| 5 種 bitmap（全白、全黑、棋盤、文字樣式、隨機雜點）× 寬 1728/2048/2432 × 250 列 × y 解析度 98/196 × `tiffcp -c g3:1d` 與 `g3:1d:fill` × 預設多 strip 與 `-r 100000` 單 strip，共 **120 頁**。每個 strip 單獨用我們的解碼器解，`rows` 等於 strip 列數、`bad_lines=0`、`error=None`，串起來與來源 **逐像素相同**。FillOrder 以 TIFF tag 判讀（libtiff 輸出皆為 msb-to-lsb；FillOrder=2 時測試端會反轉 bit）|
| F02 | `oracle::f02_...g3_2d...` | 通過（Docker）| 同上矩陣，`g3:2d`、`g3:2d:fill`，共 **120 頁**。另掃描 libtiff 輸出的 EOL 後 tag bit：y=98 時連續 2D 列最多 1（K=2），y=196 時最多 3（K=4），所以 K=2 與 K=4 都實際被解到 |
| F03 | `oracle::f03_...g4...` | 通過（Docker）| 3 寬度 × 5 bitmap × 2 解析度 × 2 種 strip 設定，共 **60 頁**，逐像素相同 |
| F01–F03 補充 | `oracle::f01_f03_every_run_length_against_libtiff` | 通過（Docker）| 寬 2600、5202 列：每個 run 長度 0..=2600（白→黑、黑→白）各一列，經 `tiffcp` 的 g3:1d、g3:2d、g4 編碼後由我們解碼，逐像素相同。因此白／黑的所有結尾碼、make-up 碼與延伸 make-up 碼（1792–2560）都對 libtiff 驗證過（我們的編碼器與解碼器共用同一張表，只靠 round trip 抓不到表錯誤）|
| F04 | `codec::f04_eol_with_fill_bits_rtc_and_eofb`、`mh_known_vector_...`、`mmr_known_vector_...`、`eol_less_mh_rows...` | 通過 | (a) 編碼器的 fill 模式每個 EOL 結尾對齊 byte 邊界（逐 EOL 斷言）；(b) 在每個 EOL 前塞 40 個 0（遠超過最少 11 個）後仍逐像素相同、結束原因 `Rtc`；(c) RTC 後接 5 bytes 垃圾：rows 仍是 30、`Rtc`；(d) 只有 5 個 EOL（無 RTC）後資料結束：30 列、`EndReason::Data`；(e) MR 的 RTC（6 × EOL+1）；(f) MMR：EOFB 後垃圾被忽略（`Eofb`、30 列），去掉 EOFB 則 `Data`、30 列；(g) 手寫 bit 向量（依 T.4 表手算，不經我們的編碼器）：MH `EOL+白1728+EOL+白1黑1727+6×EOL` → 2 列、`Rtc`；MMR `1 1 1 EOFB` → 3 列全白，`H 白10 黑10` 的第二列正確，且編碼器輸出與該 bit 字串 **逐 bit 相同**；(h) `eol_required=false` 讀取無 EOL 的 3 列，`eol_required=true` 則報 bad line |
| F05 | `codec::f05_corrupted_mh_rows_resync_and_substitute`、`..._first_row_bad...`、`..._corrupted_mr_row...`、`..._mmr_error...` | 通過 | 隨機雜點頁 40 列 MH：把第 5、17、18 列中段改成 1。結果 `rows=40`（列數保留）、`bad_lines=3`、`longest_bad_run=2`、結束 `Rtc`；壞列等於上一列（18 列等於 17 列的替代列），其餘 37 列與原圖逐 byte 相同；逐 byte 串流（chunk=1）結果相同。第 0 列壞 → 以全白替代。列中間被刪掉一段使 EOL 提早出現 → 該列為 bad、下一列正常。最後一列被截斷（無 RTC）→ 計為 1 個 bad line、不 panic。MR(K=2)：壞掉的 2D 列後面接 1D 列，之後所有列恢復正確。MMR：中段 4 bytes 設 0 → `Aborted`，`error` 有值，已輸出的列與原圖前綴 **逐像素相同**，`bad_lines=0`；`0000001xxx` 擴充 → `Unsupported`；截斷 → `Truncated` |
| F06 | `oracle::f06_our_mmr_encoder_is_decoded_by_libtiff` | 通過（Docker）| 3 寬度 × 5 bitmap = 15 頁：我們的 `encode_mmr` → `write_tiff` → `tiffcp -c none` → 解析 raw strip 與來源 **逐像素相同**；同時 `tiffcmp -t` 對獨立寫的未壓縮參考檔回傳 0 |
| F07 | `oracle::f07_multipage_tiff_is_valid_for_libtiff` | 通過（Docker）| 3 頁（文字／雜點／棋盤，y=98／196／98）。`tiffinfo` 實際輸出：3 個 `TIFF Directory`、每頁 `Compression Scheme: CCITT Group 4`、`Photometric Interpretation: min-is-white`、`FillOrder: msb-to-lsb`、`Resolution: 204, 98`（2 頁）與 `204, 196`（1 頁）、`Page Number: 0-3 / 1-3 / 2-3`、`ImageDescription: TSI: +886 2 2345 6789`、`Software`、`DateTime: 2026:10:21 08:30:00`、有 bad lines 的那頁 `Fax Data: receiver regenerated`、`Bad Fax Lines: 5`、`Consecutive Bad Fax Lines: 2`；**stderr 為空**（libtiff 沒有任何警告）。`tiffcmp -t` 與獨立參考檔相同；**負向對照**：參考檔改 1 個像素後 `tiffcmp -t` 結束碼非 0。`tiffcp -c none` 轉出後逐頁解析，3 頁都與來源逐像素相同 |
| F08 | `oracle::f08_pdf_is_valid_for_poppler_and_renders_pixel_exact` | 通過（Docker）| 5 頁（y=98／196 交錯，各 300 列）。`pdfinfo`：`Pages: 5`、stderr 空；各頁 `609.882 x 220.408 pts`（y=98）與 `609.882 x 110.204 pts`（y=196），與 1728/204·72、300/98·72、300/196·72 差距 < 0.01。`pdftoppm -mono -rx 204 -ry <98|196>` 每頁輸出 1728×300 的 PBM，raster 與來源 bitmap **逐像素相同**（5 頁）|
| F09 | `codec::f09_arbitrary_bytes_never_panic`、`f09_eol_rich_junk_never_panics`、`f09_bit_flipped_valid_streams`、`zero_fill_and_junk_streams_stay_bounded`、`row_limit_and_config_validation` | 通過 | proptest 各 400 組：任意 bytes（MH/MR/MMR、寬度 1728/2048/2432/1–300、任意 chunk、`eol_required` 開關）→ 不 panic、`rows ≤ max_rows`、`bitmap.len() = rows × row_bytes`、`longest_bad_run ≤ bad_lines ≤ rows`、MMR 的 `bad_lines=0`；EOL 密集的垃圾；合法串流隨機翻 bit。另 4 種 coding × 4 種填充（0x00／0xFF／0x55／0x01）各灌 1 MiB：rows ≤ 16 384。`max_rows=10` 對 50 列的 MMR → 剛好 10 列、`RowLimit`；全 0xFF 的 MH 在 `max_rows=7` 下 rows ≤ 7 |
| F09（fuzz）| `crates/t4/fuzz` 的 `t4_decode` | 通過 | 見 §4 |
| Round trip | `round_trip_all_codings_widths_and_chunkings`、`odd_and_tiny_widths_round_trip`、`every_run_length_round_trips`、`roundtrip_random_bitmaps`、`streaming_gives_...` | 通過 | 4 種 coding（MH、MR K=2／4、MMR）× 寬 1728/2048/2432 與 1、7、8、9、63–65、100、2559–2561、5000、8192 × 5 bitmap，逐像素相同；MH/MR 的 fill bit 版本；chunk 1/2/3/7/100/4096/100000 結果相同；proptest 隨機寬高與密度 |
| Writers | `codec::tiff_and_pdf_writers_validate_input` | 通過 | 空頁、零尺寸、零解析度、空資料、壞日期都回錯誤；ImageDescription 的非 ASCII 與控制字元換成 `?`；PDF 的 xref 每筆剛好 20 bytes，每個 offset 都指到 `N 0 obj` |

## 3. 過程中的問題與根因
1. **`tiffcmp` 預設不比像素。** F07 第一版以 `tiffcmp ref.tif ours.tif` 判定相同，通過；加上負向對照（參考檔改一個像素）後，`tiffcmp` 仍回傳 0。原始輸出：
   ```
   Output { status: ExitStatus(unix_wait_status(0)), stdout: "Compression: 1 4\nDirectory 1:\nCompression: 1 4\nDirectory 2:\nCompression: 1 4\n", stderr: "" }
   ```
   根因：兩個檔案的 Compression tag 不同（1 對 4），`tiffcmp` 印出差異後直接略過該頁的像素比對並回傳成功。修正：改用 `tiffcmp -t`（忽略 tag 差異，由 libtiff 解碼後逐 scanline 比較），並保留負向對照。**這個陷阱也適用於之後所有 `tiffcmp` 的用法**（M2d 的 E01/E02 若以 `tiffcmp` 比對 TIFF，必須加 `-t`，並要有負向對照）。
2. 其餘均為測試端錯誤：F05 第一版用文字 bitmap（列太短，無法破壞）→ 改用雜點頁；PDF xref 測試用 `from_utf8_lossy` 後的字串索引 byte offset → 改用原始 bytes；`tiffinfo` 的 tag 名稱是 `Fax Data: receiver regenerated`（我原本猜 `Clean Fax Data`）。
3. clippy 1.99.1：`while_let_loop`（decoder 主迴圈），已改。
4. cargo-fuzz 的第一次 60 秒只跑了 1 199 次（約 19 exec/s）。根因：harness 以 1 byte chunk 重複餵資料，而解碼器在每次 push 都會從頭重試未完成的那一列（對列長為二次方；見未解事項）。harness 改為 chunk ≥ 8 後同樣 60 秒跑了 13 628 次。1 byte chunk 的行為仍由單元測試覆蓋。

## 4. Fuzz
`crates/t4/fuzz`（獨立 workspace，`t4_decode`）：第一個 byte 選 coding／寬度（含 1–556 的任意寬度）／chunk 大小／`eol_required`；斷言：一次餵入與分段餵入的 bitmap 與 summary 完全相同、`rows ≤ max_rows`、`bad_lines/longest_bad_run` 一致、解出的任何影像都能用 `encode_mmr` 重新編碼並解回相同像素。
- 命令：`cargo +nightly fuzz run t4_decode -- -max_total_time=60 -max_len=4096`（ASAN，macOS arm64，nightly 1.101）
- 結果：**13 628 runs / 61 s，cov 690、ft 3251，沒有 crash、沒有 timeout、artifacts 為空**。種子 20 個（由 `T4_WRITE_SEEDS=crates/t4/fuzz/corpus/t4_decode cargo test -p t4 --test codec -- --ignored write_fuzz_seeds` 產生，corpus 不含 fuzzer 產生的檔案）。

## 5. 變異驗證
| 變異 | 結果 |
|---|---|
| 不再累計 `bad_lines` | F05 的 3 個測試失敗 |
| 2D pass mode 的 `a0 = b2` 改成 `a0 = b1` | round trip、chunking、odd-width 3 個測試失敗 |
| 白色 run=2 的碼 `0111` 改成 `0110`（編碼與解碼共用表，所以 round trip 不會抓到）| 單元測試（前綴自由性、已知碼）失敗；Docker 內 **oracle 7 個全部失敗**（libtiff 解不了我們的資料／我們解不了 libtiff 的資料）|
皆已還原。

## 6. 涵蓋率
`T4_ORACLE=skip cargo llvm-cov -p t4 --summary-only`（主機，oracle 跳過；門檻 85 %）：**行 98.52 %**（1151 行、漏 17），函式 100 %。

| 檔案 | 行涵蓋率 | 檔案 | 行涵蓋率 |
|---|---|---|---|
| bitio.rs | 97.65 % | lib.rs | 100 % |
| changes.rs | 98.08 % | pdf.rs | 98.84 % |
| decoder.rs | 98.95 % | tables.rs | 100 % |
| encoder.rs | 100 % | tiff.rs | 96.54 % |
| error.rs | 100 % | **合計** | **98.52 %** |

Docker 映像沒有 `cargo-llvm-cov`，所以「含 oracle」的涵蓋率沒有量；oracle 測試走的是同一批解碼／寫出程式碼，只會更高。未涵蓋的行主要是 `TooLarge`（>4 GiB）分支。

## 7. 與規格的偏離與取捨
1. **編碼器也支援 MH／MR**（規格只要求 MMR）。F04／F05 需要產生含 EOL、fill bits、RTC 的 MH／MR 串流，與其在測試裡另寫一份，不如放進 crate；正式流程只用 `encode_mmr`。
2. **RTC 的定義**：連續 6 個 EOL（MH 與 MR；MR 每個 EOL 後帶 tag bit）。少於 6 個 EOL 後資料結束，結束原因是 `Data`，已解出的列都保留。
3. **MH／MR 的「列後必須接 EOL」檢查**（`eol_required`，預設 true）：一列解完但後面不是 `≥11 個 0 加 1` 的 EOL，這一列視為壞列並重新同步，避免把錯位的資料當成好列。第一列前的 EOL 可省略（有些發送端不送）。無 EOL 的 strip 可設 `eol_required=false`。
4. **資料結束時殘缺的最後一列**計為 1 個 bad line（以上一列替代）；只剩 fill／補齊用的 0 則視為正常結束。
5. **MMR 單一 EOL 視為 EOFB**（不要求兩個）。MMR 的資料結尾補齊 0 視為正常結束。
6. **不支援** T.6 的 uncompressed mode／擴充碼（`0000001xxx`）：回 `Unsupported`（MH/MR 視為壞列；MMR 中止）。解碼器**不處理 FillOrder=2**（呼叫端自行反轉 bit；測試端的 TIFF 讀取器已示範）。
7. **TIFF 多寫了** `T6Options=0`，有 bad lines 時再寫 `BadFaxLines`／`CleanFaxData=1`／`ConsecutiveBadFaxLines`（規格列的 tag 全部都有）。`ImageDescription` 原樣寫入呼叫端給的字串（非 ASCII 換成 `?`），由 fax-in 決定格式（例如 `TSI: …`）。`DateTime` 由呼叫端提供（crate 不讀時鐘）；附 `format_tiff_datetime(unix_secs)`。每頁一個 strip（RowsPerStrip＝高度）。
8. **PDF 頁面大小**以 1/10000 pt 無條件捨去（寬 609.8823 pt）。無條件進位會讓 poppler 的 `ceil` 多出 1 個像素；實測 poppler 以 204×98／196 渲染得到剛好 1728×N。
9. 硬限制：寬度 1..=8192（`MAX_WIDTH`）、`max_rows` 預設 16 384、每列變色點 ≤ 寬度＋2、內部輸入緩衝每次最多多收 4 KiB 後就先解碼並丟棄已消耗的 bytes；全零或全 0xFF 的輸入不會讓緩衝區無限成長（resync 與 fill 掃描時只保留最後 11 個 0）。

## 8. 建議新增的 ADR（請 Opus 放入 `docs/DECISIONS.md`）
> **ADR-0xx：t4 crate 的語意與限制**
> - 影像 row 一律是 packed 1-bit、MSB 優先、**1 = 黑**；解碼器逐列以 callback 交出，永遠不自行累積整頁。
> - 非 ECM 錯誤處理：MH／MR 壞列以上一列替代、計入 `bad_lines`／`longest_bad_run`，在下一個 EOL 重新同步（`rows` 不變，列數與發送端一致）；MMR 無法重新同步，第一個錯誤即放棄該頁剩餘部分（`EndReason::Aborted` 加 `Summary::error`），已輸出的列保留。T.30 層以 `Summary` 判斷 MCF／RTN。
> - RTC＝連續 6 個 EOL；EOFB＝一個 EOL 起即視為結束。結束後的資料一律忽略。
> - 硬限制：寬度 ≤ 8192、`max_rows` 預設 16 384，達到時以 `EndReason::RowLimit` 結束。fax-in 應依頁面長度需求設定 `max_rows`，不可關閉。
> - TIFF Class F：little endian、每頁一個 G4 strip、`FillOrder=1`、MinIsWhite、`T6Options=0`；`DateTime`／`ImageDescription` 由呼叫端提供；有 bad lines 時寫 Class F 的 `BadFaxLines` 系列 tag。PDF：每頁一個 `CCITTFaxDecode` 影像（`K -1`、`BlackIs1 false`），頁面大小以 `pixels × 72 / dpi` 計算（1/10000 pt 捨去）。
> - 測試規範：以 libtiff 比對像素必須用 `tiffcmp -t` 並附負向對照（預設的 `tiffcmp` 在 tag 不同時不比像素）。

## 9. 給 Opus 的後續動作（我沒有碰這些檔案）
- 根 `Cargo.toml` 的 `exclude` 加入 `"crates/t4/fuzz"`（現為 `["tools", "crates/sip-core/fuzz"]`）。
- `fuzz.yml` 加入 `t4_decode`（工作目錄 `crates/t4/fuzz`，與 sip-core 同樣的做法；建議 `-max_len=4096`）。
- CI 的 coverage job 加 `cargo llvm-cov -p t4 --fail-under-lines 85`；CI／`ci-local.sh` 的 Docker 步驟加 `cargo test -p t4`（oracle 需要 Rocky 9 映像；主機步驟用 `T4_ORACLE=skip`，否則缺工具的主機會失敗）。
- `crates/t4/fuzz/Cargo.lock` 已被 `crates/t4/fuzz/.gitignore` 忽略（與 sip-core 一致）。

## 10. 未解事項
- **小 chunk 的重試成本**：每次 `push` 若剛好停在一列中間，會從該列開頭重解（成本是列長的二次方：release 下 11 KB 的棋盤頁以 1 byte 一包約 118 ms，16 bytes 一包約 5 ms，整包 0.2 ms）。T.38 封包通常 30–260 bytes、最長的列約 1.3 KB，實務上可忽略，也不會造成資源耗盡（每個 byte 最多重解一列）。若 M2c 的量測顯示問題，可把列內進度（x、a0、顏色）存進解碼器改成可續解。
- 解碼器目前沒有 ECM 專用介面（ECM 的 frame 重組由 t30 負責，重組後的位元流可直接餵入同一個解碼器）。
- 沒有 PDF/TIFF 的串流寫出（整份檔案在記憶體中組好）；一份 50 頁的傳真約數 MB，目前可接受。
- 含 oracle 的涵蓋率未量（Docker 映像缺 `cargo-llvm-cov`）。

## Opus 審核結論（2026-10-21）
**結論：通過。**

T.4 的對照測試已經整合進 `scripts/e2e-docker.sh` 和 CI 的 `e2e-spandsp` job，F01–F08 都在容器中實際執行並通過。主機和 CI 的 `test` job 則設定 `T4_ORACLE=skip` 略過這些測試。

已檢查：
- **F01–F03**：libtiff 產生的 300 頁影像全部逐像素解對。另外對每個 run length（0–2600）都和 libtiff 交叉驗證，涵蓋到「編碼器和解碼器共用同一張碼表，round-trip 測試抓不到錯碼」這個盲點。
- **F08**：用 poppler 把 PDF 轉回點陣圖，與原圖逐像素相同。
- **mutation**：3 次故意改壞都被測試抓到。

`tiffcmp -t` 加反例檢查的規則已經寫進 ADR-018，M2d 的端到端比對必須遵守。

接受的偏離：
- 不支援 FillOrder=2 和 T.6 uncompressed mode。T.30 協商時不會宣告這兩種能力，所以接受。

需要追蹤：
- 每次餵入的資料很小時，解碼的成本會變成平方級。M2c 整合時要用實際的 T.38 封包大小量測效能。
