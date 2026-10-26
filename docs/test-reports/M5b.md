# M5b 測試報告：HDLC 與 V.21（對 SpanDSP `hdlc_tx`／`hdlc_rx`／`fsk_tx`／`fsk_rx`、音訊引擎的半雙工）

- 實作：Opus／Haiku（`crates/fax-modem/src/hdlc.rs`、`v21.rs`、`crates/fax-in/src/audioengine.rs`）；測試與本報告：Sonnet（依 Opus 的測試計畫）；本次沒有產品程式碼的修正（§3）
- 計畫：`docs/test-plans/M5.md` §1、§5（H01–H03、V01–V03、V05；V04 不在本次範圍）
- 分支：`m5`
- 範圍：H01–H03、V01–V03、V05。**不在範圍**：V04（前導偵測事件的時間）、其他分期。
- 狀態：待 Opus 審核

## 1. 環境

| 項目 | 內容 |
|---|---|
| 主機 | Windows 11（Docker Desktop）；Rust 1.99.1 |
| 純 Rust 測試 | `crates/fax-modem/tests/m5b_h.rs`（H01–H03，21 個）、`m5b_h03_memory.rs`（H03 記憶體上限，1 個，自帶計數用的 global allocator，所以獨立成一個測試檔）、`m5b_v.rs`（V01、V02 的第二來源與 V21Rx 的 proptest，8 個）、`crates/fax-in/tests/m5b_v05.rs`（V05，8 個，其中 1 個 `#[ignore]`）。原生執行（`T4_ORACLE=skip`），合計約 10 秒 |
| SpanDSP 測試 | `tools/fax-sim/tests/h_hdlc_spandsp.rs`（H01、H02，7 個，約 0.2 秒）、`v_v21_spandsp.rs`（V01–V03，14 個，其中 1 個 `#[ignore]`，單執行緒約 50 秒，debug 組態）；共用的 SpanDSP 包裝與通道模型在 `tools/fax-sim/tests/m5b_common/mod.rs` |
| 執行環境 | Rocky 9 映像 `sip-fax-rs-test:rocky9`（SpanDSP v3.1.1），`scripts/e2e-docker.sh`；CI 的 `e2e-spandsp` job 跑整個 `tools/` workspace，新檔案自動納入（不需改 `ci.yml`） |
| SpanDSP FFI | `tools/spandsp-sys/src/lib.rs` 補 `fsk_spec_t`、`preset_fsk_specs`（用 `FSK_V21CH2`）、`fsk_tx_init/free/power`、`fsk_tx`、`fsk_rx_init/free`、`fsk_rx_set_signal_cutoff`、`fsk_rx`、`hdlc_tx_init/free`、`hdlc_tx_frame`、`hdlc_tx_flags`、`hdlc_tx_get_bit`、`hdlc_rx_init/free`、`hdlc_rx_put_bit`。簽名只讀 Rocky 映像裡的 `fsk.h`、`hdlc.h`、`async.h`，沒有讀 SpanDSP 原始碼。只在 `tools/`，主執行檔不含 SpanDSP |
| fuzz 目標 | `crates/fax-modem/fuzz/`（仿 `crates/t4/fuzz`，自己的 `[workspace]`），目標 `hdlc_deframe`；根 `Cargo.toml` 的 `exclude` 與 `.github/workflows/fuzz.yml` 的矩陣已加入。`cargo check` 通過（本機沒有 nightly，沒有實際跑 libFuzzer，由夜間 workflow 跑） |
| 隨機性 | 固定 seed，每個條件 3 個 seed：SpanDSP 測試 `0x5B01_000x`（H）、`0x5B02_000x`（V01–V03）；純 Rust `0x5B01_100x`、`0x5B02_100x`；V05 `0x5B05_000x`；proptest 以 ChaCha 固定 seed（1、2、3）、關閉 failure persistence |
| 通道模型（`Chan`） | 與 `a_v27ter_spandsp.rs`、`b_v29_spandsp.rs` 相同：電平（對訊號有效段量測，dBm0）、白色高斯雜訊（SNR 為訊號功率比 0–4 kHz 全頻帶雜訊功率，訊號前後各 200 ms 含雜訊）、頻率偏移（255 階 Hilbert）、時脈偏差（16 階加窗 sinc）。測試內不含 G.711 來回（V.21 的案例不需要） |
| 條件的組成 | 每個 V 條件：3 seed × 14 個 burst × 5 個 frame = 210 個 frame（SpanDSP 測試），burst 是 1 s 前導（38 個 flag）＋5 個 T.30 樣式的 frame（FF 13／03、FCF、0–30 個資訊 octet，每 4 個有 1 個是滿 1 的資料，把填充逼到極限）。每個 burst 一個新的收端（引擎也在每次訊號出現時重建收端） |
| 收端的前置 | V01–V03 把收端放在引擎的「訊號存在偵測」之後：只餵訊號前後各 10 ms 的音訊（`Heard::gated`）。**不這麼做時**，訊號前後的 200 ms 雜訊會讓 V21Rx＋HdlcRx 吐出雜訊產生的假 frame（V01 SNR 15 dB 時每 210 個 frame 多 9–21 個 `fcs_ok = false` 的 frame，沒有一個是好的 frame），這不是產品行為（引擎的 `PresenceDetector` 不會把雜訊交給 V21Rx）。見 §4 ⑥ |

## 2. 案例對照表

| ID | 測試位置 | 做法與驗證內容 | 結果 |
|---|---|---|---|
| H01 | `h_hdlc_spandsp.rs::h01_every_two_octet_frame_has_the_bits_spandsp_makes` | **全部 65 536 個 2 octet frame**（涵蓋每一種「前一個 octet 的結尾、下一個的開頭、其後 FCS」的填充邊界）放進一個 burst（38 個前導 flag，2 個結尾 flag）：我方 `encode_burst` 與 SpanDSP `hdlc_tx` 的位元流逐 bit **相同**（2 652 247 bit） | 通過（Docker） |
| H01 | `…::h01_crafted_frames_one_by_one_and_together` | 13 個刻意設計的 frame（五個 1 在 octet 尾或頭、資料就是 0x7E、`FF×40`、`00×10`、六個與七個 1 跨 octet、`F8 1F` 重複、0..255 全序列、255 個 0xAA 等），逐個與合在一起：位元流相同 | 通過（Docker） |
| H01 | `…::h01_random_frames_in_three_seeds` | 3 seed × （500 個 T.30 樣式 frame；300 個 1–260 octet 的隨機 frame × 前導 1／2／38／43 個 flag）：位元流相同。SpanDSP 的結尾 flag 在它要下一個 octet 時結束，不在 flag 邊界，所以比較時 SpanDSP 多出的尾端必須是 flag 的位元 | 通過（Docker） |
| H01 | `m5b_h.rs::h01_*`（5 個，純 Rust） | 第二個獨立寫的編碼器（以標準的電路描述：16 bit 移位暫存器，預設全 1，由 x¹⁵ 端送出；逐 bit 填充）對我方 `encode_burst`：隨機 frame 3 seed × 300 個、全部 65 536 個 2 octet frame、FCS 公開檢查值（"123456789" → 0x906E）、固定位元向量、frame 內沒有連續 6 個 1 | 通過 |
| H02 | `h_hdlc_spandsp.rs::h02_our_receiver_decodes_spandsp_bursts_with_a_one_second_preamble` | SpanDSP `hdlc_tx` 的 burst（前導 38 個 flag ≈ 1 s，1–4 個 frame），3 seed × 40 burst：我方 `HdlcRx` 先回 `Preamble`（恰一次），再逐一回出與送出相同的 frame，`fcs_ok = true`，沒有 `Abort` | 通過（Docker） |
| H02 | `…::h02_one_long_stream_of_many_bursts_and_idle_flags` | 100 個 burst 接在一起，burst 之間 1–20 個閒置 flag：300 個 frame 全對 | 通過（Docker） |
| H02 | `…::h02_fcs_verdicts_agree_with_spandsp_on_damaged_streams` | SpanDSP 的 burst 隨機翻 1 個 bit，400 次：我方判 `fcs_ok = false` 的 frame 有 390 個；我方判為好的 frame 都是真的送過的、且**與 SpanDSP `hdlc_rx` 判為好的 frame 集合相同**（400 次全部一致） | 通過（Docker） |
| H02 | `…::h02_spandsp_decodes_our_bursts_too` | 反方向：我方 burst 3 seed × 200 個 frame 給 SpanDSP `hdlc_rx`：全部好、內容相同 | 通過（Docker） |
| H02 | `m5b_h.rs::h02_*`（4 個，純 Rust） | 1 s 前導＋1–5 個 frame、長度 1..20 與 259、260 的 frame、閒置 flag 1–6 個與第一個 flag 前的垃圾、`reset()` 後重新收 | 通過 |
| H03 | `m5b_h.rs::h03_a_wrong_fcs_is_reported_…`、`h03_a_wrong_fcs_value_gives_fcs_ok_false_with_the_data` | 一個 frame 的 burst 逐 bit 翻轉（每個位置一次）：**沒有任何一次出現資料不同的好 frame**；FCS 欄位錯誤值 3 seed × 200 次：回 `Frame { data = 原資料, fcs_ok = false }` | 通過 |
| H03 | `…::h03_abort_seven_ones_drops_the_frame_and_the_next_frame_is_fine`、`h03_abort_when_no_frame_is_open_…` | frame 中間插入 7–15 個 1：回 `Abort`、被中止的 frame 不輸出，接著的 flag 與下一個 frame 正常（100 次）；沒有開著的 frame 時的 7 個 1 不回 `Abort`、之後恢復 | 通過 |
| H03 | `…::h03_short_frames_are_never_good`、`h03_a_frame_that_is_not_a_whole_number_of_octets_…` | 0、1、2 個 octet（含只有 FCS 的 frame）：`fcs_ok = false`；一個 octet 的資料是最短的好 frame；加 1–7 個多餘 bit：不會是好 frame（7 × 30 次） | 通過 |
| H03 | `…::h03_overlong_frames_are_dropped_to_bad_and_bounded` | 260 octet 是好；261、262、400、1000、70 000 octet：`fcs_ok = false`，保留的資料 ≤ `MAX_FRAME`，下一個 frame 正常 | 通過 |
| H03 | `…::h03_noise_before_a_burst_does_not_spoil_it` | burst 前 0–3000 個隨機 bit：burst 的 3 個 frame 全部是好的（3 seed × 50 次） | 通過 |
| H03 | `…::h03_proptest_*`（4 個） | proptest（3 seed × 200–300 組）：任意位元流（任意位元、flag、連續 1 或 0、剛好含填充的 frame、200–400 octet 的長 frame 拼接）、純任意 20 000 bit：不 panic、frame 資料 ≤ `MAX_FRAME`、事件數 ≤ 位元數的一半、被叫「好」的 frame 重新編碼再收還是好的；任意 frame 3 seed × 200 組往返；單 bit 翻轉不產生資料不同的好 frame | 通過 |
| H03 | `m5b_h03_memory.rs::h03_memory_stays_bounded_on_streams_that_never_end_a_frame` | 計數用的 global allocator 量尖峰記憶體。6 種各 2000 萬 bit 的輸入：一個 flag 之後永遠不結束的資料（0xAA）、全 1、全 0、剛好含填充但沒有 flag 的隨機資料、純隨機位元、600 octet 開著的 frame 接 abort 循環：每一種比起起點的尖峰增量都 < 64 KiB（限制值）；並斷言計數器有在計（隨機位元那組的尖峰 > 0） | 通過 |
| H03 | `crates/fax-modem/fuzz/fuzz_targets/hdlc_deframe.rs` | cargo-fuzz：任意位元組當位元流餵 `HdlcRx`（不 panic、frame ≤ `MAX_FRAME`、好 frame 重新編碼再解碼相同）。已加入 `fuzz.yml` 矩陣 | 已建、`cargo check` 通過，未實跑（見 §1） |
| V01 | `v_v21_spandsp.rs::v01_levels_minus_10_to_minus_43_and_snr_15_to_30` | SpanDSP `hdlc_tx` → `fsk_tx`（V.21 通道 2）→ 通道 → 我方 V21Rx＋HdlcRx。電平 −10／−20／−30／−43 dBm0 × SNR 15／20／30 dB，每個條件 **210 個 frame：210 個全對、FCS 錯誤 0、假的好 frame 0**（12 個條件） | 通過（Docker） |
| V01 | `…::v01_clean_signal_at_every_level`、`v01_snr_12_db_has_at_most_one_percent_errors` | 無雜訊四個電平：210／210；SNR 12 dB（−10、−43 dBm0）：**210／210，錯誤率 0 %**（上限 ≤ 1 %） | 通過（Docker） |
| V01 | `…::v01_the_error_rate_curve_below_the_plan_is_recorded` | 記錄：−20 dBm0 在 SNR 10／8／6 dB 仍然 210／210（沒有要求） | 通過（只記錄） |
| V01 | `m5b_v.rs::v01_*`（純 Rust，第二來源） | 另一個獨立寫的 FSK 產生器（自己的相位累加器、位元時脈由取樣序號算、內建 ppm 與頻偏）：同樣 12 個電平×SNR 條件，3 seed × 14 burst × 3 frame = 126 個 frame／條件：全對、0 錯；SNR 12 dB 0 錯；20 秒純雜訊（−10／−30／−43 dBm0 × 3 seed）與 3000 bit 的單音：**沒有好的 frame** | 通過 |
| V02 | `…::v02_clock_deviation_and_frequency_offset` | ±100 ppm、±10 Hz 各別與四種同號／異號組合（8 組）× 電平 −10／−30／−43 dBm0，SNR 20 dB，每個 210 個 frame：**24 個條件全部 210／210，FCS 錯誤 0**（上限：同 V01） | 通過（Docker） |
| V02 | `…::v02_deviations_without_noise_and_with_a_lot_of_it`、`v02_the_limits_of_the_deviations_are_recorded` | 無雜訊與 SNR 15 dB 的最差組合（±100 ppm＋±10 Hz）：210／210。記錄：±500 ppm、±20 Hz、+40 Hz（SNR 20 dB）仍然 210／210（計畫只要求 ±100 ppm／±10 Hz） | 通過 |
| V02 | `m5b_v.rs::v02_*`（純 Rust，第二來源） | 同樣 8 組偏差 × 3 電平，126 個 frame／條件：全對；±2000 ppm（記錄）：全對。**把位元時脈迴圈增益改成 0 的變異體在 ±2000 ppm 失敗，在 ±100 ppm 不失敗**：±100 ppm 在 1.5 s 的 burst 只讓位元時脈偏 0.04 bit，所以 V02 的計畫條件本身對時脈迴圈不敏感（見 §4 ⑤） | 通過 |
| V03 | `…::v03_spandsp_receives_our_frames` | 我方 `V21Tx`（−10／−20／−30／−40 dBm0）→ SpanDSP `fsk_rx` → SpanDSP `hdlc_rx`：每個電平 **210／210 全對，FCS 錯誤 0** | 通過（Docker） |
| V03 | `…::v03_the_tones_are_1650_and_1850_hz` | 以過零點內插量 3000 個 bit（10 s）的頻率：**mark 1649.999 Hz、space 1850.001 Hz**（−10 與 −30 dBm0 同），±1 Hz 的要求有 3 個數量級的餘裕；同一方法量 SpanDSP `fsk_tx`：1649.999／1849.947 Hz（證明量測有解析度）；相位連續：換 bit 處最大樣本跳變不超過頻率允許的值 | 通過（Docker） |
| V03 | `…::v03_our_preamble_is_one_second_plus_or_minus_15_percent` | 引擎（預設 `preamble_ms = 1000`）送出的 DIS burst（量 T4 重送的那一個，因為第一個 DIS 直接接在 CED 後面、分不開，見 §4 ①；兩者走同一個 `queue_frames`）：從量到的音訊長度（1286.6 ms）扣掉 frame 與兩個結尾 flag：**前導 1.013 s（38 個 flag）**，範圍 0.85–1.15 s | 通過（Docker） |
| V03 | `…::v03_extra_the_gap_between_ced_and_dis_is_recorded`、`v03_extra_ced_is_followed_by_75_ms_of_silence`（`#[ignore]`） | 計畫沒有，量前導時發現：CED（量到 ≥ 2.9 s 的 2100 Hz）之後**沒有靜音**，V.21 前導直接接上（0 ms；T.30 5.3.2 要 75 ± 20 ms）。第一個測試記錄現況（< 20 ms），第二個斷言 55–95 ms，目前失敗，`#[ignore]`。見 §4 ① | **未達成**（`#[ignore]`；不在計畫案例表內） |
| V03 | `…::v03_spandsp_hears_the_tones_as_ones_and_zeros`、`v03_spandsp_receives_our_frames_through_noise_and_deviations` | 方向檢查（300 bit 的 1／0／1：SpanDSP 收到 300 個 1、155 個 0，沒有反相）；額外：同一個通道（SNR 20 dB，±100 ppm／±10 Hz）加在我方送端與 SpanDSP 自己的送端後面，SpanDSP 收端：兩者都 210／210 | 通過（Docker） |
| V05 | `m5b_v05.rs::v05_our_own_echo_is_never_a_frame` | 引擎在輸入端收到自己輸出的回音（−10 dB，延遲 0／10／20／40／60 ms，加 −50 dBm0 雜訊，3 seed，seed 也改變第一個步長的相位），整段包含 CED、DIS 與 T4 重送的 DIS：**日誌沒有任何 `V.21 rx` 或 `CNG heard`**，通話沒有結束，T.30 停在 `WaitCommand`（15 個組合） | 通過 |
| V05 | `…::v05_listening_resumes_75_ms_after_our_signal_ends`、`v05_a_peer_that_starts_at_once_is_heard_too_with_a_normal_preamble` | 對方在**我方重送的 DIS**（第二次傳送）結束後剛好 75 ms 開始送 TSI＋DCS（前導只有 4 個 flag，沒有餘裕），回音延遲 0–60 ms：**15 個組合全部收到 TSI 與 DCS**。1 s 前導的對方在 0／30／60 ms 開始：也收到。另掃 0–140 ms（每 5 ms）：第二次傳送之後的每一個間隔都收到 | 通過 |
| V05 | `…::v05_control_an_echo_that_comes_back_after_the_guard_is_heard`、`v05_the_guard_setting_is_not_the_only_protection` | 對照：回音晚 1.5 s（超過 guard，只有 DIS 不含 CED）會被當成對方的 DIS（日誌出現 `V.21 rx DIS`），證明上面的測試有可能失敗。`echo_guard_ms` 設 0 時延遲 60 ms 的回音也沒有 frame（傳送中的聾期已經蓋住回音，回音結束後的尾巴只是 flag），所以 75 ms 是額外餘裕，不是唯一的保護（記錄） | 通過 |
| V05 | `…::v05_after_the_first_transmission_a_peer_from_120_ms_on_is_heard`、`v05_after_the_first_transmission_a_peer_within_85_ms_is_missed_recorded` | **第一次傳送（CED＋第一個 DIS）之後**：對方 120／150／300 ms 後開始：收到。**對方在我方結束後 0–80 ms 內開始（每 5 ms 掃，seed 0）：整個 DCS 都沒有被聽到**；≥ 85 ms 起收到（邊界隨 seed 的步長相位在 80–95 ms 之間移動，所以通過的測試取 ≥ 120 ms）。見 §4 ② | 通過（記錄行為） |
| V05 | `…::v05_plan_a_peer_75_ms_after_the_first_transmission_is_heard`（`#[ignore]`） | 計畫字面：對方在**第一次傳送**結束後 75 ms 開始，回音 0／20／60 ms：目前失敗（DCS 沒有被聽到） | **未達成**（`#[ignore]`） |

## 3. 發現並修正的產品問題

**沒有修正任何產品程式碼。** 發現兩個產品行為問題，都需要設計決定或會動到既有已審核測試，所以只記錄、不修（§4 ①②）。曾經試修 §4 ① 的 CED 靜音（一行：`queue_ced` 的 `off_ms` 改為 `turnaround_ms`），做了又還原，原因在 §4 ①。

## 4. 與計畫不同之處、未達成的項目

① **CED 結束後沒有 75 ms 的靜音，DIS 的前導直接接在 CED 後面（未修；一行就能修，但會讓 M5d 的 M04 11 dB 案例失敗）**。T.30 5.3.2：CED 之後、第一個 V.21 訊號之前要有 75 ms ± 20 ms 的靜音。`audioengine.rs` 的 `queue_ced` 以 `ToneGenerator::new(CED_HZ, level, ced_ms, 0)` 產生 CED（結尾靜音 0），後面的 `queue_frames` 的轉折靜音只相對於對方訊號結束（開機時 `peer_off_at = 0`），所以不補。實測：引擎輸出的 CED 2100 Hz 與 V.21 的 flag 之間 0 ms 靜音（10 ms 區塊的平均振幅 3649 連續到 V.21 開始）。修法：`off_ms` 改為 `cfg.turnaround_ms`（預設 75），加一個單元測試斷言（600 ± 8 個零樣本）。我做了這個修正，`cargo test -p fax-in` 與 V 系列都通過，但跑 Docker 的整個 tools 測試時 `m5d_audio_engine_spandsp.rs::m04_constant_snr_11_db_fails_the_9600_tcf_with_ftt_and_the_call_succeeds_lower` 失敗：整通時間軸移動 75 ms，雜訊的對齊跟著變，seed 0 的第一個 9600 TCF 變成 13 984／14 632 個 0 位元 = 95.6 %（門檻 95 %）→ CFR 而不是 FTT（修正前 94.6–94.9 %，該測試的註解自己寫「at 11 dB the TCF has 94.6-94.9 % zero bits (T.30 needs 95 %)」，也就是這個案例本來就刻意放在門檻邊上，任何時序變動都會翻轉）。要修 CED 就要同時重新調這個案例（換 SNR 或 seed 的做法是設計決定，也不是本分期的測試），所以還原了修正，產品維持原狀。待辦建議：改 `queue_ced`，並把 M04 的 11 dB 案例改成不是靠門檻邊上的雜訊（例如 10.5 dB，並重新掃過各 seed）。以 `#[ignore]` 的 `v03_extra_ced_is_followed_by_75_ms_of_silence` 保留斷言，另有記錄現況的 `v03_extra_the_gap_between_ced_and_dis_is_recorded`。真實對端多半容忍（SpanDSP 呼叫端、`e2e_m5e` 都沒問題），但嚴格的 gateway 可能在 CED 與 V.21 之間要靜音來辨識。

② **V05「送完 75 ms 後恢復收」：第一次傳送之後的 0–80 ms（邊界約 80–95 ms）內開始的對方訊號整個漏掉（未修，需要設計決定）**。引擎一開機就先送 CED＋DIS 約 4.6 s，這期間完全不聽，所以 `PresenceDetector` 第一次被餵到音訊時，對方的訊號可能已經在了。偵測器的設計（`tone.rs` 註解：「the first block only tells the noise (a signal already on then is missed until it ends)」）是把第一個區塊當雜訊底，所以一個一開始就響的訊號被當成雜訊、要等它結束才會重新偵測，**整個 TSI／DCS burst 因此被漏掉**。引擎要等 T4（3 s）重送 DIS 才再給對方一次機會（第二次傳送之後偵測器已經有雜訊底，**所有間隔 0–140 ms 都能收到**，見 §2）。實測：第一次傳送之後對方在 0–80 ms（每 5 ms 一格，seed 0）開始：全漏；85 ms 起：收到；邊界隨引擎第一個步長的相位在 80–95 ms 移動（引擎處理 20 ms 一個封包，聾期用封包粒度結束）。T.30 允許對方在 75 ± 20 ms（55–95 ms）回覆，所以**合規的、回得快的對方（以 SpanDSP 作為呼叫端的 M 系列測試與 `e2e_m5e` 都通過，沒有量它的 DCS 比我方 DIS 結束晚多少）第一次的 DCS 會漏掉，要多等 3 s 的 T4 重送**。計畫字面的案例（75 ms）以 `#[ignore]` 的 `v05_plan_a_peer_75_ms_after_the_first_transmission_is_heard` 保留。**為什麼沒有修**：最直接的修法（在建構引擎時先餵偵測器一個靜音區塊，讓雜訊底是「安靜」）會讓一條從第一個區塊就有 ≥ −43 dBm0 穩定雜訊的線路被當成「訊號一直在」，這正是 M5a N 系列「穩定雜訊不是訊號」案例要避免的；要不要換這個取捨，或改成「只在我方傳送剛結束後的第一個區塊用靜音的雜訊底」，是設計決定。建議：記 ADR，選「引擎建構時以靜音區塊預設雜訊底」（−43 dBm0 以上的穩定雜訊線路本來也收不了 V.21），或在 `AudioEngine` 的聾期結束處理裡，對還沒有雜訊底的偵測器先餵一個靜音區塊。

③ **V03 前導長度的量測定義**：計畫寫「前導 flag 長度 1 s ± 15 %（T.30 5.3.1）」，沒有說明量什麼。我的解讀：引擎實際送出的 DIS burst（T4 重送的那一個，因為第一個緊接在 CED 後面、能量上分不開，見 ①）的在空中長度，扣掉 frame 與兩個結尾 flag 之後的前導位元數 ÷ 300。為了不依賴引擎的內部變數，是從音訊樣本量的（能量上下界到樣本，加上以我方 `V21Rx`＋`HdlcRx` 解出 frame 來算 frame 的位元數）。結果 1.013 s。

④ **V01 的「SNR」定義**：計畫沒有說明雜訊頻寬。與 M5c／M5d 一致：SNR ＝ 訊號功率 ÷ 0–4 kHz 全頻帶白雜訊功率（不是只在 V.21 的頻帶內）。以頻帶內的雜訊算，實際 SNR 還要高約 6–8 dB，所以條件比計畫字面更嚴。結果：SNR 6 dB 仍然 0 錯（記錄），計畫的 12 dB／15 dB 有很大的餘裕。

⑤ **V02 的條件對時脈迴圈不敏感**：±100 ppm 在 1.5 s 的 burst 只讓位元時脈偏 0.04 bit，固定時脈的收端也過得了；把 V21Rx 的位元時脈迴圈增益改成 0 的變異體在 V02 的計畫條件下仍然通過。因此補了 ±2000 ppm 的案例（純 Rust，`v02_deviations_far_beyond_the_plan_…`），變異體在這裡失敗。計畫的條件字面達成。

⑥ **V21Rx＋HdlcRx 直接面對沒有訊號的雜訊會吐假 frame（`fcs_ok = false`）**：V01 在沒有「訊號存在偵測」閘門時，訊號前後各 200 ms 的雜訊讓每 210 個 frame 多出 9–21 個壞 frame（沒有一個是好的 frame；純 Rust 的 20 秒雜訊與單音測試斷言沒有好的 frame）。引擎的 `PresenceDetector` 不會把這種雜訊交給 V21Rx，所以不是產品問題；V01–V03 的收端因此放在訊號存在偵測之後（訊號前後各 10 ms）。純靠 FCS 是安全的（0 個假的好 frame），但如果以後有人在偵測器之外直接用 V21Rx＋HdlcRx，要知道會有壞 frame 事件。

⑦ **H01 SpanDSP 的結尾 flag 不在 flag 邊界**：`hdlc_tx` 是以 octet 為單位拿位元，結尾的 flag 在它要下一個 octet 時結束，所以結尾可能少於或多於 flag 的整數倍幾個 bit；比較時以 SpanDSP 多出的尾端必須是 flag 的位元為準（我方的 `encode_burst` 結尾永遠是整數個 flag）。串接 burst 的 H02 測試以我方 burst 的長度截斷 SpanDSP 的位元（H01 已證明到那裡為止相同）。

⑧ **fuzz：`audio_rx` 目標有、沒在 nightly 矩陣裡**（`crates/fax-in/fuzz/fuzz_targets/audio_rx.rs` 存在，但 `.github/workflows/fuzz.yml` 沒有列它；計畫 §1 寫「nightly fuzz workflow 納入 `rtp_decode`、`hdlc_deframe`、`audio_rx`」）。本次只加了 `hdlc_deframe`；`audio_rx` 與 `rtp_decode` 屬於 M5a／M5e，未動。

## 5. 涵蓋率

`cargo llvm-cov clean --workspace` 後，`cargo llvm-cov -p fax-modem --fail-under-lines 85`：

| 檔案 | 行涵蓋率 |
|---|---|
| `hdlc.rs` | 99.11 %（224 行，漏 2） |
| `v21.rs` | 98.01 %（151 行，漏 3） |
| `tone.rs` | 96.83 % |
| `dsp.rs` | 98.37 % |
| `goertzel.rs`、`level.rs` | 100 % |
| `v27ter.rs` | 89.71 % |
| `v29.rs` | 93.20 % |
| **`fax-modem` 合計** | **93.83 %**（門檻 85 %，通過） |

（只開 `-p fax-modem` 時沒有 `testtx` feature，與 M5d 報告 §1 B09 欄的說明相同。）

## 6. 新增與修改的檔案

- 新增：`crates/fax-modem/tests/m5b_h.rs`、`m5b_h03_memory.rs`、`m5b_v.rs`；`crates/fax-modem/fuzz/`（`Cargo.toml`、`.gitignore`、`fuzz_targets/hdlc_deframe.rs`）；`crates/fax-in/tests/m5b_v05.rs`；`tools/fax-sim/tests/h_hdlc_spandsp.rs`、`v_v21_spandsp.rs`、`m5b_common/mod.rs`。
- 修改：`tools/spandsp-sys/src/lib.rs`（FFI）；`Cargo.toml`（`exclude` 加 `crates/fax-modem/fuzz`）；`.github/workflows/fuzz.yml`（矩陣加 `hdlc_deframe`）；`docs/TODO.md`。
