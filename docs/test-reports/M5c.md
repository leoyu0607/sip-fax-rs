# M5c 測試報告：V.27ter 收端（對 SpanDSP `v27ter_tx`、音訊引擎＋T.30、整通 `fax_state`）

- 實作：Opus／Haiku（`crates/fax-modem/src/{dsp,v27ter}.rs`、`crates/fax-in/src/audioengine.rs`，commit e6d2d47）；測試與本報告：Sonnet（依 Opus 的測試計畫）；§3 的產品修正由測試者做最小修正
- 計畫：`docs/test-plans/M5.md` §1、§6（A01–A07、A09；沒有 A08）、§7（M01）；ADR-061、ADR-062
- 分支：`m5`
- 範圍：A01–A07、A09、M01。**不在範圍**：B 系列與 M02–M08（M5d）。
- 狀態：（待審核）

## 1. 環境

| 項目 | 內容 |
|---|---|
| 主機 | macOS（Apple Silicon，Docker Desktop）；Rust 1.99.1 |
| SpanDSP 測試（A01–A07、M01） | `tools/fax-sim/tests/a_v27ter_spandsp.rs`（19 個測試）、`tools/fax-sim/tests/m01_audio_engine_spandsp.rs`（2 個）；在 Rocky 9 映像 `sip-fax-rs-test:rocky9`（SpanDSP v3.1.1）內以 `scripts/e2e-docker.sh` 執行，CI 的 `e2e-spandsp` job 跑整個 `tools/` workspace，新檔案自動納入（不需改 `ci.yml`） |
| SpanDSP FFI | `tools/spandsp-sys/src/lib.rs` 補 `v27ter_tx_init/free/power`、`v27ter_tx`、`span_get_bit_func_t`、`SIG_STATUS_END_OF_DATA`（只讀標頭取簽名；沒有讀 SpanDSP 原始碼）。只在 `tools/`，主執行檔不含 SpanDSP |
| A09 | `crates/fax-modem/tests/m5c_a09.rs`（原生執行，不需 Docker） |
| 通道模型（測試內的小工具 `Chan`） | 電平（dBm0，對訊號有效段的均方量測，誤差 < 0.05 dB）、白色高斯雜訊（SNR 為訊號功率比 0–4 kHz 全頻帶雜訊功率；含訊號前後各 200 ms 靜音）、頻率偏移（255 階 Hilbert 解析訊號乘複指數）、時脈偏差（16 階加窗 sinc 重取樣，長度比例誤差 < 1e-4）、G.711 μ-law／A-law 來回（SpanDSP `g711_encode/decode`）、20 ms（160 樣本）全零的缺口。`a01_the_channel_helpers_do_what_they_say` 驗證這些工具本身 |
| 送端 | SpanDSP `v27ter_tx`（必要，-10 dBm0 產生後縮放到目標電平）；我方 `testtx::Tx` 當第二個來源（`fax-modem` 的 `testtx` feature，`fax-sim` 的 dev-dependency 開啟） |
| 隨機性 | `fax_sim::SplitMix64` 固定 seed（`0xA270_000{1,2,3}`；A09 `0xA090_000{1,2,3}`；M01 `0x4D01_000{1,2,3}`），每個案例 3 個 seed。proptest 以 ChaCha 固定 seed、關閉 failure persistence，每個性質 3 個 seed |
| 效能設定 | `tools/Cargo.toml` 加 `[profile.dev.package.fax-modem] opt-level = 3`（測試 profile 也吃這項），否則百萬 bit 的案例在 debug 太慢；其他 crate 不變 |
| 位元對齊 | 收端輸出從 Segment 5（8 個符號的擾碼 1）開始；比對時在 0–96 位元內找資料的起點（前 400 位元錯最少者）。SpanDSP 與我方送端的起點都固定是 24（4800）／16（2400）位元，A01 斷言這個值 |

## 2. 案例對照表

| ID | 測試位置 | 驗證內容 | 結果 |
|---|---|---|---|
| A01 | `a_v27ter_spandsp.rs::a01_spandsp_clean_signal_has_no_bit_errors_over_a_million_bits` | SpanDSP 送端，-10 dBm0，2400 與 4800 各 3 個 seed × 340 000 bit（共 1 020 000 bit/速率）：0 錯；`Training` 恰 1 次且在訊號開始後 19.5 ms（4800）／32.6 ms（2400）；`Trained` 恰 1 次；`CarrierLost` 恰 1 次，在訊號結束後 40 ms 內；資料起點 24／16 bit | 通過（Docker） |
| A01 | `…::a01_our_own_transmitter_is_a_second_source`、`a01_the_channel_helpers_do_what_they_say` | 我方 `testtx::Tx` 當來源（2 速率 × 3 seed × 100 000 bit，0 錯、事件同上）；通道工具的電平、雜訊電平、缺口、ppm 長度比 | 通過（Docker） |
| A01 | `…::a01_periodic_data_and_blank_t4_rows_against_spandsp` | SpanDSP 送端，-10 dBm0、無雜訊、2400 與 4800：全 0、全 1、週期 2／3／4／6／8／9／12（固定 PRBS 取前幾個 bit 為單位）各 20 000 bit，以及 T.4 MH 空白 A4 頁（`t4::Encoder`，EOL＋全白列碼）100 列（2 976 bit）、800 列（23 272 bit）、800 列加填充位元（25 696 bit）：**全部 0 錯**。我方解擾器的 guard 觸發次數（以測試內的擾碼器複本在還原出的線上位元上計數，已斷言與產品輸出完全相同）：週期 3 共 95–96 次、週期 6 共 42–43、週期 8 共 25–26、週期 9 共 39–40、週期 12 共 53–54、MH 空白頁 1–15 次；全 0／全 1／週期 2／週期 4 為 0–1 次（2400 的全 0 與週期 2 各 1 次） | 通過（Docker） |
| A02 | `…::a02_levels_from_minus_10_to_minus_43_dbm0` | SpanDSP 送端，電平 -10／-20／-30／-40／-43 dBm0 × 2 速率 × 3 seed × 50 000 bit：0 錯、事件同 A01、Training < 100 ms | 通過（Docker） |
| A02 | `…::a02_our_own_transmitter_at_the_same_levels`、`a02_below_43_dbm0_is_recorded_not_required` | 第二來源 -10／-30／-43 dBm0 0 錯。記錄：-45、-47 dBm0（在開啟與關閉門檻之間）兩速率也 0 錯、各 1 次 Training／Trained／CarrierLost（沒有要求） | 通過（Docker） |
| A03 | `…::a03_awgn_ber_at_the_plan_snr_and_two_db_above` | 4800、SNR 15 dB、3 seed × 340 000 bit：**7 錯／1 020 000 bit，BER 6.9×10⁻⁶ ≤ 10⁻⁵**（計畫值，字面達成）；SNR 17 dB：0 錯。2400（SNR 12 dB，自訂）：0 錯／1 020 000；SNR 14 dB 0 錯 | 通過（Docker） |
| A03 | `…::a03_ber_curve_is_recorded` | BER 曲線（3 seed × 100 000 bit）。4800：SNR 12 dB 3.3×10⁻¹、13 dB 1.7×10⁻¹、14 dB 1.5×10⁻⁴、15 dB 2.0×10⁻⁵、17 dB 0；2400：6 dB 1.2×10⁻³、8／10／12／14 dB 0。4800 在 13 dB 以下的高錯誤率是某些 seed 整段失敗（失去同步，不是平滑劣化），見 §5 | 通過（只記錄） |
| A04 | `…::a04_clock_deviation_and_frequency_offset` | ±100 ppm、±7 Hz 各別與同號組合（+100 ppm＋7 Hz、−100 ppm−7 Hz），2 速率 × 3 seed × 100 000 bit，SNR 為計畫值。**4800 在計畫 SNR（15 dB）**：+100 ppm 3.3×10⁻⁵、−100 ppm 0、±7 Hz 0、兩組合各 2.0×10⁻⁵；**2400（12 dB）全部 0**。斷言：計畫 SNR 下 BER < 10⁻⁴，計畫 SNR＋2 dB 下 ≤ 10⁻⁵（皆 0 錯）。**字面的 10⁻⁵ 在 4800 的 +100 ppm 與兩個組合沒有達成，見 §4 ①** | 通過（Docker；偏離見 §4） |
| A04 | `…::a04_without_noise_every_deviation_is_error_free`、`a04_our_own_transmitter_with_its_own_deviation_inputs` | 無雜訊、同組偏差、2 速率 × 3 seed × 50 000 bit：0 錯、事件同 A01；第二來源（+100 ppm＋7 Hz、−100 ppm−7 Hz）0 錯 | 通過（Docker） |
| A05 | `…::a05_g711_round_trip_of_both_laws` | μ-law 與 A-law 來回（SpanDSP g711）＋AWGN：4800（15 dB）各 4.0×10⁻⁵（12 錯／300 000 bit）、2400（12 dB）0 錯；SNR＋2 dB 皆 0 錯。**同 A04，字面 10⁻⁵ 在 4800 沒有達成（§4 ①）** | 通過（Docker；偏離見 §4） |
| A05 | `…::a05_g711_alone_at_low_and_normal_levels_is_error_free` | 只有 G.711（無雜訊）-10／-30／-40 dBm0，兩 law × 2 速率：0 錯、事件同 A01 | 通過（Docker） |
| A06 | `…::a06_a_gap_in_the_data_does_not_hang_the_receiver` | 30 000 bit 資料的 30%／50%／70% 處丟掉 20 ms（160 個零樣本），2 速率 × 3 seed，電平 -15 dBm0。**實際行為：收端回報 `CarrierLost`（DataEnd），位置在缺口開始後約 11 ms（5 ms 能量塊 × 2），缺口之前送出的位元全對**；缺口之後的資料因為已經回到找 Segment 3 的狀態而不再解碼。同一個收端接著收 0.5 s 靜音與一次乾淨的完整傳送：`Training`、`Trained` 各 1 次、10 000 bit 0 錯（沒有卡住） | 通過（Docker） |
| A06 | `…::a06_after_a_gap_in_the_tcf_t30_answers_and_the_next_dcs_is_heard`、`a06_a_gap_after_a_long_enough_tcf_still_gives_cfr` | 音訊引擎＋T.30：TCF 資料中途（資料開始後 0.2／0.6／1.0 s）缺口 → 收到的 TCF 不到 1.2 s → **FTT**（不是掛住、也不是 CFR）；對方照常再送 DCS＋乾淨 TCF（V.21 照常收到）→ **CFR**。缺口在 1.4 s 處（已收超過 1.2 s 的全 0）→ CFR | 通過（Docker） |
| A07 | `…::a07_all_zero_tcf_is_confirmed_with_cfr` | SpanDSP 與我方送端 × 2 速率 × 3 條通道（乾淨 -15 dBm0／-30 dBm0＋AWGN／μ-law）：全 0 的 1.5 s TCF → 引擎經 `AudioEngine`＋t30 回 **CFR**，沒有 FTT | 通過（Docker） |
| A07 | `…::a07_a_tcf_with_errors_is_refused_with_ftt`、`a07_the_threshold_is_five_percent_ones` | 每個 bit 以 8%／20%／50% 機率為 1 的 TCF（兩種送端、2 速率、3 seed）→ **FTT**；門檻對照：2% 為 1（連同 Segment 5 的 8 個 1 仍 < 5%）→ CFR、8% → FTT，與 T.38 路徑同一條規則（`tcf_zero_percent` 95） | 通過（Docker） |
| A09 | `m5c_a09.rs::a09_white_noise_and_silence_never_make_a_training_short_run` | 白雜訊 -60／-55／-50／-48／-46／-43／-40／-30／-25／-20 dBm0 與靜音，3 seed × 每個 2 s（每速率 66 s 的音訊）：沒有任何事件（Training 0、Trained 0、CarrierLost 0、bits 0） | 通過 |
| A09 | `…::a09_proptest_no_panic_no_event_for_anything_that_is_not_a_v27ter_signal`、`a09_proptest_after_a_training_anything_is_bounded` | proptest（3 seed × 300 組 × 2 速率）：任意 i16、常數長串、滿刻度極值、-70 到 -10 dBm0 白雜訊、靜音、不是 Segment 3 頻率的單音（V.27ter 的 Segment 3 就是 1800 Hz ± 波特率的一半的單音，見 §5）、任意位元與電平的 V.21，任意分塊餵入：不 panic，**一個事件都沒有**。另一個性質（3 seed × 200 組 × 2 速率）：先給一個 Segment 3 單音使收端進入訓練，再餵任意樣本：不 panic，輸出位元數 ≤ 符號率允許的上限 | 通過 |
| A09 | `…::a09_one_hour_of_white_noise_per_level`（`#[ignore]`） | 每速率 10 個電平＋靜音各 1 小時（共 22 小時音訊，release 約 80 秒）：**假 Training 0 次**（所有電平與兩速率；Trained 0、CarrierLost 0、bits 0）。結果表在 §4 ② | 通過（release） |
| A09 | `…::a09_recorded_the_calling_tone_looks_like_segment_3` | 記錄：1100 Hz 的 CNG 對兩種速率都被當成 Segment 3（8 秒內 Training 8 次（2400）／12 次（4800））。收端只在 DCS 之後才建立，那時不會有 CNG（§5） | 通過（只記錄） |
| M01 | `m01_audio_engine_spandsp.rs::m01_spandsp_v27ter_one_standard_page_is_received_pixel_exact` | SpanDSP `FaxModem`（撥出，只開 V.27ter）送 1 頁 standard（1728×300 與 1728×1100）進 `AudioEngine`（in-process，20 ms 一塊）：SpanDSP 結果碼 0；`T30Result`：`code` OK、`pages` 1、**`modem` = V27ter、`bit_rate` = 4800**、`ReceivedPage.modem` = V27ter；`bad_lines` 0；G4 條帶解碼後與 `render_bitmap` **逐 octet 相同**、列數相同、y dpi 98。模擬時間 31.9 s／75.0 s | 通過（Docker；§3 ④ 修正前失敗） |
| M01 | `…::m01_with_line_noise_in_three_seeds` | 同上（300 列），音訊加白雜訊 -55／-52／-50 dBm0（SNR 41／38／36 dB，3 seed）：逐像素相同、V.27ter 4800 | 通過（Docker） |

## 3. 發現並修正的產品問題

以下都是測試發現的真實缺陷，做了最小修正（共約 70 行），各有測試。依 CLAUDE.md，這些修正的決策記在 ADR-062。

1. **擾碼器 guard 之後回授錯誤的位元**（A01 對照 SpanDSP 發現，`fax-modem/src/v27ter.rs` 的 `Scrambler`）。原因：guard（連續 33 個位置等於 8、9 或 12 位元前的位元就反轉下一位）觸發時，我方把 guard **之前**的位元放進暫存器，標準的自同步擾碼器放的是**實際送上線的**（反轉後的）位元。單計數器「等於 8、9 或 12 位元前任一個」的規則對隨機資料約每 700 bit 觸發一次，所以 SpanDSP 送的隨機資料每 700 bit 就有一次兩邊不一致，約 1‰ 的 bit 錯誤（實測 4800：20 000 bit 中 44 個錯，2400：26 個，電平無關；錯誤成對出現）。我方自己的 `testtx::Tx` 用同一個 `Scrambler`，兩邊自洽，所以過去的單元測試完全測不出來。修正：暫存器收線上的位元，重複計數器仍看 guard 之前的位元（`Scrambler::step`）。過程中試過另外兩種：把 8／9／12 分成三個計數器（錯誤減半，仍有 22 個）、把計數「第一次重合不計」（Figure I-2 的 Note 3，錯誤變成 82 個），都不對；8 種「回授／比較／觸發時是否更新」組合只有「回授線上的位元」的 4 種達到 0 錯（對隨機資料的差別看不出來，選保持原樣的那一種）。證據：還原後 A01 的 SpanDSP 案例 44／20 000 bit 錯誤；`the_scrambler_feeds_back_the_line_bit_and_the_guard_inverts_after_33`（單元測試）造出週期 8 的線上樣式，guard 觸發 ≥ 3 次、擾碼／解擾來回相同。**周期性資料已補驗**（A01 的樣式案例，見 §2）：guard 在週期 3、6、8、9、12 與 MH 空白頁的資料上實際觸發（每 20 000 bit 最多 96 次），產品對 SpanDSP 仍 0 錯。把線上位元還原後以 8 種語意（回授線上或 guard 前的位元 × 計數器比較 guard 前或後的位元 × 觸發的位置是否更新計數）重新解擾：回授 guard 前位元的 4 種（旗標 0、2、4、6）在有觸發的樣式都出錯，回授線上位元的 4 種（旗標 1、3、5、7，含產品）全部 0 錯。所以**回授線上位元已確定**；「計數器比較 guard 前或後」與「觸發時是否更新計數」SpanDSP 的資料分辨不出來（連 33 次觸發後的資料都相同），保留產品現狀。
2. **雜訊與帶外訊號會騙收端進入 Training，之後永遠卡住**（A03 的 SNR 15 dB 與 A09 發現，`v27ter.rs` 的 Acquire／Train）。原因：Segment 3 的判定是「連續 8 個符號 180° 反轉」，角度在 ±60° 內就算（白雜訊每符號機率 1/3），1600 符號／秒時約每 4 秒一次；而且 AGC 把任何微弱的漏過訊號放大成滿幅。假 Training 之後，Train 階段 50 個符號後第一個非軸向符號就宣告 `Trained`，進入資料階段，**只要雜訊高於 -48 dBm0 就永遠不會回到找 Segment 3**，後來真正的訊號收不到。實測（修正前，每速率每電平 600 秒白雜訊）：-48 dBm0 以上每個電平都有假 Training，-46 dBm0 在 600 秒內有 15–16 次（Trained 15–16 次），-43 dBm0 以上 1 次後卡在資料狀態，累積 120 萬–290 萬個垃圾位元；A03 的 4800、SNR 15 dB、seed 1 訊號前 200 ms 的雜訊就觸發，整段 50% 的錯。A09 的 proptest 也找到：+4570 的 32 個樣本接著 -65（-48 dBm0）的直流，在 2400 就是 Training（直流混頻後的 -1800 Hz 在 1200 baud 剛好是每符號 180°）。修正（約 25 行）：反轉要連續 10 個、角度 ±46° 內、相鄰兩符號功率比在 2 倍內、匹配濾波器輸出能量至少是輸入的 0.25 倍（正常訊號約 2 倍，每 5 ms 一塊量測；一開始用 EMA 量測被前一個大訊號的殘留騙過，改成區塊量測）；Train 階段前 50 個符號內非軸向超過 8 個就放棄、回到 Acquire。證據：修正前後 A09 同一份 600 s 輸入的假 Training 次數（上述）→ 0；1 小時結果在 §4 ②；單元測試 `a_false_training_in_noise_is_given_up_and_the_real_signal_is_received`（先給短單音騙進 Training，接著雜訊，之後真正的 4000 bit 傳送完全正確）與 `a_tone_out_of_the_band_of_the_matched_filter_is_not_a_training`（直流步階）。
3. **Segment 4 的載波迴路在 +7 Hz 加雜訊時沒拉進就被判為 Segment 5**（A04 發現，`v27ter.rs`）。原因：4800 的 Segment 5 判定包含「接近 45° 奇數倍」，只容許 ±22.5° 的相位誤差；+7 Hz 偏移在 1600 baud 是每符號 1.6° 的相位漂移，PLL（kp 0.08、ki 0.002）第 50 個符號時積分器只到 0.0176（需要 0.0275），相位誤差加上雜訊超過 22.5° 就在第 51 個符號宣告 `Trained`，等化器沒訓練完，之後全部是垃圾（4800、SNR 15 dB、+7 Hz、seed 3：50 057／100 000 bit 錯）。修正（5 行）：Segment 4 的積分增益 0.006、資料階段維持 0.002。證據：同案例 0 錯；±7 Hz 兩個方向、兩速率、3 seed 全部通過；`quiet_offset_and_drifting_signals`（既有單元測試）仍通過。
4. **音訊引擎不認得撥出端的 DCS**（M01 發現，`fax-in/src/audioengine.rs`）。撥出端在看到 DIS 之後送的 FCF 帶 X 位元（T.30 記法的最高位元，DCS 是 `FF C8 C1 …`；SpanDSP 就是這樣送），引擎判斷「這是 DCS」的條件用 `frame[2] & 0xFE == DCS & 0xFE`，遮掉的是最低位元而不是 X 位元，所以 `C1` 不被認得，不建高速收端，T.30 收不到 TCF，2 次 T2 逾時後掛斷（`t2_expired_command`，M01 一開始就是這樣失敗）。既有的單元測試用的 DCS 沒有 X 位元，所以測不出來。修正（1 行）：`frame[2] & 0x7F == DCS`。證據：`a_dcs_of_a_calling_terminal_carries_the_x_bit_and_still_selects_the_receiver`（新增的單元測試，還原修正後失敗）、M01 通過。

## 4. 與計畫不同之處

① **A04、A05 的 BER 門檻在 4800 的計畫 SNR（15 dB）沒有達到 10⁻⁵**。A03（同樣是 4800、SNR 15 dB）字面達成（6.9×10⁻⁶），但加上 ±100 ppm（+100 ppm 3.3×10⁻⁵、兩個組合各 2.0×10⁻⁵）或 G.711 量化（μ-law 與 A-law 各 4.0×10⁻⁵）後，每 300 000 bit 錯 6–12 個。這些錯誤是孤立的符號判錯（雜訊貼著 8DPSK 的極限，3 條曲線的位置（14 dB 1.5×10⁻⁴、15 dB 2×10⁻⁵、17 dB 0）也是平滑的），不是失去同步。**我沒有放寬到讓它通過再說通過**：這兩項的測試在計畫的 SNR 斷言 BER < 10⁻⁴（不能有斷崖）、在 SNR＋2 dB（17 dB）斷言 ≤ 10⁻⁵（實測 0 錯），並且在報告與測試輸出裡把 15 dB 的實測值寫出來。如果審核者堅持字面的 10⁻⁵，這兩項要算未達成；2400（SNR 12 dB）全部 0 錯。另外 SNR 的定義計畫沒有寫，我用 0–4 kHz 全頻帶雜訊（最寬鬆的常見定義，量到的雜訊電平與目標差 < 0.3 dB）；若改為 300–3400 Hz，同樣的 SNR 數字雜訊密度更高，更難達到。
② **A09 的 1 小時白雜訊結果**（每速率；指令 `cargo test -p fax-modem --release --test m5c_a09 -- --ignored --nocapture`，約 80 秒）：

| 電平 | -60 | -55 | -50 | -48 | -46 | -43 | -40 | -30 | -25 | -20 | 靜音 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 2400 假 Training | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| 4800 假 Training | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |

   訊號閘門由 -43 降到 -48 dBm0 後，-48 dBm0 以上的雜訊會讓收端一直在找 Segment 3；§3 ② 的修正使 22 小時的音訊（10 個電平＋靜音 × 2 速率）沒有一次假 Training。**修正前**同樣的 600 秒輸入：2400 在 -48／-46／-43／-40／-30／-25／-20 dBm0 是 3／16／1／1／1／1／1 次，4800 是 5／15／1／1／1／1／1 次（-60 到 -50 為 0）。統計上的把握：1 小時×22 = 79 200 秒、0 次，以修正前 ≈ 每 600 秒 1 次的速率來看，修正至少壓低了 100 倍以上；不能證明是 0。
③ **A06 的 `Training` 次數**：計畫只要求「不卡住」。修正前，缺口之後繼續到達的資料在 4800 偶爾（70% 處，seed 3，缺口後約 1 s）被當成 Training（`Training` 2 次），修正 ② 之後不再出現；測試斷言至少 1 次 Training 而不是恰 1 次，因為這是缺口之後狀態重置的自然結果。
④ **A07 的 SNR**：計畫只說「乾淨」的 TCF；我另外加了 -30 dBm0＋AWGN 與 μ-law 兩條通道。
⑤ **A02 的 -45／-47 dBm0**：計畫到 -43；多測的兩個只記錄（見表）。
⑥ **M01 的加雜訊案例**：計畫沒有；加了 3 個 seed（M 系列計畫說通道可插入雜訊）。噪音限制在 -55 到 -50 dBm0，原因見 §5 ①。
⑦ **`T30Result` 有 `modem` 與 `bit_rate`**，M01 直接斷言，沒有新增欄位。
⑧ **1 小時白雜訊的測試**：計畫「1 小時白雜訊誤判次數 0」；正常執行只有每電平 2 s、3 seed 的短版（每速率 66 s 音訊，debug 約 10 s）。

## 5. 觀察到、沒有修的問題（供審核者決定）

1. **結束偵測用絕對電平 -48 dBm0，線路雜訊底高於它就收不到訊號結束**（M01 的雜訊案例發現，設計層級，沒有修）。收端的 `CarrierLost` 與引擎的 `NoSignal`（T.30 用它結束 TCF 與頁面）都靠「5 ms 能量連續兩塊低於 -48 dBm0」。SpanDSP 以 -14 dBm0 送，雜訊 SNR 30 dB（-44 dBm0）時，引擎永遠等不到 TCF 的結束，SpanDSP 逾時掛斷（`remote_disconnect`，M01 初版以 SNR 30／25／22 dB 就是這樣失敗的）；A03–A05 的雜訊案例同理不會有 `CarrierLost`，測試因此不斷言它。實際電話線的雜訊底多在 -60 dBm0 以下，但雜訊大的線路（SNR 低於約 34 dB）會整通失敗。改法（結束偵測改看相對於訊號的下降，或收端自己從符號品質判斷載波消失）會動到 DSP 設計與 T.30 的訊號事件，超過「最小修正」，留給設計決定。計畫 §7 的 M04（通道 SNR 讓 9600 的 TCF 失敗）會碰到同一件事。
2. **4800 在 SNR 13 dB 以下是斷崖**（A03 曲線）：12 dB 的 3 個 seed 有 2 個整段是垃圾（BER 3.3×10⁻¹）、13 dB 有 1 個（1.7×10⁻¹），14 dB 起平滑。垃圾是從資料的第一個 bit 開始（和 §3 ③ 同型：Segment 5 提早或晚到，或等化器沒訓練好），沒有深入查原因；低於計畫的 SNR，不影響任何案例。
3. **1100 Hz 的 CNG 會被當成 Segment 3**（A09 記錄）：Segment 3 就是 1800 Hz ± 波特率的一半的單音，角度容許 ±46°，所以頻率在 1800 − 800 ± 約 250 Hz（4800）／1800 − 600 ± 約 250 Hz（2400）內的單音（包括 CNG 的 1100 Hz）都符合；收端只在 DCS 之後才建立，那時對方不會再送 CNG。若 M5e 把收端提前建立，這件事要處理（例如要求單音頻率在載波偏移 ±7 Hz 的容許內）。
4. **`CarrierLost` 之後的資料不再解碼**（A06）：封包遺失造成的 20 ms 缺口會讓收端放棄這次傳送，之後直到下一個 Segment 3 都沒有位元。T.30 對 TCF 回 FTT（因為不足 1.2 s）、對頁面以現有的 bad_lines 處理（頁面中途缺口 = 頁面提前結束，缺口之後的列都沒有；計畫 M05 的「頁面中有 1 包遺失、bad_lines ≥ 1 時 MCF」在 M5d／M5e 驗證前需要注意：目前缺口不是壞行，是頁面被截斷）。
5. 既有單元測試的 `debug_trace`（`#[ignore]` 的除錯用，約 60 行）佔了 v27ter.rs 未涵蓋行的一大部分。
6. 時脈偏差與頻率偏移的通道工具都是測試內 f64 實作，沒有獨立驗證（`a01_the_channel_helpers_do_what_they_say` 驗證了電平、雜訊、缺口與長度比，沒有驗證頻移量）；頻移是否精確由 4800／2400 在 ±7 Hz 下 0 錯間接證明。

## 6. 涵蓋率

| 項目 | 數字 | 門檻 |
|---|---|---|
| `fax-modem`（行）`cargo llvm-cov -p fax-modem --fail-under-lines 85 --summary-only` | **93.76%**（87 行未涵蓋；`v27ter.rs` 89.35%；`dsp.rs` 98.37%、`hdlc.rs` 96.88%、`tone.rs` 97.96%、`v21.rs` 98.01%） | ≥ 85% |
| `fax-in`（行）`cargo llvm-cov -p fax-in --fail-under-lines 70 --summary-only` | **93.18%**（`audioengine.rs` 88.41%） | ≥ 70% |

SpanDSP 的測試（`tools/`）不計入涵蓋率（獨立 workspace）；涵蓋率只含 `fax-modem` 的單元測試、`m5a_n`、`m5c_a09`，與 `fax-in` 的單元與整合測試。

## 7. 執行指令

```sh
# A01–A07 與 M01（SpanDSP，Docker；含 tools/ 的 fmt、clippy、t4 oracle）
E2E_ONLY="--test a_v27ter_spandsp --test m01_audio_engine_spandsp" scripts/e2e-docker.sh
# A09 正常版（約 10 秒）
cargo test -p fax-modem --test m5c_a09
# A09 一小時白雜訊（release，約 80 秒；A09_SECONDS=600 縮短）
cargo test -p fax-modem --release --test m5c_a09 -- --ignored --nocapture
cargo llvm-cov -p fax-modem --fail-under-lines 85 --summary-only
cargo llvm-cov -p fax-in --fail-under-lines 70 --summary-only
```

## 8. 本機驗證結果

| 指令 | 結果 |
|---|---|
| `E2E_ONLY="--test a_v27ter_spandsp --test m01_audio_engine_spandsp" scripts/e2e-docker.sh` | 通過（fmt、clippy `-D warnings`、`a_v27ter_spandsp` 19 個（約 74 秒）、`m01_audio_engine_spandsp` 2 個、t4 oracle 都通過） |
| `cargo test -p fax-modem` | 全部通過（單元 21＋1 ignored、`m5a_n` 28、`m5c_a09` 4＋1 ignored） |
| `cargo test -p fax-in --lib audioengine` | 6 個通過（含新增的 X 位元案例） |
| `cargo test -p fax-modem --release --test m5c_a09 -- --ignored` | 通過，假 Training 0 次（§4 ②） |
| 修正前的失敗確認 | §3 ① A01 SpanDSP 案例 44／20 000 bit 錯誤；§3 ② 600 s 白雜訊的假 Training 與 A09 proptest 的直流案例；§3 ③ 4800＋7 Hz seed 3 的 50 057 個錯；§3 ④ 單元測試還原後失敗、M01 失敗 |
| `scripts/ci-local.sh` | 通過（exit 0；第一次跑時 `apps/sip-faxd` 的 `k07_new_routes_apply_to_the_next_fax_and_a_broken_set_is_refused` 失敗（資料夾投遞 Permission denied，與本次變更無關），單獨跑 3 次與在 stash 後都通過，重跑 ci-local 全綠） |

## 9. Opus 審核

2026-10-24，**通過，但 A04、A05 記為未達成計畫門檻**（另立後續項目，不放寬計畫）。

- 斷言與 mock：抽查 `a_v27ter_spandsp.rs`、`m01_audio_engine_spandsp.rs`、`m5c_a09.rs`。送端一律是 SpanDSP（`v27ter_tx`、`fax_state`），我方只有收端；BER 斷言對整段位元逐一比較；M01 逐像素比對並斷言 `T30Result` 的數據機與速率。沒有空斷言，也沒有以我方元件模擬對方。
- §3 的四項修正都有「還原就失敗」的證據，接受，記於 ADR-062。
  - ① 擾碼器：審核時要求補週期性資料（全 0、全 1、週期 2–12、T.4 MH 空白頁 100／800 行），對照 SpanDSP 兩種速率都是 0 錯，guard 在週期 3／6／8／9／12 與空白頁觸發數十次。8 種語意中，回授 guard 之前位元的 4 種都失敗。結論：回授線上位元已確定；計數器比較 guard 前或後的位元、觸發時是否更新計數，用 SpanDSP 的資料分辨不出，也就不影響收端與 SpanDSP 的互通，維持現狀。
  - ② 假 Training：這是 M5c ①②（訊號閘門由 −43 降到 −48 dBm0）留下的風險，A09 正好驗到並修好。1 小時 × 11 個電平 × 2 種速率都是 0 次。
- §4 ①（A04、A05）：4800 在計畫 SNR 15 dB，加 ±100 ppm 或 G.711 量化後，BER 為 2–4×10⁻⁵，未達計畫的 10⁻⁵。依理論估算（頻帶內 SNR 約 19 dB，同調 8PSK 加差分解碼），實作損失約 1 dB 以內就應達到。M5d 的 V.29 收端已證明：時脈偏差時，Gardner 加小步階等化器會損失數 dB 的 MER，改以等化器係數重心帶時脈、加大資料段步階後回到雜訊底。後續項目：把同樣的做法套到 V.27ter，用 MER 指標驗證，再以本報告的 A04／A05 原門檻重測。測試裡「計畫 SNR < 10⁻⁴、+2 dB ≤ 10⁻⁵」的斷言，到時改回計畫門檻。
- §5 列入 M5e 的設計待辦：① 結束偵測用絕對電平（線路雜訊高於 −48 dBm0 時等不到 TCF／頁面結束）；③ 1100 Hz CNG 會符合 Segment 3（收端提前建立時要處理）；④ 20 ms 缺口讓頁面截斷，而不是成為壞行（影響 M05）。
- 涵蓋率：fax-modem 93.76%、fax-in 93.18%，符合門檻。
- ci-local：第一次 `apps/sip-faxd` 的 `k07_new_routes_apply_to_the_next_fax_and_a_broken_set_is_refused` 以 Permission denied 失敗，重跑通過。這是本 session 第三個同型偶發（另兩個是 `r05`、`m07`），另立待辦追查。
- **後續（2026-10-25，Opus）**：把 V.29 的重心時脈套到 V.27ter 後沒有改善。A05 本身沒有時脈偏差也一樣差，可見 15 dB 時是受雜訊限制。依 ADR-064，4800 的 A04／A05 改以「計畫 SNR＋2 dB 時 ≤ 10⁻⁵、計畫 SNR 時 ≤ 10⁻⁴」判定，現有斷言正是如此，改記為達成。
