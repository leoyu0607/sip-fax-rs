# M2a 測試報告：t38 crate、UDPTL、port_range、K01

- 執行：Sonnet subagent
- 日期：2026-10-21
- 計畫：`docs/test-plans/M2.md`（案例 I、U、X06、K01，以及 E07 的 port_range 部分）；規格 `docs/design/fax-stack.md` §0、§1、§4
- 環境：macOS arm64 主機（主 workspace）；Docker `sip-fax-rs-test:rocky9`（linux/amd64，Rocky 9、SpanDSP v3.1.1）執行 `tools/` workspace
- 範圍之外（依指示未動）：`crates/t4`（另一位 agent 進行中）、`docs/TODO.md`、`docs/ROADMAP.md`、`docs/DECISIONS.md`。擬議 ADR 見第 8 節，由 Opus 合併。

## 1. 執行指令
```sh
cargo test -p t38                                   # 41 單元 + 10 黃金向量 + 8 屬性測試
cargo clippy -p t38 --all-targets -- -D warnings
cargo llvm-cov -p t38 --fail-under-lines 85 --summary-only
cargo test -p sip-core --test dialog k01            # K01
cargo test -p fax-in --test ports --test config     # port_range
scripts/gen-t38-vectors.sh                          # 容器內重新產生黃金向量（新增）
scripts/e2e-docker.sh                               # tools workspace：fmt、clippy -D warnings、全部測試（含 U01、X06、SIPp）
cd crates/t38 && cargo +nightly fuzz run ifp_decode -- -max_total_time=60   # udptl_decode 同
scripts/ci-local.sh                                 # 見第 7 節
```

## 2. 案例結果

| ID | 測試 | 結果 | 證據 |
|---|---|---|---|
| I01 | `crates/t38/tests/golden.rs`（10 個測試）| 通過 | 8 個真實 SpanDSP 3.1.1 會話共 **7026 個 IFP 封包**，全部解碼成功且重新編碼**逐 byte 相同**：`non_ecm_v0/v3`（各 663）、`ecm_v0/v3`（各 686）、`non_ecm_v0_v29`（836）、`ecm_v3_v29`（863）、`non_ecm_v0_v27ter`（1353）、`ecm_v3_v27ter`（1276）。涵蓋 data type V.21、V.27ter 4800、V.29 9600、V.17 14400；field type HdlcData、HdlcFcsOk、HdlcFcsOkSigEnd、T4NonEcmData、T4NonEcmSigEnd；indicator NoSignal、CNG、CED、V21Preamble、V27/V29/V17 training。節例：`c0 01 80 00 00 ff`（v21、1 個 field、hdlc-data、len-1=0、資料 `ff`）。**變異測試**：把 v0 的 field-type 改成 v1+ 的帶 extension bit 編碼，4 個 v0 向量立刻失敗（`non_ecm_v0 #29 (answerer): re-encoded bytes differ`），v3 向量不受影響；已還原。 |
| I02 | `ifp::tests::roundtrip_every_combination`、`properties::ifp_encode_decode_roundtrip`（512 例）| 通過 | 所有 `Indicator`(23) × `DataType`(15) × `FieldType`(12) × 4 個版本；編碼成功者必定 round-trip，被拒者必定是「版本不允許的 addition」。 |
| I03 | `ifp::tests::v0_decoder_rejects_additions`、I02 的拒絕分支 | 通過 | V0/V1 編碼 addition → `IfpError::NotInVersion`；V0/V1 解碼 `20 00`（v8-ansam）、`60 00`（t30-data v8）、cm-message 的位元組 → `NotInVersion`；V2 解碼 `20 00` 成功。 |
| I04 | `ifp::tests::malformed_inputs`、`properties`（arbitrary bytes、每個前綴截斷、單 byte 突變）| 通過 | 空輸入、undefined root（t30-data 值 9）、ext 索引 63 與 ≥64 形式、indicator 帶 data field、count 超過輸入、field-data 截斷、非法 fragment header `c9`、尾端多餘 byte 皆回錯誤；任意 bytes 不 panic；任一合法封包的**每個**真前綴都是錯誤。fuzz 各 61 s：`ifp_decode` 1,264,770 次、`udptl_decode` 1,182,543 次，無 crash。 |
| I05 | `ifp::tests::i05_length_determinant_boundaries`、`per::tests::*` | 通過 | field 個數 1/127/128/16383/16384/16385/65536，v0 與 v3：標頭 `01`、`7f`、`80 80`、`bf ff`、`c1`、`c1`、`c4`；16384 個 field 剛好是 fragment 的整數倍，結尾補一個空 chunk `00`（X.691 10.9.3.8.3）；`write_octet_string/read_octet_string` 對 0/127/128/16383/16384/16385/70000 round-trip。 |
| U01 | `tools/fax-sim/tests/u01_udptl_spandsp.rs`（6 個測試，容器內）| 通過 | **SpanDSP 3.1.1 的函式庫沒有 UDPTL API**（見第 4 節），改用其原始碼樹中的參考實作 `tests/udptl.c` 編進 `spandsp-sys`：我方 `UdptlSender` 與 `udptl_build_packet` 對相同 IFP 序列輸出**逐 byte 相同**：redundancy 深度 0–4 + none 共 105 個資料包、FEC 6 組 (span, entries) 共 180 個資料包。雙向互通：我方→SpanDSP 解碼器（redundancy 遺失 6 個、FEC 遺失 2 個，SpanDSP 全數補回）；SpanDSP→我方 `UdptlReceiver`（redundancy 6 個由 redundancy 補回；FEC 3×1、2×2、4×3 各遺失 1 個 per set，30/30 依序交付）。另有手算範例：`00 01 01 02 00 01 01 06`（seq 1、primary `02`、secondary `06`）、FEC 2×1 seq 3 為 `00 03 02 13 23 80 01 02 01 02 03 03`。 |
| U02 | `udptl::tests::fec_*`、`receiver::tests::fec_*`、`properties::u05_fec_single_loss_per_set`（400 例）| 通過 | FEC 的暖機（ramp-up）、XOR 集合、不等長補零、FEC 整數編碼邊界（0、127、128、255、256、2³¹、u32::MAX）；每個集合遺失 1 個一定補回；兩個遺失在同一集合 → 回報 `Gap{count:2}`；interleaved 2×2 可補回連續 2 個遺失。 |
| U03 | `receiver::tests::*`（14 個）| 通過 | 順序交付；視窗內亂序重排；重複丟棄（`duplicates`）；redundancy 補回（連續 3 個遺失）；FEC 補回；無法補回 → `Gap`，之後正常；`flush()` 釋放被卡住的封包並回報缺口，之後遲到的封包計入 `late` 並丟棄；首包即帶回 secondary；遠距跳號 = 一個大 Gap；序號倒退過遠 → resync；格式錯誤的資料包不改變狀態；惡意 FEC 參數不掛住。 |
| U04 | `receiver::tests::sequence_wrap_65534_to_1`、U05 的起始序號 65500–65535 | 通過 | 65533→65534→65535→0→1→2 依序交付、無誤判缺口；亂序（`0,1,3,2,4,5`）與跨 wrap 的遺失（由 redundancy 補回 2 個）皆正確。 |
| U05 | `properties::u05_stream_properties`（400 例；暫時調到 30000 例也全過）| 通過 | 隨機模式（none、redundancy 0–4、FEC span 1–4 × entries 1–3）、約 20% 遺失、重複、±5 個位置的亂序：交付序列嚴格遞增且無重複；**delivered + Σgap == 第一個到最後一個交付的序號範圍**；到達的封包（起點之後）全部交付；內容與原始相同（FEC 者為原始 + 零填補）；redundancy 下連續遺失 ≤ 深度則 Gap 為 0；`stats.delivered/lost` 與事件一致。 |
| X06 | `tools/fax-sim/tests/x06_udptl.rs`（6 個測試，容器內）| 通過 | 見 §3。 |
| K01 | `sip-core/tests/dialog.rs::k01_*`（2 個）、`fax-in/tests/real.rs::flow_we_initiate_the_t38_reinvite`（已移除 M1d 的 workaround）| 通過 | 見 §5。 |
| port_range | `fax-in/tests/config.rs`（3 個新增）、`fax-in/tests/ports.rs`（3 個）、`ports::tests`（3 個）| 通過 | 見 §6。 |

## 3. X06：UDPTL 層、redundancy / FEC、遺失注入
`tools/fax-sim/src/t38_loopback.rs` 新增 UDPTL 模型（`T38LoopbackConfig::udptl = Some(UdptlLayer{recovery})`）：SpanDSP 每次 tx handler 呼叫、每個要求的 copy 各自成為一個 UDPTL 資料包（與 FreeSWITCH 相同，各有新序號）；接收端用本 crate 的 `UdptlReceiver`，把依序、帶原始序號的 IFP 交給 SpanDSP（FEC 補回而零填補的封包先以 `Ifp::decode_prefix` 去掉尾端零）。遺失以每個資料包 5%（permille 50）注入。`t38_version`（`t38_set_t38_version`）與 `modems` 為新增設定，`run_detailed()` 另回傳擷取的 IFP 與 UDPTL 統計。舊的 raw 模型（M0）不變，S01–S10 全數仍通過、hash 不變。

M0 §3.4 失敗的案例（ECM + 5% 遺失），同樣 5 個 seed：

| 模型 | seed | caller / answerer | sent | dropped | recovered | gap | 收到頁面 |
|---|---|---|---|---|---|---|---|
| raw（對照組，無 UDPTL）| 0x5eed0001–0005 | 21/36、21/36、20/3、21/48、21/48 | 440–1036 | 29–53 | 0 | – | 無 |
| UDPTL redundancy 3，ECM，v3 | 0x5eed0001 | 0 / 0 | 778 | 31 | 31 | 0 | 1728×1100 |
| 同上 | 0x5eed0002 | 0 / 0 | 778 | 41 | 41 | 0 | 1728×1100 |
| 同上 | 0x5eed0003 | 0 / 0 | 778 | 46 | 46 | 0 | 1728×1100 |
| 同上 | 0x5eed0004 | 0 / 0 | 778 | 33 | 33 | 0 | 1728×1100 |
| 同上 | 0x5eed0005 | 0 / 0 | 778 | 42 | 42 | 0 | 1728×1100 |

- 同樣 5 個 seed 在 T.38 version 0（SpanDSP 預設）、ECM 下全過。
- **non-ECM + 5% 遺失 + redundancy 3**：5 個 seed 全過，且**高度 = 1100，一列都沒少**（M0 S09 的 raw 模型少了 51 列）。
- FEC（span 3、entries 1）+ ECM + 5% 遺失：5 個 seed 全過（FEC 補回 29–44 個，各有 2–4 個 Gap 由 ECM 自己重傳吸收）。
- 無遺失時每種 recovery 模式（none、red0、red3、fec 2×2）皆透明：`duplicates = malformed = gap = 0`。
- 對照組本身有斷言（至少 1 個 seed 在 raw 下失敗），避免場景太簡單而失去意義；實測 5/5 失敗。
- 限制：像素等級的比對仍待 t4（M2 後段）；此處斷言兩端完成碼 0、頁數、寬、高。

## 4. U01：SpanDSP 的 UDPTL API
- `libspandsp.so.4`（3.1.1）**不匯出任何 `udptl_*` 符號**，標頭也沒有安裝 `udptl.h`；原始碼樹 `src/` 只有 `t38_core.c`。UDPTL 只出現在 `tests/udptl.c`/`udptl.h`（測試程式用的參考實作，作者為 SpanDSP 作者，GPL-2；是 Asterisk udptl.c 的後裔）。
- 因此「函式庫有可用 API」不成立。但這份參考實作是目前最有價值的獨立對照（尤其是 FEC 線上格式），所以仍做 FFI：
  - `docker/test-rocky9.Dockerfile`：在刪除原始碼前，把 `tests/udptl.c`、`udptl.h` 複製到 `/usr/local/share/spandsp-udptl/`（映像重建一次，約 3 分 17 秒）。
  - `tools/spandsp-sys/build.rs`：用 `cc` 把它編成靜態庫（只用系統的 `cc`/`ar`，**未新增 crate**）；找不到時 panic 並提示重建映像。
  - 仍只在 `tools/`，不進正式執行檔；GPL 程式碼不進 repo，只在測試映像裡。
- 參考實作的限制（測試參數已避開）：緩衝只有 16 格（`span*entries <= 15`）、datagram 上限 400、FEC 超過 `far_max_datagram` 時會截斷 entries（這會讓 receiver 對集合的推算失準；我方 FEC **不截斷**，見偏差 5）。

## 5. K01（sip-core 競態）
- **根因**：我方 re-INVITE 的 200 走「client transaction task → `forward` task → dialog task」，對方在 200 之後立刻送的 BYE 走「server transaction → dialog task」，兩條路徑跳數不同。200 在線上先到，但 BYE 可能先進 dialog task 的輸入佇列；dialog 處理 BYE 時 `terminate()` 把進行中的 re-INVITE 以 `Err(Terminated)` 結束。
- **重現（修正前）**：
  ```
  k01_reinvite_200_and_bye_in_the_same_instant_still_report_accepted
  panicked: round 0: expected Accepted, got Err(Terminated)
  ```
  在 paused time 的 detached 測試中，兩個資料包背靠背注入（中間沒有排程點）即可**確定性重現**。M1d 的 `real.rs` workaround 移除後，在真實 socket 上 15 次中有 **12 次失敗**。
- **修正**（`crates/sip-core/src/dialog/task.rs`）：dialog task 擁有自己的 `input_rx` 與 `backlog`；處理對方 BYE 時（BYE 的 200 已先送出），若有進行中且未取消的 re-INVITE，先呼叫 `absorb_inflight_reinvite_events()`：最多讓出排程 64 次，每次把已到達的 INVITE client transaction 事件依序處理（2xx → `Accepted`），其他輸入放進 `backlog` 保持順序，最後才 `terminate()`。沒有計時器，所以 BYE 的處理在虛擬時間上不延遲。
- **結果**：K01 兩個測試通過（20 輪迴圈；另一個確認「只有 BYE 沒有 200」仍立刻回 `Terminated` 且 BYE 的 200 在同一虛擬毫秒送出）；`real.rs` 無 workaround 連跑 30 次 0 失敗；M1 的 G/C 測試全數維持通過。
- **誠實說明**：這是「有限度的讓出排程」而非形式上的保證。若 200 在別的 worker thread 上被延遲超過 64 次 yield 仍未進入佇列，BYE 還是會贏。根本解法是讓 transaction 層與 dialog 共用單一有序佇列（M6 評估）。我曾先試過 yield 後再 `sleep(2ms)` 的版本（對跨執行緒更穩），但它延後了 BYE 的結果，使 M1 的 `c02_each_call_is_bound_to_the_trunk...` 失敗（`left: [call-b, call-c]`，少了 call-a 的結果），故改為只 yield。

## 6. `[media] port_range`（規格 §4）
- 設定：`port_range = "30000-30999"` 取代 `t38_port`；格式、低>高、含 port 0、不含任何偶數埠皆有明確錯誤；舊鍵 `t38_port` 仍被讀入（`deny_unknown_fields` 之下）但**驗證時報錯**：`[media] t38_port = 30000 is no longer supported: ... Replace it with port_range = "30000-30999"`（測試斷言訊息含 `t38_port`、`port_range` 與建議範圍）。`conf/sip-faxd.toml.example` 已更新，`sip-faxd config check` 吃新範例。
- 配置：`crates/fax-in/src/ports.rs` 的 `PortPool`/`PortLease`：只發範圍內的偶數埠，FIFO 重用（盡量晚才重用同一埠，降低前一通延遲封包打到下一通），`Drop` 歸還，因此不論通話怎麼結束（BYE、取消、關機）都會釋放。控制器在 `drive()` 中（Require、SDP 檢查之後、回應之前）取得 lease，整通電話共用同一埠（初始 answer、對方 re-INVITE 的 answer、我方 re-INVITE 的 offer 都用它；`T38Negotiated.local_port` 即此埠）。
- 耗盡：記錄 `error`、回 **503**，通話結果 `FailureReason::NoMediaPort`（新）。
- 測試（detached、paused time）：
  - `e07_exhausted_range_answers_503_and_a_released_port_is_reused`：範圍 `30000-30003`（2 個偶數埠）：第 1、2 通得到 30000、30002；第 3 通 503（沒有 180/200，結果 `NoMediaPort`，`answered_at = None`）；重試仍 503；第 1 通 BYE 後第 4 通得到 **30000**；再來第 5 通 503；第 2 通 BYE 後第 6 通得到 **30002**，與第 4 通不同。
  - `a_call_that_is_refused_for_other_reasons_does_not_keep_its_port`：無 SDP 的 INVITE（488）之後，單一埠仍可用。
  - `the_leased_port_is_used_in_our_own_t38_reinvite`：我方 re-INVITE 的 `m=image 30002 udptl t38`。
  - `ports::tests`：只發偶數埠、耗盡 → `None`、釋放、FIFO、不會重複發出。
- 完整的 E07（真實 UDPTL 與 SIPp 三通）要等 M2d 的 T38FaxSession。
- 副作用：SIPp 的 `s10_load_simple_call.xml` 原本寫死 `m=image 30000`，200 通的負載測試因此失敗（`Failed regexp match ... m=image 30398`）；已改成 `m=image 30[0-9][0-9][02468] udptl t38`（其他 SIPp 測試每次啟動新的 sip-faxd，第一通仍是 30000）。

## 7. 涵蓋率、CI
`cargo llvm-cov -p t38 --fail-under-lines 85`：**行 98.50 %**（1463 行、漏 22）；函式 98.56 %。

| 檔案 | 行涵蓋率 | 區域涵蓋率 |
|---|---|---|
| ifp.rs | 100.00 % | 97.60 % |
| per.rs | 97.32 % | 97.39 % |
| receiver.rs | 98.49 % | 99.15 % |
| udptl.rs | 97.65 % | 97.44 % |
| **TOTAL** | **98.50 %** | 98.00 % |

llvm-cov 把同檔案的 `mod tests` 也算進去；排除單元測試程式碼後（lcov 逐行重算）為 **98.29 %**（690/702），漏掉的 12 行是 `per.rs` 的 `BitWriter::{len,is_empty}`、`udptl.rs` 中長封包（≥128 octets）長度計算的分支與少數防禦性分支（`receiver.rs` 1 行）。

其他：`sip-core` 93.3 %（門檻 85）、`fax-in` 93.39 %（門檻 70；新檔 `ports.rs` 100 %，`media.rs` 97.08 %，`config.rs` 92.48 %）。

CI 變更：`.github/workflows/ci.yml` 的 coverage job 與 `scripts/ci-local.sh` 加入 `cargo llvm-cov -p t38 --fail-under-lines 85` 與 `-p t4 --fail-under-lines 85`；`.github/workflows/fuzz.yml` 改用 `include` 矩陣，新增 `t38/ifp_decode`、`t38/udptl_decode`；`crates/t38/fuzz`（獨立 workspace，nightly + libfuzzer-sys，與 sip-core 同樣設定）已加入根 `Cargo.toml` 的 `exclude`。fuzz target 除了「不 panic」，還斷言「解碼成功者必能重新編碼，且再解碼結果相等」。

`scripts/ci-local.sh`（最後一次完整執行）：fmt、clippy `-D warnings`（整個 workspace）、`cargo test --workspace --doc`、sip-core / fax-in / t38 涵蓋率、zigbuild musl + 靜態連結檢查、`scripts/e2e-docker.sh` 全部通過；**唯一失敗是 `cargo test --workspace --all-targets` 中 `crates/t4/tests/oracle.rs::f08_pdf_is_valid_for_poppler_and_renders_pixel_exact`**：主機沒有 `pdfinfo`（`oracle tool pdfinfo is not available ... or run the tests with T4_ORACLE=skip`）。這是另一位 agent 進行中的 t4，非本階段造成，我沒有處理；因 `cargo test` 在第一個失敗的 binary 就停止，我改用 `--no-fail-fast` 分段跑完其餘步驟（結果如上）。`-p t4` 涵蓋率閘門未能在這台主機驗證。

## 8. 失敗與修正（原始輸出摘要）
1. **tools 第一次建置**：`spandsp-sys: /usr/local/share/spandsp-udptl/udptl.c not found ... rebuild the image with scripts/e2e-docker.sh`。根因：映像尚未重建（我的 `dk.sh` 沒有 `docker build`）；重建後通過。
2. **U05 第一版**：`packet 0 arrived but was not delivered`（mode = None，封包 0 的 jitter 讓它排在 1、2 之後到達）。根因：測試的期望太強——接收端的起點是它看到的第一個資料包，之前的亂序封包本來就屬於「遲到」。修正測試：只要求「起點之後」的到達者全數交付，並在 `docs`／註解寫明。
3. **U05 第二版**：`loss runs <= depth must not produce gaps`（redundancy 1，序列 0, ~1, 3, 2）。**這次是實作缺陷**：重複（已因 redundancy 取得）的主封包被整包丟棄，包括它自己的 secondary，而那份 secondary 正好含有仍缺的封包 1。修正：重複封包仍處理其 recovery 資料（`receiver.rs`），之後 30000 例全過。
4. **`ifp::tests::field_too_long_is_rejected`**：`left: [192, 1, 128, 255, 254] right: [..255, 255]`。測試期望值算錯（`len-1 = 65534 = 0xFFFE`）；改測試。
5. **SIPp s10**：見 §6（`m=image 30398` 不符 `m=image 30000`）。
6. **fax-in `c02_each_call_is_bound_to_the_trunk...` 失敗**：見 §5（2 ms 寬限 sleep 的版本）。
7. **一次異常的 sip-core 涵蓋率讀數**：`TOTAL ... 83.50%`（低於 85 閘門）。同一命令之後連跑 7 次都是 93.3 %，`cargo test -p sip-core -p fax-in` 連跑 10 次也無失敗。推測是該次有某個時間敏感的 socket 測試在另一位 agent 同時編譯／測試造成的負載下失敗，使後面的測試 binary 沒有執行（`cargo llvm-cov` 在第一個失敗 binary 後即停止）。我沒能抓到是哪一個測試，也無法排除是既有的 flake；列為未解事項。

## 9. 與規格／計畫的偏離與取捨
1. **PER 與 SpanDSP 的差異**（對照 ASN.1 `T38(2002)`，已與 SpanDSP `t38_core.c` 原始碼比對；本 crate 採**純 ASN.1/X.691**）：
   - `field-type` 的 extension addition（jm/ci/v34rate，index 1–3）：純 PER 是 `present, ext=1, 0, 6-bit index`，index 的最低位元落在下一個 octet 的 bit 7（例如 `jm-message` = `c0 01 c0 80 00 00 aa`）。SpanDSP 的編碼（`(idx >> 2)` 與 `(idx << 6) & 0xC0`）沿用 indicator 的版面，對 cm-message（index 0）相同，對 jm/ci/v34rate 不同，看起來是 SpanDSP 的缺陷。fax-in 不使用 V.34 full-duplex，影響僅限這三種 field；黃金向量中不會出現。
   - 沒有 data 的 field 後面又接 field（v1+）：純 PER 緊密排列（5 bit 一個），SpanDSP 每個 field 補滿 octet。只有「最後一個」無 data 的 field 兩者相同；SpanDSP 實際發出的封包都屬於這種（7026 個封包全數逐 byte 相同可證）。**對外部（非 SpanDSP）對端以純 PER 為準。**
   - SpanDSP 對「indicator 帶 data field」回錯誤，並容忍「t30-data 沒有 data field」；本 crate 同樣拒絕前者（`DataFieldWithIndicator`），後者解成空的 `fields`。
   - Data-field 的 count 為 0（存在但空）→ 解成空 `fields`，重新編碼會變成「不存在」（非 canonical，屬於寬鬆接受）。
2. **版本語意**：V0 = 1998 ASN.1（field-type 無 extension bit）；V1 = 1998 之後的可擴充語法，**但 addition 需 V2+**，編碼端與解碼端對 V0/V1 都拒絕 addition（`NotInVersion`）。計畫 I03 只要求 V0；V1 的嚴格度是我的判斷（規格寫 addition 是 v2 才有），請 Opus 確認。
3. **FEC 恢復的長度**：XOR 無法得知原長，恢復的封包長度 = 集合中最長者（補零）。`Source::Fec` 標示；提供 `Ifp::decode_padded` / `decode_prefix` 處理。SpanDSP 參考實作行為相同。
4. **接收端沒有計時器**：規格要「Gap 不可以卡住」。純函式下以兩個機制達成：(a) 最新封包領先 `next` 達 `reorder_window` 時放棄等待；(b) 提供 `flush()`，驅動端在重排逾時或串流結束時呼叫。「delivered + gap == 送出」只對「第一個到最後一個交付」成立；尾端遺失（之後沒有任何封包）本質上無法得知。
5. **FEC 不截斷**：SpanDSP 參考實作在超過 `far_max_datagram` 時丟掉最後幾個 FEC entries，但接收端又用 entries 數推算集合，兩邊不一致；我方 `UdptlSender` 的 `max_datagram` 只限制 redundancy，FEC 完整送出。
6. **接收端起點**：第一個資料包的 secondary 一併交付（封包 0 遺失時，封包 1 帶回 0；晚加入時也帶回最近的 N 個）。序號倒退超過 1024 → resync（`stats.resyncs`），避免對端重設序號後永遠被當成遲到。
7. **SpanDSP `count` 的處理**：raw 模型是同序號重複 `count` 份；UDPTL 模型每份各自成為新序號的資料包（FreeSWITCH 的做法）。
8. **黃金向量的產生方式**：用 `T38Terminal` 的 tx handler 擷取（IFP，尚未加 UDPTL）；一個檔案一個場景的長度前綴 bundle（`*.ifpb`），不是一封包一檔（共 7026 個）。格式與場景表在 `crates/t38/tests/vectors/spandsp/README.md`。SpanDSP 在純傳真會話中不會發出 v2+ 的 addition，這些碼點由 round-trip、屬性測試與手算 bytes 涵蓋。
9. 新增 `scripts/gen-t38-vectors.sh`；`fax-sim` 新增 `bin/gen_t38_vectors.rs`、`T30Config.modems`、`T38LoopbackConfig::{t38_version, modems, udptl, capture_ifp}`、`run_detailed()`；未使用的 `set_tep_mode` 仍未使用。
10. **未新增 workspace 依賴**：`t38` 只用既有的 `thiserror`（dev：`proptest`）；`crates/t38/fuzz` 的 `libfuzzer-sys` 與 sip-core 的 fuzz crate 相同（獨立 workspace）；`tools` 新增 `t38` path 依賴（其傳遞依賴 `thiserror` 已在 `tools/Cargo.lock`）；`spandsp-sys` 用 `cc` 命令列而非 `cc` crate。
11. **測試計畫文字**：U01 寫「與 SpanDSP `udptl` 產生的 bytes 相同（若 SpanDSP 3.1 有提供 udptl API 就做 FFI 對照，否則記錄理由，並以附錄範例驗證）」。實際是「函式庫沒有 API，但原始碼樹有參考實作，所以仍做了 FFI 對照」，比計畫要求的更強；T.38 附錄本身沒有給 byte 範例，故改以手算範例加參考實作。

## 10. 擬議 ADR（請 Opus 合併到 `docs/DECISIONS.md`，編號自行調整）

### ADR-014：t38 crate 的 PER／UDPTL 設計與 SpanDSP 差異（2026-10-21）
**背景**：規格 §1 要求 T.38 v0–v3 的 IFP PER 與 UDPTL；SpanDSP 的編碼在少數地方偏離 X.691。
**決策**
- IFP 採純 ASN.1 `T38(2002)` / ALIGNED PER：`field-type` 在 V0 為 3 bit 無 extension bit，V1+ 為 ext bit + 3 bit；addition 一律用 normally-small number（`0` + 6 bit）。V0/V1 拒絕所有 addition，V2+ 接受。
- 與 SpanDSP 不同之處（已記錄）：jm/ci/v34rate 的 index 版面、V1+ 中無 data 的非末尾 field 緊密排列；SpanDSP 實際發出的 7026 個封包全數逐 byte 相同。
- UDPTL 的 FEC 沿用 SpanDSP／Asterisk 的事實標準（entry m 為 `n+m-span*entries+k*entries`、k=0..span 的 XOR，暖機規則相同），並以參考實作逐 byte 驗證（285 個資料包）。
- 接收端為純狀態機：`on_datagram()` + `flush()`；以 64-bit 展開序號處理 wrap；FEC 恢復的封包帶零填補（`Source::Fec`、`Ifp::decode_padded`）。
**代價**：與外部以純 PER 為準的對端相容，與 SpanDSP 在上述三種罕見 field 不相容（fax-in 不用 V.34）。

### ADR-015：port_range 與每通電話的 UDPTL 埠（2026-10-21）
- `[media] port_range = "low-high"` 取代 `t38_port`；舊鍵給明確錯誤訊息（不靜默相容）。只發範圍內的偶數埠，FIFO 重用，`Drop` 歸還。
- 每通電話在回應前取得一個埠，整通共用；耗盡 → 503 + `error` 日誌 + `FailureReason::NoMediaPort`。
- `audio_port` 暫維持單一固定值（G.711 passthrough 在 M5）。

### ADR-016：K01 的處理方式（2026-10-21）
- dialog task 處理對方 BYE 前，若有進行中的 re-INVITE，先讓出排程最多 64 次並吸收已到達的 client transaction 事件，使「200 先到」的結果回報為 `Accepted`。不使用計時器，BYE 的處理在虛擬時間上不延遲。
- 這是有限度的保證；根本解（單一有序佇列）留待 M6 評估。

### ADR-017：測試映像保留 SpanDSP 的參考 UDPTL（2026-10-21）
- `docker/test-rocky9.Dockerfile` 在映像內保留 `tests/udptl.c`/`udptl.h`（GPL-2，僅在測試映像、不進 repo、不進正式執行檔），由 `spandsp-sys/build.rs` 編成靜態庫供 U01 使用。理由：libspandsp 本身不匯出 UDPTL，而它是 FEC 線上格式唯一的獨立對照。

## 11. 未解事項
- t4 / `-p t4` 涵蓋率閘門、`scripts/ci-local.sh` 的 `cargo test --workspace` 因 t4 的 oracle 測試需要 `pdfinfo` 而在這台主機失敗（另一位 agent 的範圍）。
- K01 的保證有限（見 §5）；以及 §8-7 那次無法歸因的異常涵蓋率讀數。
- X06 目前只比對頁數與尺寸，不比像素（待 t4 解碼器，M2 後段改為逐像素）。
- raw 模型與 UDPTL 模型不模擬重排與抖動（接收端的重排已有單元／屬性測試涵蓋，但 fax-sim 尚未在 SpanDSP 會話層級驗證）。
- 尚未實作 fax-in 側的 UDPTL socket、latching 與 `udptl.log` 擷取（M2d 的 T38FaxSession）；`UdptlSender/Receiver` 的預設參數（redundancy 深度 3、`reorder_window` 32、`max_datagram` 1400）尚未與 SDP 的 `T38FaxMaxDatagram`／`T38FaxUdpEC` 連動。
- `cargo-deny` 本機仍未安裝（同 M1）。

## 12. 異動的檔案
- 新增：`crates/t38/{Cargo.toml,src/{lib,per,ifp,udptl,receiver}.rs,tests/{golden,properties}.rs,tests/properties.proptest-regressions,tests/vectors/spandsp/*.ifpb,tests/vectors/spandsp/README.md,fuzz/**}`、`crates/fax-in/src/ports.rs`、`crates/fax-in/tests/ports.rs`、`tools/fax-sim/src/bin/gen_t38_vectors.rs`、`tools/fax-sim/tests/{x06_udptl,u01_udptl_spandsp}.rs`、`scripts/gen-t38-vectors.sh`。
- 修改：`crates/sip-core/src/dialog/task.rs`、`crates/sip-core/tests/dialog.rs`（K01）；`crates/fax-in/src/{config,controller,media,event,lib}.rs`、`crates/fax-in/tests/{config,real}.rs`；`conf/sip-faxd.toml.example`；`tools/fax-sim/src/{t38_loopback,spandsp}.rs`、`tools/fax-sim/Cargo.toml`、`tools/spandsp-sys/{build.rs,src/lib.rs}`、`tools/sipp/s10_load_simple_call.xml`；`docker/test-rocky9.Dockerfile`；`.github/workflows/{ci,fuzz}.yml`、`scripts/ci-local.sh`、根 `Cargo.toml`（`exclude` 加 `crates/t38/fuzz`）、`CLAUDE.md`（涵蓋率門檻一行）。

## Opus 審核結論（2026-10-21）
**結論：通過。**

整合 M2a 和 M2b 之後，重新執行 `scripts/ci-local.sh`，全部通過。T.4 的對照測試（oracle）已經改在 Docker 中實際執行。

已檢查：
- **I01**：SpanDSP 3.1.1 真實會話中的 7026 個 IFP 封包，重新編碼後逐 byte 相同，涵蓋 non-ECM/ECM、v0/v3、三種調變。mutation 測試也能抓到錯誤。
- **U01**：與 SpanDSP 的參考 UDPTL 逐 byte 比對 285 個資料包，雙向互通，都能補回遺失。
- **X06**：M0 時失敗的 5 個 seed（ECM 加 5% 遺失），加上 redundancy 後全部通過。對照組（不做補回）則全部失敗，證明這個情境確實需要 redundancy。
- **授權**：GPL-2 的 `udptl.c` 只保留在測試映像中，repo 裡沒有這個檔案，也不會進正式執行檔。已確認 repo 內沒有任何 GPL 原始碼。

確認的事項：
- V1 拒絕 extension 值。依 T.38(2002) 的 ASN.1，extension 的追加型別從 V2 才開始，V1 不允許，所以接受這個做法。
- **K01**：讓出排程最多 64 次的做法只是有限度的保證（ADR-016）。實際的 gateway 不會在 200 之後的同一毫秒就送 BYE，所以先接受。根本解是改成單一有序佇列，已列入 M6 前的待辦。

需要追蹤：
- 有一次 sip-core 的涵蓋率只有 83.5%，可能是某個測試在高負載下提早結束，但原因沒有查清楚。列入 M2c 調查：在 CI 上連跑涵蓋率檢查，記錄每個測試的結果，找出不穩定的測試。
