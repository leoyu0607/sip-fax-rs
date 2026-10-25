# M5d 測試報告：V.29 收端（對 SpanDSP `v29_tx`／`v17_tx`、音訊引擎＋T.30、整通 `fax_state`）

- 實作：Opus／Haiku（`crates/fax-modem/src/v29.rs`、`crates/fax-in/src/audioengine.rs`，commit 2356c91）；測試與本報告：Sonnet（依 Opus 的測試計畫）；§3 的產品修正由測試者做最小修正
- 計畫：`docs/test-plans/M5.md` §1、§6（B01–B07、B09、B10；沒有 B08）、§7（M02–M08）；ADR-061、ADR-062
- 分支：`m5`
- 範圍：B01–B07、B09、B10、M02–M08。**不在範圍**：M5e（J06、E01–E08）。
- 狀態：Opus 審核通過（2026-10-25，見 §9）

## 1. 環境

| 項目 | 內容 |
|---|---|
| 主機 | macOS（Apple Silicon，Docker Desktop）；Rust 1.99.1 |
| SpanDSP 測試（B01–B07、B10） | `tools/fax-sim/tests/b_v29_spandsp.rs`（25 個測試，其中 1 個 `#[ignore]`，單執行緒約 30 秒） |
| SpanDSP 測試（M02–M08） | `tools/fax-sim/tests/m5d_audio_engine_spandsp.rs`（15 個，其中 1 個 `#[ignore]`，約 3 秒）。M01（V.27ter）仍在 `m01_audio_engine_spandsp.rs` |
| 執行環境 | Rocky 9 映像 `sip-fax-rs-test:rocky9`（SpanDSP v3.1.1），`scripts/e2e-docker.sh`；CI 的 `e2e-spandsp` job 跑整個 `tools/` workspace，新檔案自動納入（不需改 `ci.yml`） |
| SpanDSP FFI | `tools/spandsp-sys/src/lib.rs` 補 `v29_tx_init/free/power`、`v29_tx`、`v17_tx_init/restart/free/power`、`v17_tx`（只讀 `v29tx.h`、`v17tx.h` 的簽名；沒有讀 SpanDSP 原始碼）。只在 `tools/`，主執行檔不含 SpanDSP |
| B09 | `crates/fax-modem/tests/m5d_b09.rs`（原生執行，不需 Docker）。用到 `testtx` 的部分（proptest 輸入裡的 V.27ter 訊號、`b09_proptest_after_a_training_anything_is_bounded`）以 `#[cfg(feature = "testtx")]` 包起來：`cargo test --workspace`（fax-in 的 dev-dependency 開 feature）會跑，單獨 `cargo test -p fax-modem` 不會，要跑請加 `--features testtx`。（試過在 `Cargo.toml` 加對自己的 dev-dependency，會讓 `cargo llvm-cov -p fax-modem` 把同一個檔案算兩份，涵蓋率掉到 83.6%，已拿掉。） |
| 通道模型（B 系列，`Chan`） | 與 `a_v27ter_spandsp.rs` 相同的測試內工具：電平（dBm0，對訊號有效段量測）、白色高斯雜訊（SNR 為訊號功率比 0–4 kHz 全頻帶雜訊功率，訊號前後各 200 ms 靜音含雜訊）、頻率偏移（255 階 Hilbert）、時脈偏差（16 階加窗 sinc）、G.711 μ-law／A-law 來回（SpanDSP `g711_encode/decode`）、20 ms 缺口。`b01_the_channel_helpers_do_what_they_say` 驗證工具本身（時脈偏差的長度比改用 200 000 bit 的訊號，V.29 的訊號尾巴較長，30 000 bit 的誤差超過 1e-4） |
| 通道模型（M 系列） | 兩個 hook：SpanDSP→引擎（可讀引擎的 `t30::State`）與引擎→SpanDSP。雜訊、缺口、遺失 MCF 都在 hook 內做。M06 先跑一次記下引擎送出的音訊，找出含 MCF 的 burst，第二次（同樣是確定性的）把其中一個換成靜音 |
| 送端 | SpanDSP `v29_tx`（必要，-10 dBm0）；我方 `v29::testtx::Tx` 當第二個來源。B10 用 SpanDSP `v17_tx` |
| 隨機性 | `fax_sim::SplitMix64` 固定 seed（B 系列 `0xB290_000{1,2,3}`；M02 `0x5D02_*`、M04 `0x5D04_*`；B09 `0xB090_000{1,2,3}`），每個案例 3 個 seed。proptest 以 ChaCha 固定 seed、關閉 failure persistence，每個性質 3 個 seed |
| 位元對齊 | 收端輸出從 Segment 4（48 個符號的擾碼 1）開始，所以資料起點是 192 bit（9600）／144 bit（7200）；B01 斷言這個值。比對時在 0–320 位元內找資料起點（前 400 位元錯最少者） |
| 7200 的 SNR | 計畫只給 9600（20 dB）。7200 用 17 dB（單元測試原本就用這個值） |

## 2. 案例對照表

| ID | 測試位置 | 驗證內容 | 結果 |
|---|---|---|---|
| B01 | `b_v29_spandsp.rs::b01_spandsp_clean_signal_has_no_bit_errors_over_a_million_bits` | SpanDSP 送端，-10 dBm0，9600 與 7200 各 3 seed × 340 000 bit（1 020 000 bit／速率）：**0 錯**；`Training`、`Trained` 各恰 1 次（同一刻，在訊號開始後 219.6 ms，即 Segment 3 結束；§3 ① 之前是 59.6 ms）；`CarrierLost` 恰 1 次，在訊號結束後 40 ms 內；資料起點 192／144 bit | 通過（Docker） |
| B01 | `…::b01_our_own_transmitter_is_a_second_source`、`b01_the_channel_helpers_do_what_they_say` | 我方 `testtx::Tx` 當來源（2 速率 × 3 seed × 100 000 bit，0 錯、事件同上）；通道工具的電平、雜訊電平、缺口、ppm 長度比 | 通過（Docker） |
| B02 | `…::b02_levels_from_minus_10_to_minus_43_dbm0` | 電平 -10／-20／-30／-40／-43 dBm0 × 2 速率 × 3 seed × 50 000 bit：0 錯、事件同 B01、Training < 300 ms | 通過（Docker） |
| B02 | `…::b02_our_own_transmitter_at_the_same_levels`、`b02_below_43_dbm0_is_recorded_not_required` | 第二來源 -10／-30／-43 dBm0 0 錯。記錄：-45 dBm0 兩速率 0 錯；**-47 dBm0（在開啟與關閉門檻之間）會訓練成功但資料是垃圾**（7200 錯 11 969／20 000，9600 錯 17 596／20 000），各只有 1 次 Training／Trained／CarrierLost，沒有要求 | 通過（只記錄） |
| B03 | `…::b03_awgn_ber_at_the_plan_snr_and_two_db_above` | 9600、SNR 20 dB，3 seed × 340 000 bit：**0 錯／1 020 000 bit**；SNR 22 dB：0 錯／1 020 000。7200、SNR 17 dB，3 seed × 100 000 bit：0 錯；SNR 19 dB：0 錯。上限 10⁻⁵（計畫值，字面達成） | 通過（Docker） |
| B03 | `…::b03_ber_curve_is_recorded` | BER 曲線（3 seed × 100 000 bit）。9600：SNR 16 dB 2.8×10⁻³、17 dB 7.5×10⁻⁴、18 dB 1.5×10⁻⁴、19 dB 3.3×10⁻⁵、20 dB 0、22 dB 0；7200：13 dB 1.4×10⁻³、14 dB 2.5×10⁻⁴、15 dB 4.0×10⁻⁵、16 dB 0、17 dB 0、19 dB 0 | 通過（只記錄） |
| B03 | `…::b03_a_long_7200_page_at_22_db_is_error_free` | 7200 一整頁長度（340 000 bit，47 秒）、SNR 22 dB、3 seed：0 錯 | 通過（Docker） |
| B03 | `…::b03_a_long_7200_page_at_low_snr_is_recorded`、`b03_a_long_7200_page_at_the_plan_snr_unmet`（`#[ignore]`） | **7200 在 SNR 17 dB 的 340 000 bit：3 個 seed 都在 30–37 秒後失去資料，之後約一半的位元錯，不再恢復**（錯 36 914／53 252／60 867，第一個錯在 36.8／32.1／30.1 s）；SNR 19 dB：1 個 seed 在 45.7 s 失去（錯 5 146），其餘 0 錯。`unmet` 那個測試斷言 SNR 17 dB 整頁 0 錯，目前失敗，所以 `#[ignore]`。見 §4 ① | **未達成**（記錄，`#[ignore]`） |
| B04 | `…::b04_clock_deviation_and_frequency_offset` | ±100 ppm、±7 Hz 各別與同號組合，2 速率 × 3 seed × 100 000 bit，SNR 為計畫值（9600：20 dB；7200：17 dB）；SNR＋2 dB 也測：**12 個條件全部 0 錯／300 000 bit**（上限 10⁻⁵，字面達成） | 通過（Docker） |
| B04 | `…::b04_without_noise_every_deviation_is_error_free`、`b04_our_own_transmitter_with_its_own_deviation_inputs` | 無雜訊、同組偏差、2 速率 × 3 seed × 50 000 bit：0 錯、事件同 B01；第二來源（+100 ppm＋7 Hz、−100 ppm−7 Hz）0 錯 | 通過（Docker） |
| B05 | `…::b05_g711_round_trip_of_both_laws` | μ-law 與 A-law 來回＋AWGN：9600（20 dB）、7200（17 dB）各 4 個條件 **0 錯／300 000 bit**；SNR＋2 dB 也 0 錯（上限 10⁻⁵） | 通過（Docker） |
| B05 | `…::b05_g711_alone_at_low_and_normal_levels_is_error_free` | 只有 G.711（無雜訊）-10／-30／-40 dBm0，兩 law × 2 速率：0 錯、事件同 B01 | 通過（Docker） |
| B06 | `…::b06_a_gap_in_the_data_does_not_hang_the_receiver` | 30 000 bit 資料的 30%／50%／70% 處丟掉 20 ms（160 個零樣本），2 速率 × 3 seed，-15 dBm0。**實際行為：收端回報 `CarrierLost`（DataEnd），在缺口開始後 110–240 個樣本（14–30 ms，量測粒度是 160 樣本的區塊）；缺口之前送出的位元全對**；缺口之後的資料不再解碼（已回到找 Segment 2）。同一個收端接著收 0.5 s 靜音與一次乾淨的完整傳送：`Training`、`Trained` 各 1 次、10 000 bit 0 錯（沒有卡住） | 通過（Docker） |
| B06 | `…::b06_after_a_gap_in_the_tcf_t30_answers_and_the_next_dcs_is_heard`、`b06_a_gap_after_a_long_enough_tcf_still_gives_cfr` | 音訊引擎＋T.30：TCF 資料中途（0.2／0.6／1.0 s）缺口 → 收到的 TCF 不到 1.2 s → **FTT**（不是掛住、也不是 CFR）；對方照常再送 DCS＋乾淨 TCF → **CFR**。缺口在 1.4 s 處（已收超過 1.2 s 的全 0）→ CFR | 通過（Docker） |
| B07 | `…::b07_all_zero_tcf_is_confirmed_with_cfr` | SpanDSP 與我方送端 × 2 速率 × 3 條通道（乾淨 -15 dBm0／-30 dBm0＋AWGN／μ-law）：全 0 的 1.5 s TCF → 引擎經 `AudioEngine`＋T.30 回 **CFR**，沒有 FTT | 通過（Docker） |
| B07 | `…::b07_a_tcf_with_errors_is_refused_with_ftt`、`b07_the_threshold_is_five_percent_ones` | 每個 bit 以 8%／20%／50% 機率為 1 的 TCF → **FTT**；門檻對照：1% 為 1 → CFR、8% → FTT，與 T.38 路徑同一條規則（`tcf_zero_percent` 95）。**9600 的餘裕只有約 1.9%**，見 §5 ② | 通過（Docker） |
| B09 | `m5d_b09.rs::b09_white_noise_and_silence_never_make_a_training_short_run` | 白雜訊 -60／-55／-50／-48／-46／-43／-40／-30／-25／-20 dBm0 與靜音，3 seed × 每個 2 s（每速率 66 s 的音訊）：沒有任何事件 | 通過 |
| B09 | `…::b09_proptest_no_panic_no_event_for_anything_that_is_not_a_v29_signal` | proptest（3 seed × 300 組 × 2 速率）：任意 i16、常數長串、滿刻度極值、-70 到 -10 dBm0 白雜訊、靜音、**50–3900 Hz 的任意單音**、任意位元與電平的 V.21、**V.27ter（兩速率、長短 training、-45 到 -5 dBm0、任意資料）**，任意分塊餵入：不 panic，**一個事件都沒有** | 通過 |
| B09 | `…::b09_proptest_after_a_training_anything_is_bounded` | 先餵真實 V.29 傳送到 `Trained`，再餵任意樣本（3 seed × 200 組 × 2 速率）：不 panic，輸出位元數 ≤ 符號率允許的上限 | 通過 |
| B09 | `…::b09_no_pure_tone_is_taken_for_a_training` | 100–3900 Hz（25 Hz 一格）× -40／-20／-6 dBm0 × 2 速率的單音：沒有事件、沒有位元。**修正前**：100–350、650–950、2450–2750、3050–3350 Hz 的單音（7200）與 100–275、700–900、2500–2700、3125–3325 Hz（9600）在 2 秒的單音上都給 `Training`、`Trained` 與約 13 000（7200）／17 500（9600）個垃圾位元（§3 ①） | 通過 |
| B09 | `…::b09_one_hour_of_white_noise_per_level`（`#[ignore]`） | 每速率 10 個電平＋靜音各 1 小時（共 22 小時音訊，release 約 51 秒）：**假 Training 0 次**（所有電平與兩速率；Trained 0、CarrierLost 0、bits 0）。修正前同一輸入：9600 在 -43 dBm0 有 1 次 Training＋Trained，之後 27 038 708 個垃圾位元（卡在資料狀態），§3 ① | 通過（release） |
| B10 | `b_v29_spandsp.rs::b10_a_v17_signal_is_not_taken_for_a_v29_training` | SpanDSP `v17_tx` 7200／9600／12000／14400 × 長／短 training × -10／-25／-40 dBm0 × 我方 7200 與 9600 收端（共 48 個）：**沒有 Training、Trained、位元** | 通過（Docker） |
| B10 | `…::b10_a_v29_receiver_after_a_v17_signal_still_receives_v29` | 同一個收端先聽 V.17（9600，30 000 bit），再收 V.29：V.17 部分沒有事件，V.29 部分 `Training` 1 次、20 000 bit 0 錯 | 通過（Docker） |
| B10 | `…::b10_t30_does_not_confirm_a_v17_training_it_cannot_receive` | 對方送 DCS 指定 V.17 9600／14400（我方 DIS 沒有）＋V.17 TCF：引擎記「DCS selects V17 … not received」，T.30 回 **FTT**，對方不再試，T2 後 **DCN**，沒有 CFR。**計畫寫的「以 DIS 重新協商」沒有發生**，見 §4 ② | 通過（偏離見 §4） |
| M02 | `m5d_audio_engine_spandsp.rs::m02_v29_forced_three_fine_pages_mh`、`…_mr` | SpanDSP 只開 V.29，3 頁 fine（300／400／500 列），MH 與 MR：SpanDSP 結果碼 0；`T30Result`：`code` OK、`pages` 3、**`modem` = V29、`bit_rate` = 9600**、`compression` = Mh／Mr、`resolution` = Fine；每頁 `ReceivedPage.modem` = V29、`source_coding` = Mh／Mr、`bad_lines` 0、G4 條帶解碼後與 `render_bitmap` **逐 octet 相同**、y dpi 196。模擬時間 MH 71.0 s、MR 43.5 s | 通過（Docker） |
| M02 | `…::m02_standard_resolution_page_over_v29`、`m02_v29_with_line_noise_in_three_seeds` | 1 頁 standard 600 列，逐像素相同；2 頁 fine，整通加白雜訊 -54／-51／-49 dBm0（SNR 41／38／36 dB，3 seed；原因見 M5c §5 ①）：逐像素相同 | 通過（Docker） |
| M03 | `…::m03_spandsp_picks_v29_9600_from_a_dis_without_v17` | SpanDSP 三種數據機都開：選 **V.29 9600**，沒有選 V.17（我方 DIS 沒有）；V.27ter＋V.29 兩種、第 1 頁 standard 第 2 頁 fine：V.29 9600；逐像素相同 | 通過（Docker） |
| M03 | `…::m03_a_caller_with_v17_only_falls_back_to_v27ter_4800` | SpanDSP 只開 V.17（沒有共同數據機）：SpanDSP 不放棄，退到 V.27ter 4800，我方收下（逐像素相同，`modem` = V27ter、4800）。計畫沒有這一項 | 通過（Docker） |
| M04 | `…::m04_a_bad_tcf_at_9600_is_refused_and_the_call_falls_back` | 第一次 TCF 中間加 600 ms 的雜訊（-18 dBm0，約 SNR 5 dB），3 seed：第一次 TCF 零位元比例 67.7%／67.3%／71.3%（需 95%）→ **FTT**；SpanDSP 降到 **7200**，第二次 TCF（1389 octets，10 818／11 112 為 0）→ CFR；2 頁逐像素相同；`T30Result` 的 `bit_rate` 7200。**這是雜訊 burst，不是計畫的固定 SNR**，見 §4 ③ | 通過（偏離見 §4） |
| M04 | `…::m04_constant_noise_that_fails_9600_is_recorded` | 計畫的字面做法（整通固定 SNR 12／16 dB）：引擎 29.2 秒後結束，T.30 結果 `RemoteDisconnect`、0 頁，SpanDSP 結果碼 20。因為雜訊高於 -48 dBm0，引擎看不到 TCF 結束（M5c §5 ①）。只斷言引擎有結束 | **未達成**（記錄） |
| M05 | `…::m05_a_lost_packet_in_the_middle_of_a_page_what_happens`、`m05_a_lost_packet_in_the_middle_of_a_page_plan`（`#[ignore]`） | 3 頁 fine（各 500 列）的第 2 頁，進入 RxImage 後 600 ms 丟掉 20 ms：**呼叫失敗**，T.30 結果 `T2ExpiredCommand`，只交出第 1 頁（逐像素相同，第 2、3 頁沒有），SpanDSP 結果碼 21，模擬 48.3 s。`what_happens` 斷言這個行為（以及「損壞的呼叫不會被報成乾淨的成功」）；`plan` 斷言計畫的結果（3 頁、第 2 頁 bad_lines ≥ 1、第 1、3 頁逐像素相同），目前失敗，`#[ignore]`。見 §4 ④ | **未達成**（記錄，`#[ignore]`） |
| M06 | `…::m06_a_lost_mcf_is_repeated_not_taken_for_a_new_page` | 3 頁 fine（MPS、MPS、EOP）。先正常跑一次（37.8 s，3 個 MCF burst）；再把第 1 個 MCF（MPS 之後）、第 3 個 MCF（EOP 之後）各換成靜音：對方等 T4 後重送命令，引擎記「MPS repeated: response resent」／「EOP repeated: response resent」，重回 MCF，**3 頁、不重複、逐像素相同**，SpanDSP 結果碼 0，模擬時間各多 4.6 s（42.4 s） | 通過（Docker） |
| M07 | `…::m07_a_caller_with_ecm_gets_a_non_ecm_call_because_our_dis_has_no_ecm` | SpanDSP 開 ECM、我方 `ecm` 關（預設）：DIS 沒有 ECM，SpanDSP 退到非 ECM，2 頁逐像素相同，`T30Result.ecm` = false | 通過（Docker） |
| M07 | `…::m07_ecm_on_the_audio_path_is_not_supported_recorded` | **音訊路徑不支援 ECM，計畫的「開啟時 V.29 ECM 收」無法測**：引擎的 `t30.ecm = true`、SpanDSP 開 ECM 時，TCF 正常（CFR），之後高速收端把 HDLC 框當影像位元交給 T.30，影像永遠解不出來：`T2ExpiredImage`、0 頁，18.1 s 結束，SpanDSP 結果碼 21。測試斷言引擎有結束、沒有交出錯的頁 | **不可測**（記錄） |
| M08 | `…::m08_the_caller_sends_cng_all_through_our_ced_and_dis_and_does_not_hear_them` | SpanDSP 前 7 秒聽不到引擎（一直送 CNG，蓋過我方的 CED、CSI、DIS 與 T4 重送的 DIS），之後聽得到：引擎記「CNG heard」，呼叫完成，1 頁逐像素相同 | 通過（Docker） |
| M08 | `…::m08_a_dcs_in_the_middle_of_our_ced_is_lost_but_the_call_recovers_on_the_repeat` | 對方不等 CED，在接通後 500 ms（CED 中）就送 TSI＋DCS：**沒有被聽到也沒有被回應**（引擎送任何訊號時都不聽，CED 也算，半雙工設計）；DIS 之後對方重送 TSI＋DCS＋SpanDSP `v29_tx` 的 TCF → **CFR**（1862 octets，14 439／14 896 為 0）。計畫的「照樣收下」只對會重送的對方成立，見 §4 ⑤ | 通過（偏離見 §4） |

## 3. 發現並修正的產品問題

做了最小修正，共約 20 行，有單元測試（還原就失敗）。依 CLAUDE.md，這個修正的決策需要記在 ADR-062（我沒有改 `docs/DECISIONS.md`，請主 session 補）。

1. **V.29 收端的 Segment 3 沒有驗證，雜訊與單音會騙它進入資料狀態，之後在雜訊中永遠出不來**（B09 的 1 小時白雜訊發現，`fax-modem/src/v29.rs`）。原因：`Acquire` 以連續的 ±135° 轉角鎖定載波，下一個比較靠近 C 而不是 A 的點就宣告 Segment 3 開始並送出 `Training`；之後 `Train` 階段 384 個符號一律以已知的 C／D 點訓練，結束時無條件宣告 `Trained` 進入資料狀態。真實的訓練與雜訊在這裡沒有任何區別。進入資料狀態後，只要輸入高於關閉門檻（-48 dBm0）就不會有 `CarrierLost`，永遠不回到找 Segment 2，等於 M5c §3 ② 同型的問題。證據（修正前）：9600 在 -43 dBm0 白雜訊 1 小時有 1 次 `Training`＋`Trained`，其後 27 038 708 個垃圾位元（7200 與其他電平 0 次）；B09 的單音掃描：7200 在 100–350／650–950／2450–2750／3050–3325 Hz、9600 在 100–275／700–900／2500–2700／3125–3325 Hz 的純音在 2 秒的單音上都得到 `Training`、`Trained` 與約 13 000（7200）／17 500（9600）個垃圾位元（800／2600 Hz 的單音每符號轉 135°，與 Segment 2 的 A→B 相同）。修正：Segment 3 最後 128 個符號累計與已知 C／D 點的平方誤差，結束時若誤差大於已知點功率的 0.25（MER 低於 6 dB；真實訊號在該速率可用的最低 SNR 也在 15 dB 以上，雜訊硬套訓練序列約 0 dB）就放棄（`reset()`，回到找 Segment 2），不送 `Trained`；**`Training` 事件改到 Segment 3 結束、與 `Trained` 同一刻才送**（否則誤判仍會送出一次 `Training`；修正的第一個版本只加了誤差檢查，1 小時白雜訊在 9600／-43 dBm0 仍有 1 次 `Training`）。代價：`Training` 比以前晚 160 ms（59.6 ms → 219.6 ms，Segment 3 的長度）；t30 的接收端幾乎不用 `RxSignal::Training`（只有 ECM 區塊等待時設一個旗標），audioengine 只是轉送。證據（修正後）：1 小時白雜訊 22 小時音訊假 `Training` 0 次；單音掃描與 proptest（含 V.27ter 訊號）0 個事件；所有 B 案例通過；單元測試 `a_tone_that_looks_like_segment_2_is_given_up_and_the_real_signal_is_received`（800／2600 Hz 的單音：無事件、無位元，接著真實傳送的 4000 bit 完全正確；把門檻改成 10⁹ 還原後失敗：「Training, Trained, 27080 bits」）。

沒有修的缺陷見 §4 ①。

## 4. 與計畫不同之處、未達成的項目

① **7200 在較低 SNR 的長傳送會失去資料（B03，未修，需要設計決定）**。7200、SNR 17 dB（本報告選的 7200 值）時，一個 340 000 bit（47 秒）的傳送在 30–37 秒後突然失去資料（3 個 seed 都是），之後約一半的位元錯，不再恢復；SNR 19 dB 有 1 個 seed 在 45.7 秒失去；SNR 22 dB 3 個 seed 都沒事。9600 在 20 dB 的 1 020 000 bit 0 錯，在 22 dB 也是。100 000 bit（14 秒）以下 7200 在 17 dB 完全沒事（B03、B04、B05 的 7200 條件全部 0 錯）。**原因（部分查明）**：用我方 `testtx` 當來源加同樣的雜訊，在原生環境重現（17 dB：3 個 seed 的第一個錯在 271 552／277 244／336 544 bit，15 dB 更早：第一個錯在 31 332–153 864 bit）。在 `v29.rs` 的測試裡逐秒量測（17 dB）：平方誤差平均在 0.07（雜訊底）穩定到第 33 秒，等化器係數的總能量從 1.83 慢慢爬到 2.12，第 36 秒誤差開始變大（0.10），第 39 秒是 0.62、係數能量掉到 0.84、載波相位亂掉，不再恢復。判斷是決策導向 LMS 沒有洩漏項，係數隨雜訊漂移，最後一次罕見的判錯就把迴路推垮；（7200 的星座對角線點是 √2、軸上是 3，最近兩點的距離 √5 比同功率的 8PSK 小，可能是 7200 比 9600 更早出事的原因，沒有驗證。）**嘗試**：每符號 `w *= 1 - 10⁻⁴` 的洩漏：更糟（AGC 在資料段凍結，洩漏讓整體增益縮水，MER 從第一秒就是 0.58），已還原。更好的做法（洩漏加上增益補償、資料段步階、偏離時回到 training 的檢查）都是設計決定，沒有做。**影響**：要在 SNR 17–19 dB 的線路收 40 秒以上的 7200 頁面才會碰到（一頁 fine A4 文字約 20–60 KB MH，7200 約 22–67 秒，並不罕見）。`b03_a_long_7200_page_at_the_plan_snr_unmet`（`#[ignore]`）寫出計畫的要求，修好後拿掉 `#[ignore]`；`b03_a_long_7200_page_at_22_db_is_error_free` 現在擋 22 dB。

② **B10：DCS 指定我方沒有的數據機時沒有以 DIS 重新協商**。計畫寫「t30 依 DCS 回應（DCS 指定我方沒有的數據機時以 DIS 重新協商）」。實際：`audioengine` 認得 V.17 的 DCS 但沒有 V.17 收端（記「not received」），`t30` 的接收端不檢查 DCS 的數據機是否在我方 DIS 之內，等 TCF，T2 逾時才回 **FTT**，之後對方不再試，再 T2 後 **DCN**（`t2_expired_command`，22 s）。沒有 CFR（沒有錯認），也沒有新的 DIS。真實的 V.17 機器收到 FTT 會在 V.17 內降速，不會退到 V.29，所以這個呼叫還是會失敗，只是慢。「不誤判為 V.29 training」這部分完全達成（48 個條件 0 事件）。建議 M5e 或 t30 在 DCS 選到未宣告的數據機時立刻回 FTT 或重送 DIS，記成待辦。

③ **M04 用雜訊 burst 而不是固定 SNR**。計畫「通道 SNR 讓 9600 的 TCF 失敗」。9600 的 TCF 要失敗需要 SNR 約 16 dB 以下，雜訊底約 -28 dBm0 以上，遠高於引擎的絕對結束門檻 -48 dBm0（M5c §5 ①），引擎永遠看不到 TCF 結束：整通 29.2 秒後 `RemoteDisconnect`、0 頁（12 dB 與 16 dB 都是；見 `m04_constant_noise_that_fails_9600_is_recorded`）。為了仍然驗證「FTT → 降速 → 成功」，我只在第一次 TCF 中間加 600 ms 的雜訊 burst，其餘時間乾淨。M04 的字面做法算**未達成**，與 M5c 的待辦（結束偵測改看相對下降）是同一件事。

④ **M05 未達成：封包遺失讓整通失敗，而不是壞行**（M5c §5 ④ 已預告）。第 2 頁中間遺失 20 ms：收端在缺口後 14–30 ms 回報 `CarrierLost`（B06 的量測），T.30 把頁面在那裡結束；對方照常把剩下的資料送完，我方在 `Acquire` 看不到 Segment 2，引擎的回應被「對方訊號還在」擋住，500 ms 後「answering anyway」，之後對方的 MPS 沒有被正確收到，`T2ExpiredCommand`，只有第 1 頁，SpanDSP 結果碼 21。沒有深入查 T.30 的 RxImage 之後的路徑，這屬於 M5e 的設計待辦。計畫的期望（第 2 頁 bad_lines ≥ 1、MCF 或 RTN、其餘頁逐像素相同）寫在 `#[ignore]` 的 `m05_a_lost_packet_in_the_middle_of_a_page_plan`。

⑤ **M08 的「不等 CED 就送 DCS」不會被聽到**。引擎送任何東西的時候都不收（`deaf_until`，含 CED、CSI＋DIS 與其後 75 ms 的回音保護），所以 CED 中的 DCS 完全被忽略。符合 T.30 的對方（收到 DIS 才送 DCS）不受影響，SpanDSP 的整通（M02、M03 …）和「CNG 蓋過 CED」的案例都通過；只有「從不重送」的非標準對方會失敗。這是半雙工設計的後果，不修，列出供審核。

⑥ **M07 不可測**：見表。音訊路徑沒有 ECM；M5e 必須讓音訊 session 的 `t30.ecm` 保持 false（產品設定 `[trunks.fax] ecm` 預設為開，`config.rs` 註記），否則 ECM 的對方（幾乎所有機器）會得到「TCF 成功、影像失敗」。

⑦ **其他小偏離**：B07 的門檻對照改為 1%（M5c 是 2%），原因見 §5 ②；B01／B02 的 Training 時間上限從 100 ms 改為 300 ms（§3 ①）；B03 的 7200 SNR 17 dB 是我選的；M03 多測一個「只開 V.17」的案例；M02 多測 standard 解析度與有雜訊的呼叫；B09 的單音在修正前的掃描範圍見 §2。

## 5. 觀察到、沒有修的問題（供審核者決定）

1. 見 §4 ①（7200 長傳送失去資料）、②（V.17 的 DCS）、③（固定 SNR 與結束偵測）、④（封包遺失）、⑤（CED 中的 DCS）、⑥（ECM）。
2. **9600 的 TCF 判定餘裕只有約 1.9%**：收端的位元流從 Segment 4 開始，SpanDSP 在資料後還會送一小段（約 288 bit 的 1），一個全 0 的 9600 TCF 被量到 14 896 bit 中有 457 個 1（3.07%），T.30 的門檻是 5%，所以 TCF 本身只能再有 1.9% 的 1。同樣的 7200 全 0 TCF：11 112 bit 中 294 個 1（2.6%）；V.27ter 4800 只有 8 個 Segment 5 的 1，餘裕 4.9%。真實線路有雜訊的 9600 TCF 比 V.27ter 更容易被判 FTT，可能造成不必要的降速。改法：收端不要輸出 Segment 4 的位元（192／144 bit），或讓 T.30 的 TCF 計算排除起頭的 1。這是設計選擇，沒有改。（`b07_the_threshold_is_five_percent_ones` 的測試用 1%。）
3. B02：-47 dBm0（在開啟門檻 -43 與關閉門檻 -48 之間）會「訓練成功但資料垃圾」，1 次 Training／Trained 之後約 60–88% 的位元錯；計畫只要求到 -43。
4. 修正（§3 ①）的 `Training` 延後 160 ms 在 M5e 的 `audio.log` 上會顯示得比實際的載波晚；`audio.log` 若要記載波出現的時間，應該用高速收端的 presence，不是 `Training`。

## 6. 涵蓋率

| 項目 | 數字 | 門檻 |
|---|---|---|
| `fax-modem`（行）`cargo llvm-cov -p fax-modem --fail-under-lines 85 --summary-only` | **94.06%**（`v29.rs` 94.34%，在 `scripts/ci-local.sh` 內，workspace 統一開 `testtx`；單獨 `cargo llvm-cov -p fax-modem`、不開 `testtx` 時 88.58%，`v29.rs` 82.83%，因為用到 testtx 的測試被略過，仍 ≥ 85%） | ≥ 85% |
| `fax-in`（行）`cargo llvm-cov -p fax-in --fail-under-lines 70 --summary-only` | **92.23%**（`audioengine.rs` 92.00%） | ≥ 70% |

SpanDSP 的測試（`tools/`）不計入涵蓋率（獨立 workspace）；涵蓋率只含 `fax-modem` 的單元測試、`m5a_n`、`m5c_a09`、`m5d_b09`，與 `fax-in` 的單元與整合測試。

## 7. 執行指令

```sh
# B01–B07、B10 與 M02–M08（SpanDSP，Docker；--test-threads=1 讓輸出不交錯）
E2E_ONLY="--test b_v29_spandsp --test m5d_audio_engine_spandsp" E2E_SKIP_CHECKS=1 scripts/e2e-docker.sh --test-threads=1
# 含 fmt、clippy、t4 oracle 的完整 tools/ workspace
scripts/e2e-docker.sh
# 被 #[ignore] 的未達成項目（目前會失敗）
E2E_ONLY="--test b_v29_spandsp --test m5d_audio_engine_spandsp" E2E_SKIP_CHECKS=1 scripts/e2e-docker.sh -- --ignored
# B09 正常版（約 14 秒）
cargo test -p fax-modem --test m5d_b09
# B09 一小時白雜訊（release，約 51 秒；A09_SECONDS=600 縮短）
cargo test -p fax-modem --release --test m5d_b09 -- --ignored --nocapture
cargo llvm-cov -p fax-modem --fail-under-lines 85 --summary-only
cargo llvm-cov -p fax-in --fail-under-lines 70 --summary-only
```

## 8. 本機驗證結果

| 指令 | 結果 |
|---|---|
| `E2E_ONLY="--test b_v29_spandsp --test m5d_audio_engine_spandsp" E2E_SKIP_CHECKS=1 scripts/e2e-docker.sh --test-threads=1` | 通過：`b_v29_spandsp` 24 通過＋1 ignored（29.9 秒）、`m5d_audio_engine_spandsp` 14 通過＋1 ignored（2.7 秒） |
| `cargo test -p fax-modem`；加 `--features testtx` | 全部通過（含新增的 v29 單元測試；`m5d_b09` 3 個＋1 ignored，加 feature 後 4 個＋1 ignored） |
| `cargo test -p fax-modem --release --test m5d_b09 -- --ignored` | 通過，假 Training 0 次（22 小時音訊） |
| `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all`（根 workspace 與 `tools/`） | 無問題 |
| `cargo test --workspace --all-targets` | 通過（需 `T4_ORACLE=skip`，這台 macOS 沒有 poppler） |
| 修正前的失敗確認 | §3 ①：B09 一小時白雜訊 9600／-43 dBm0 的假 Training（27 038 708 個垃圾位元）；單元測試把 `FIT_LIMIT` 改成 10⁹ 還原後失敗 |
| `scripts/ci-local.sh` | **exit 0**（"ci-local: all checks passed"；包含 fmt、clippy、全 workspace 測試、各 crate 涵蓋率門檻、musl 建置、完整 `scripts/e2e-docker.sh`、web openssl、Playwright）。中途兩次失敗都已修：第一次是 fax-modem 涵蓋率 83.62%，原因見 §1 的 B09 一列；第二次是 tools 的 clippy `unusual_byte_groupings`（`0xB10_2`） |

## 9. Opus 審核

2026-10-25，**通過**。B03 的 7200 長頁、M04 固定 SNR、M05 記為未達成，另立後續項目（不放寬計畫）；M07 不可測。

- 斷言與 mock：抽查 `b_v29_spandsp.rs`、`m5d_audio_engine_spandsp.rs`、`m5d_b09.rs`。送端是 SpanDSP（`v29_tx`、`v17_tx`、`fax_state`），我方送端只當第二來源；BER 對整段位元逐一比較，M 系列逐像素比對並斷言 `T30Result` 的數據機與速率。只記錄的測試（M04 固定 SNR、M05 `what_happens`、M07）都在報告中標明，計畫的要求另寫成 `#[ignore]` 測試，修好後拿掉 `#[ignore]` 即可驗收。沒有空斷言，也沒有以我方元件模擬對方。
- §3 ① 的 Segment 3 擬合檢查：審過 diff（約 20 行），有「還原就失敗」的證據，接受，記於 ADR-063。`Training` 延後 160 ms 不影響 t30（只用 `Trained` 與位元）。
- §4 ①（B03 7200 長頁）：屬實的產品缺陷，不是測試問題。另立後續項目：資料段等化器的長時間穩定性（洩漏要配合增益補償，或偏離時回 training），用逐秒 MER 驗證，再拿掉 `b03_a_long_7200_page_at_the_plan_snr_unmet` 的 `#[ignore]`。
- §4 ②（V.17 的 DCS）、③（固定 SNR）、④（M05）、⑥（音訊路徑沒有 ECM，M5e 必須讓音訊 session 的 `t30.ecm` 為 false）列入 M5e 設計待辦。⑤（CED 中的 DCS）是半雙工的後果，接受。
- §5 ②（9600 TCF 只剩約 1.9% 餘裕）：列入 M5e 待辦，傾向收端不輸出 Segment 4 的位元。
- 涵蓋率：fax-modem 94.06%、fax-in 92.23%，符合門檻。ci-local exit 0。
- **後續（2026-10-25，Opus）**：§4 ① 已修（ADR-065）。根因是重心時脈與等化器互相牽動，造成時脈漂移；資料段改用 Gardner 二階迴路後，`b03_a_long_7200_page_at_the_plan_snr` 在 SNR 17／19 dB × 3 seed 都是 0 錯，`#[ignore]` 已拿掉，B03 改記為達成。
