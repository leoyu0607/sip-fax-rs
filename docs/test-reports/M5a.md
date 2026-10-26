# M5a 測試報告：RTP／G.711、jitter buffer、訊號音與能量偵測

- 實作：Opus（`crates/sip-core/src/rtp/{g711,packet,jitter}.rs`、`crates/fax-modem/src/{level,goertzel,tone}.rs`，commit dfd7180）；測試與本報告：Sonnet（依 Opus 的測試計畫）
- 計畫：`docs/test-plans/M5.md` §0–§4；ADR-061
- 分支：`m5`（本報告在其上的工作分支）
- 範圍：G01–G04、J01–J05、J07、N01–N04。**G05（SDP 協商）與 J06（送端 20 ms pacing）延到 M5e**（程式還沒有實作）。
- 狀態：Opus 審核通過（2026-10-26，見 §9）

## 1. 環境

| 項目 | 內容 |
|---|---|
| 主機 | Windows 11（Git Bash），Docker Desktop；Rust 1.99.1 |
| `sip-core` 測試（G、J） | `crates/sip-core/tests/m5a_g.rs`（G01–G04）、`crates/sip-core/tests/m5a_j.rs`（J01–J05、J07）。純 Rust，不開 socket、不用真實時間：J 案例以虛擬時鐘驅動（第 k 個 tick 在 `20 000·k` µs：先把到達時間不晚於此刻的封包 push，再 pull 160 個樣本，與 fax-in 預定的用法相同） |
| `fax-modem` 測試（N） | `crates/fax-modem/tests/m5a_n.rs`。訊號以標準函式庫的 `f64` 正弦獨立產生，不用被測的 `ToneGenerator`（N03 除外，那是被測物） |
| 獨立對照（SpanDSP） | `tools/fax-sim/tests/g01_g02_g711_spandsp.rs`、`tools/fax-sim/tests/n03_ced_spandsp.rs`；`scripts/e2e-docker.sh`（Rocky 9 映像 `sip-fax-rs-test:rocky9`，SpanDSP v3.1.1）。FFI 在 `tools/spandsp-sys/src/lib.rs` 補上 `g711_init/free/encode/decode` 與 `modem_connect_tones_rx_init/free/rx/get`（只在 `tools/`，主執行檔不含 SpanDSP） |
| 隨機性 | 固定 seed 的 xorshift64*（不依賴外部 crate）；每個用到隨機的案例至少 3 個 seed（`0x5EED_000{1,2,3}`、`0xC0FFEE0{1,2,3}`、`0xCED0_000{1,2,3}`）。proptest 以 `TestRunner::new_with_rng(ChaCha, 固定 seed)` 執行，每個性質 3 個 seed（G04 為 4 個），關閉 failure persistence（沒有 regression 檔） |
| `fax-modem` 新增相依 | dev-dependencies：`proptest`（workspace） |

SpanDSP 的 `alaw_to_linear`、`ulaw_to_linear`、`linear_to_alaw`、`linear_to_ulaw` 在 `g711.h` 是 `static inline`，函式庫沒有匯出。FFI 改呼叫匯出的 `g711_encode`／`g711_decode`，它們就是用這四個 inline 函式逐樣本處理緩衝區，結果等價。

## 2. 案例對照表

測試函式以案例編號開頭。「獨立對照」欄是不由被測程式碼產生的比對來源。

| ID | 測試位置 | 驗證內容 | 結果 |
|---|---|---|---|
| G01 | `m5a_g.rs::g01_ulaw_decoding_of_all_256_codes_equals_the_g711_table`、`g01_alaw_decoding_of_all_256_codes_equals_the_g711_table` | 256 個碼值逐一與測試內以 G.711 表格式（Table 1a／2a：段落基底與步階，μ-law `4·(((2M+33)<<S)−33)`、A-law `8·(2M+1)` 或 `8·((2M+33)<<(S−1))`）另行寫成的解碼公式比對；`const fn` 與查表（`Law::decode_one`）兩條路徑都比 | 通過 |
| G01 | `m5a_g.rs::g01_literal_values_from_the_recommendation`、`g01_decode_of_a_buffer_equals_decode_of_each_code` | 手寫字面值（μ-law 0xFF／0x7F／0xFE／0xFD／0xF0／0xEF／0x80／0x00／0x7E／0x70；A-law 0xD5／0x55／0xD4／0xD7／0xC5／0xC4／0xAB／0xAA／0x2A）；批次解碼等於逐碼解碼且附加在輸出尾端 | 通過 |
| G01 | `tools/fax-sim/tests/g01_g02_g711_spandsp.rs::g01_decoding_all_256_codes_equals_spandsp_alaw`、`…_ulaw`、`g01_spandsp_state_is_released_and_reusable` | 256 個碼值的解碼與 SpanDSP 完全相同（兩種 law），批次解碼也相同；FFI 包裝本身（兩個實例並存、釋放） | 通過（Docker） |
| G02 | `m5a_g.rs::g02_every_16_bit_value_lands_in_the_interval_of_its_code` | 全部 65536 個輸入、兩種 law：編出的碼對應的量化區間（半步階 +1）包含輸入；超出最高階的值被截到最大值且正負號正確 | 通過 |
| G02 | `m5a_g.rs::g02_encoding_is_monotonic_in_the_input`、`g02_decode_then_encode_returns_the_same_code`、`g02_every_sample_is_idempotent_through_the_codec`、`g02_full_scale_and_silence`、`g02_buffer_encode_equals_encode_of_each_sample_and_appends`、`g02_quantization_error_stays_within_the_segment_step` | 編碼對輸入單調；256 個碼 decode→encode 回到同一碼（μ-law 的兩個零：0x7F 回 0xFF，解碼值相同）；65536 個輸入 encode→decode→encode→decode 不變（idempotent）；靜音碼、滿刻度截斷、**μ-law 的奇對稱（−x 與 x 的碼只差正負號位元，修正後，見 §3）**與 A-law 的 1 補數關係；批次編碼 | 通過 |
| G02 | `tools/fax-sim/tests/g01_g02_g711_spandsp.rs::g02_encoding_all_65536_values_equals_spandsp_alaw`、`…_ulaw`、`g02_the_two_implementations_agree_through_a_full_round_trip` | 65536 個輸入的編碼與 SpanDSP 逐一相同（兩種 law）；我方編碼→SpanDSP 解碼、SpanDSP 編碼→我方解碼的樣本相同；兩邊各自再編解一次不變 | 通過（Docker；**修正前 μ-law 有 127 個輸入不同，見 §3**） |
| G03 | `m5a_g.rs::g03_every_csrc_count_from_0_to_15_is_read_and_stays_out_of_the_payload`、`g03_marker_and_payload_type_bits`、`g03_extension_of_0_1_and_many_words`、`g03_padding_is_cut_from_the_payload`、`g03_an_empty_payload_is_a_valid_packet` | 手工組出的封包（不用 `encode`）：CSRC 0–15 個、marker、payload type 0／8／18／96／101／127、extension 0／1／2／17／255 個 word、padding 1／2／4／255 與整個剩餘都是 padding、三者並存；每個欄位正確且 payload 不含 CSRC、extension、padding | 通過 |
| G03 | `m5a_g.rs::g03_version_other_than_2_is_refused`、`g03_truncated_header_csrc_list_and_extension_are_refused_at_every_length`、`g03_bad_padding_is_refused` | 版本 0／1／3 回 `Version`；含 CSRC 與 extension 的封包被截斷到每一個長度（0 到全長減 1）都回 `Truncated(長度)`；extension 宣告的長度超過、X 位元後沒有資料；padding 為 0、比剩餘長 1、255、P 位元但無資料、padding 吃進 extension 都回 `Padding` | 通過 |
| G03 | `m5a_g.rs::g03_the_sipress_decoder_took_everything_after_byte_12_as_payload`、`g03_encode_limits_csrcs_to_15_and_pads_the_extension` | sipress 原本的錯誤（payload＝第 12 byte 之後全部）：有 CSRC＋extension＋padding 時長度確實不同，新 decode 的 payload 正確（對應 `docs/sipress-extraction.md`）；encode 最多寫 15 個 CSRC、extension 資料補零到 4 byte 倍數 | 通過 |
| G04 | `m5a_g.rs::g04_encode_then_decode_is_the_identity` | proptest（4 個 seed × 2000 組）：任意欄位（CSRC 0–15、可有可無 extension、payload 0–400 byte、任意 seq／timestamp／ssrc）encode→decode 相等，且長度恰為標頭＋CSRC＋extension＋payload | 通過 |
| G04 | `m5a_g.rs::g04_decode_of_arbitrary_bytes_never_panics_and_a_decoded_packet_survives_a_round_trip`、`g04_the_g711_payload_survives_the_wire` | proptest（3 個 seed × 2000 組）：任意 0–300 byte 不 panic，decode 成功者 payload 不長於輸入且重編碼再解碼相同；20 ms 兩種 law 經 RTP 往返後解碼的樣本相同 | 通過 |
| J01 | `m5a_j.rs::j01_in_order_20_ms_output_equals_input_delayed_by_the_start_depth` | 3 個 seed × 起始深度 1／2／3／4／8 包，150 包依序、固定 20 ms：輸出逐樣本等於輸入，沒有 late／duplicate／dropped／resync／gap；開始播放時 buffer 恰為起始深度，第一塊在 tick `depth/160−1` 輸出（見 §4 ①） | 通過 |
| J01 | `m5a_j.rs::j01_the_depth_is_a_sample_count_not_a_packet_count`、`j01_nothing_plays_before_the_start_depth_and_pull_is_silent` | 10 ms 封包（80 樣本）以樣本數計深度；深度未到時 pull 回靜音與 `Pulled::default()`；空封包被忽略、空的輸出緩衝區可以 pull | 通過 |
| J02 | `m5a_j.rs::j02_jitter_of_40_ms_and_5_percent_reordering_give_the_input_back_sample_for_sample` | 3 個 seed，1500 包（30 s），到達時間 ±40 ms 均勻、約 5% 與下一包對調到達時間：輸出逐樣本等於輸入，沒有任何補入或遺失（深度 160 ms，見 §4 ②） | 通過 |
| J02 | `m5a_j.rs::j02_with_a_depth_below_the_jitter_nothing_is_misplaced_or_invented`、`j_proptest_any_order_within_the_depth_gives_the_stream_back` | 預設深度（60 ms）遇到 ±100 ms：每塊不是靜音（回報 gap）就是完整的某個封包，封包依序不重複，已收的封包都播出（3 個 seed，實測 124–311 個 gap 塊）；proptest（3 個 seed × 200 組）：80／160 樣本混合、4 包視窗內任意重排、深度 6 包：輸出等於來源 | 通過 |
| J03 | `m5a_j.rs::j03_independent_loss_of_1_percent_…`、`…_3_percent_…`、`j03_bursts_of_3_lost_packets_1_and_3_percent` | 3 個 seed × 5000 包（100 s）、到達抖動 ±10 ms：獨立遺失 1%、3%，連續 3 包遺失 1%、3%（共 12 組）。每一塊要不是原封包，就是 160 個零且 `gap=160`；遺失之後的封包都在 timestamp 決定的位置（逐塊比對）；gap 總數＝遺失數×160；late／resync／dropped 為 0 | 通過 |
| J03 | `m5a_j.rs::j03_a_burst_of_many_lost_packets_…`、`j03_the_first_packets_lost_…`、`j03_a_duplicate_and_a_late_packet_are_dropped_and_counted_not_played` | 連續遺失 50 包（1 s）之後對齊；最前面兩包遺失時從第一個到達的 timestamp 開始播；重複封包計入 `duplicate`、晚到 200 ms 的封包計入 `late` 且該塊是 gap，其餘都在原位 | 通過 |
| J04 | `m5a_j.rs::j04_timestamp_wrap_in_order_and_out_of_order_has_no_gap` | 3 個 seed × 繞回前 1／100／160／161／800／803 個樣本起始，依序與抖動＋10% 對調（跨越繞回點）：輸出逐樣本相同，沒有 gap／late／resync | 通過 |
| J04 | `m5a_j.rs::j04_a_packet_from_before_the_wrap_arriving_after_it_is_late_not_a_resync`、`j04_sequence_number_wrap_through_the_wire_format_and_both_laws` | 繞回前的封包在繞回後才到：計為 late、一個 gap、不是 resync；seq 65530 起（第 6 包繞回）且 timestamp 在第 12 包繞回，封包以真實的 RTP 位元組（`RtpPacket::encode／decode`）與兩種 law 往返＋抖動，輸出等於編解碼後的來源 | 通過 |
| J05 | `m5a_j.rs::j05_a_new_ssrc_starts_over_once_…`、`j05_a_new_ssrc_whose_timestamps_continue_…` | 3 個 seed × 新 SSRC（timestamp 任意、5 000 000、100、接續舊的）：`resyncs` 恰為 1；輸出為舊串流的前綴、之後新串流從第一個樣本完整、無 gap 地播出；舊串流最多少掉一個 buffer 深度 | 通過 |
| J05 | `m5a_j.rs::j05_a_timestamp_jump_beyond_resync_after_forward_and_backward`、`j05_a_jump_below_resync_after_is_a_gap_not_a_resync` | 同 SSRC 的 timestamp 大跳：往前 16 001 之外、往回、往前 10 億、跨 32 位元繞回，每種都恰一次 resync；8000 個樣本（低於 `resync_after`）的跳躍不是 resync，而是 8000 樣本的 gap，之後位置正確 | 通過 |
| J05 | `m5a_j.rs::j05_a_jump_before_playout_started_heals_with_one_resync_at_the_next_packet`、`j05_after_a_resync_the_counters_keep_counting_and_the_buffer_is_usable_again` | 播放開始前的跳躍：下一包 push 時自癒，一次 resync，新串流（扣掉第一包，見 §4 ④）完整；連續 4 次換 SSRC：`resyncs`＝4，每次都能再播 | 通過 |
| J07 | `m5a_j.rs::j07_plus_and_minus_200_ppm_for_10_minutes_the_buffer_stays_inside_its_limits` | 3 個 seed，送端時脈 ±200 ppm，虛擬 10 分鐘（30 000 個 tick）：buffer 全程不超過 `max_depth`（+200 ppm 最大 1280／2400 個樣本）；`resyncs`＝0；樣本守恆（接受的樣本＝播出＋dropped＋尚在 buffer）；−200 ppm 時 buffer 見底一次，補 480 個樣本（3 包）靜音後繼續，來源樣本**一個都沒有少**（連續性逐樣本檢查） | 通過（**修正前失敗，見 §3**） |
| J07 | `m5a_j.rs::j07_with_jitter_on_top_of_the_drift`、`j07_a_much_faster_or_slower_sender_is_trimmed_or_gapped_but_never_overflows`、`j07_a_buffer_that_ran_dry_waits_for_its_depth_and_plays_the_late_packet` | ±200 ppm 外加 ±15 ms 抖動，深度 960／3200；±2000、±10 000 ppm：快的被截到起始深度（`dropped`>0）、慢的見底補靜音，都不溢位、不 resync、樣本守恆、只有 `dropped`／`late` 的地方才有來源樣本跳號；見底後等到起始深度再從見底位置繼續（逐包斷言），遺失超過 buffer 時從第一個到達的封包繼續 | 通過 |
| J（性質） | `m5a_j.rs::j_proptest_any_sequence_of_pushes_and_pulls_never_panics_and_keeps_its_invariants` | proptest（3 個 seed × 400 組）：任意 push（SSRC、任意 timestamp、長度 0–400）與 pull（0–500）序列不 panic；未播放時回靜音且 gap＝0；`gap`≤輸出長度；所有計數器只增不減；播放中接受一個封包後 buffer ≤ `max_depth` | 通過 |
| N01 | `m5a_n.rs::n01_cng_is_heard_in_every_burst_within_400_ms_from_minus_10_to_minus_43_dbm0` | 3 個 seed × 電平 −10／−20／−30／−40／−42／−43 dBm0，1100 Hz 0.5 s on／3 s off，5 個 burst，SNR 20 dB（雜訊全程存在），起點隨機對齊：每個 burst 恰回報一次（off 期間不回報），延遲 300–400 ms（逐樣本餵入量測） | 通過（**−43 dBm0 修正前失敗，見 §3**） |
| N01 | `m5a_n.rs::n01_the_delay_does_not_depend_on_where_the_burst_falls_in_the_analysis_block`、`n01_chunked_feeding_reports_the_same_bursts_in_the_same_chunk`、`n01_a_continuous_tone_is_reported_once_and_a_shorter_burst_not_at_all`、`n01_below_the_threshold_nothing_is_heard`、`n01_with_less_than_the_snr_of_the_plan_the_detector_degrades_gracefully` | 起點每 7 個樣本錯開一次（0–159）最壞延遲 < 400 ms；以 1、7、80、160、161、1000、8000 個樣本為一塊餵入，回報的 burst 數與位置相同；連續 5 s 的 1100 Hz 只回報一次；300 ms 的 burst 不回報、400 ms 的回報；−50／−55／−60 dBm0 不回報；SNR 10 dB 以上全部偵測、3 dB 以下（純度不足）不偵測 | 通過 |
| N02 | `m5a_n.rs::n02_tones_near_but_not_at_1100_hz_are_not_cng` | 300、700、1000、1050、1200、1300、1650、1850、2100、2225、2750 Hz，電平 −10／−20／−35 dBm0，各 4 s：不誤判（逐樣本與 20 ms 一塊兩種餵法） | 通過 |
| N02 | `m5a_n.rs::n02_dtmf_dial_tone_ringback_busy_and_the_other_fax_tones_are_not_cng` | 16 個 DTMF 鍵（50 ms／50 ms 重複 10 次，以及連續 2 s）、撥號音 350＋440、回鈴音 440＋480、忙音 480＋620、V.21 1650／1850、CED 2100、ANSam 式 15 Hz 調幅 2100、1100＋1000 Hz 等功率：不誤判 | 通過 |
| N02 | `m5a_n.rs::n02_speech_like_signals_and_noise_are_not_cng`、`n02_speech_like_stand_in_has_a_speech_like_level_and_spectrum`、`n02_cng_after_other_signals_is_still_heard` | 3 個 seed：語音替身 20 s × 3 個電平、白雜訊 60 s × 3 個電平（含 0 dBm0）、全範圍均勻隨機樣本 30 s：不誤判（**語音是合成的，見 §4 ⑥**）；替身的電平正確、1100 Hz 的能量占比 < 30%；撥號音、語音之後的 CNG 仍然偵測得到 | 通過 |
| N03 | `m5a_n.rs::n03_ced_is_2100_hz_for_3_s_then_75_ms_of_silence`、`n03_ced_frequency_is_2100_hz_within_a_hertz`、`n03_ced_level_follows_the_setting`、`n03_ced_is_clean_continuous_phase_and_blockwise_equal_to_one_fill`、`n03_a_generator_with_other_lengths_and_frequencies`、`n03_cng_detector_does_not_take_ced_for_cng_and_the_goertzel_sees_both` | 預設 3.0 s（在 2.6–4.0 s 內）＋75 ms 全零靜音，總長 24 600，結束後 `fill` 回 0、`remaining` 為 0；頻率以過零與 2085–2115 Hz 的 Goertzel 掃描兩種方式，誤差 < 1 Hz（要求 ±15 Hz）；電平 −10 到 −40 dBm0 誤差 < 0.1 dB、峰值＝振幅；音內沒有超過正弦可能的跳變；以 1、77、160、1000、24 600、30 000 個樣本一塊填入，結果與一次填入逐樣本相同；2.6 s、4.0 s 長度 | 通過 |
| N03 | `tools/fax-sim/tests/n03_ced_spandsp.rs::n03_spandsp_detects_our_ced_at_every_level`、`n03_spandsp_detects_our_ced_with_noise_and_in_three_seeds`、`n03_the_same_receiver_also_takes_ced_given_as_ced_or_preamble` | **SpanDSP `modem_connect_tones_rx`**（`MODEM_CONNECT_TONES_ANS`）偵測我方 CED：−10 到 −30 dBm0；3 個 seed × (−10 dBm0／SNR 30)、(−20／25)、(−30／20) 的雜訊；以 `FAX_CED_OR_PREAMBLE` 模式也偵測得到；偵測發生在音持續期間，不早於起點 | 通過（Docker） |
| N03 | `tools/fax-sim/tests/n03_ced_spandsp.rs::n03_spandsp_does_not_take_other_tones_for_ced_and_ours_is_not_a_cng`、`n03_our_cng_detector_and_spandsp_agree_on_a_cng_train` | 反向對照：SpanDSP 不把靜音、1100 Hz、1650 Hz 當 CED，也不把我方 CED 當 CNG，我方偵測器亦同；額外：同一串 CNG（−10／−20／−30 dBm0，4 個 burst）我方偵測 4 次、SpanDSP 的 CNG 偵測器也偵測得到（N01 的獨立對照，計畫沒有要求） | 通過（Docker） |
| N04 | `m5a_n.rs::n04_start_and_end_are_found_within_10_ms_of_the_truth_at_every_level_above_the_threshold` | 3 個 seed × 電平 −10／−20／−30／−40／−42.5 dBm0 × 頻率 1650／1850／2100／1100 Hz，起訖樣本隨機：恰兩個事件（`On`、`Off`），位置與真實起訖的差在 ±40 個樣本（5 ms，一個區塊）以內，遠小於要求的 ±10 ms（見 §4 ⑤） | 通過 |
| N04 | `m5a_n.rs::n04_the_events_are_returned_15_ms_and_10_ms_after_the_truth`、`n04_the_threshold_is_minus_43_dbm0_and_the_end_minus_48`、`n04_hysteresis_a_signal_that_fades_between_43_and_48_stays_present` | 事件實際回傳的時間（debounce）：`On` 約 15 ms 後、`Off` 約 10 ms 後；常數＝−43／−48；−44 dBm0 以下的 1 s 訊號不啟動、−42／−41 啟動；先 −38 再降到 −45（兩個門檻之間）時維持存在，降到 −50 才 `Off`（位置在降低處 ±10 ms）（M5e 起：從 −30 降到 −45 是降 15 dB，依 ADR-066 本身就是訊號結束，所以起始電平改為 −38）；一直停在 −45 的訊號不啟動 | 通過 |
| N04 | `m5a_n.rs::n04_clicks_up_to_5_ms_are_not_a_signal_but_15_ms_are`、`n04_several_bursts_a_noisy_line_and_chunked_feeding`、`n04_is_on_follows_the_events` | 5 ms（−10 dBm0）的突波在每種對齊下都不是訊號，20 ms 的是；3 個 seed：−60 dBm0 背景雜訊上 4 個隨機長度 burst，8 個事件的位置都在 ±80 個樣本以內，任意分塊（33、160、1000、8000）事件完全相同；`is_on` 與事件一致、恰好兩次邊緣 | 通過 |
| N（性質） | `m5a_n.rs::n_proptest_any_input_never_panics_the_detectors_and_events_are_consistent` | proptest（3 個 seed × 300 組）：任意樣本（亂數、定值長串、全刻度極值）、任意分塊餵 `CngDetector` 與 `PresenceDetector`：不 panic；事件 On／Off 交替、位置不減且在輸入範圍內、`is_on` 與最後事件一致 | 通過 |
| N（性質） | `m5a_n.rs::n_proptest_goertzel_and_level_functions_take_any_number`、`n_proptest_the_tone_generator_writes_exactly_its_length_for_any_frequency_and_level`、`n_proptest_a_loud_unpredictable_input_never_looks_like_cng` | proptest：`Goertzel`（任意 f32，含 NaN／無限大）、`dbm0`、`sine_amplitude`、`mean_square` 任意輸入不 panic；`ToneGenerator`（任意頻率與電平，0–400 ms／0–200 ms，任意分塊）寫出的樣本數恰為 `remaining()`，結束後 `fill` 回 0；10 分鐘的全範圍隨機樣本 CNG 誤判 0 次（3 個 seed） | 通過 |

## 3. 發現並修正的產品問題

以下都是測試發現的真實缺陷，做了最小修正，各有測試，並以還原修正後確認該測試會失敗。

1. **jitter buffer 見底之後永遠回不來**（J07 發現，`rtp/jitter.rs`）。`pull` 在 buffer 沒有任何資料時，仍依時鐘把播放位置往前推 160 個樣本（回報 gap）。送端時脈比我方慢（−200 ppm）時，起始的 3 包餘裕在約 150 s 後用完，之後每個封包都在它的時間之後才到，到達時位置已經超過它，被當成 `late` 丟棄，下一包又是如此：**之後每一塊都是 gap，永遠不恢復**（實測 10 分鐘：gap 2 399 840 個樣本、late 14 996 包）。任何一次超過起始深度的網路抖動都會造成同樣的永久劣化。修正：見底時（pull 開始時 buffer 內沒有任何資料）進入 `priming`，位置**不再前推**，等到再累積 `start_depth` 個樣本才繼續，從見底的位置（或之後第一個到達的封包）接著播；等待期間的靜音照樣回報為 gap。實際效果：−200 ppm 的 10 分鐘只補一次 480 個樣本（3 包）的靜音，來源樣本一個都沒有少；封包遺失超過 buffer 時的對齊與修正前相同（第一個到達的封包在原本的時間播出）。測試：`j07_a_buffer_that_ran_dry_waits_for_its_depth_and_plays_the_late_packet`、`j07_plus_and_minus_200_ppm_…`、`j07_a_much_faster_or_slower_sender_…`（修正前這三個都失敗）、`j07_with_jitter_on_top_of_the_drift`、`j02_with_a_depth_below_the_jitter_…`。
2. **CNG 偵測器在 −43 dBm0 完全偵測不到**（N01 發現，`fax-modem/src/tone.rs`）。`CngDetector` 以 `ON_DBM0`（−43）當區塊能量門檻，需要連續 17 個 20 ms 區塊都高於它。−43 dBm0 的音疊上任何雜訊（SNR 20 dB）後，區塊能量會隨機落到門檻下，連續 17 個不可能，5 個 burst 一個也偵測不到。−43 dBm0 是接收端必須處理的最低電平，計畫 N01 也明確要求。修正：區塊能量門檻改用 `OFF_DBM0`（−48，「訊號已結束」的電平，5 dB 餘裕），純度（≥ 70% 的能量在 1100 Hz）才是主要的判別條件。副作用：−43 到 −48 dBm0 之間的純 1100 Hz 也會偵測（T.30 對低於 −48 不要求）。測試：`n01_cng_is_heard_in_every_burst_…`（−43 dBm0 修正前 0／5）、`n01_below_the_threshold_nothing_is_heard`（−50 以下仍不偵測）。
3. **μ-law 編碼對負值不是奇對稱，與 SpanDSP 有 127 個輸入不同**（G02 對照發現，`rtp/g711.rs`）。A-law 的 65536 個輸入、兩種 law 的 256 個解碼值都與 SpanDSP 完全相同；μ-law 編碼有 127 個負值輸入（−31612、−30588、−29564…，每一個是一個量化區間的邊界）我方編出的碼比 SpanDSP 小 1（量級小一階）。分析：μ-law（G.711 Table 2a）是以正負號加量級定義的，−x 與 x 的碼只差正負號位元；SpanDSP 對負值取 `BIAS − x`（量級 |x|），正負對稱；我方取 `BIAS − x − 1`（1 補數，這是 A-law 表的做法，不適用 μ-law），使負半邊的決策邊界偏移一個樣本。兩者誤差都在半個量化步階之內（對聽感與數據機沒有影響），但我方違反 G.711 μ-law 的奇對稱，且與計畫「與 SpanDSP 相同」不符；Sun 的參考實作（先右移 2 位元再取負）在負半邊的偏移方向相反，所以 16 位元刻度下並不存在唯一的「標準答案」，這裡以奇對稱為準。修正：μ-law 負值改為 `BIAS − x`（A-law 不動，與 SpanDSP 本來就相同）。測試：`g02_encoding_all_65536_values_equals_spandsp_ulaw`（修正前 127 個不同）、`g02_full_scale_and_silence`（μ-law 奇對稱、A-law 1 補數）。

## 4. 與計畫不同之處

① **J01「延遲等於設定的起始深度」**：延遲以兩種方式量測並斷言。開始播放的那一刻 buffer 恰有 `start_depth` 個樣本（`buffered_at_start`＝深度）；由於一個 tick 先 push 再 pull，第一塊在 tick `depth/160−1` 播出，即輸出比輸入晚「起始深度減一包（20 ms）」。若 fax-in 先 pull 再 push，延遲才恰好等於起始深度，兩者只差一包。
② **J02 的深度**：±40 ms 的均勻抖動加上 5% 對調，到達時間的分布寬度可到 80–100 ms；要「輸出逐樣本相同」buffer 起始深度必須大於這個寬度。嚴格案例用 8 包（160 ms）。預設的 `JitterConfig::G711`（3 包＝60 ms）在這個抖動下不保證不遺失（我的 3 個 seed 實測沒有遺失，但不是保證）；因此另有 ±100 ms 配預設深度的案例，驗證遺失時不會錯位或憑空產生樣本。M5e 選 fax-in 的深度時要知道這件事。
③ **J03／J05 的「回報」形式**：計畫寫 `Gap(samples)`、`Resync`「事件」。實作是 `Pulled { playing, gap }`（每塊的 gap 樣本數）與 `JitterStats`（`gap_samples`、`resyncs` 計數器），沒有事件。測試以逐塊的 `gap` 與計數器斷言（resync 恰好一次＝`resyncs` 增加 1）。
④ **J05 播放開始前的 timestamp 跳躍**：計畫只講大跳重新同步。播放還沒開始時 `push` 不檢查跳躍（沒有播放位置可比），要到下一個 `push` 才重新同步，所以新串流的第一包會連同舊資料被丟掉，並有一塊（20 ms）靜音被當成 gap。測試如實斷言（新串流第二包起完整、`resyncs`＝1）。不修，因為只影響通話開頭的一包（見 §5）。
⑤ **N04「判定延遲在 ±10 ms 內」**：以事件**回報的位置**對真實起訖的誤差斷言（實測在 ±5 ms 之內，一個 5 ms 區塊；要求 ±10 ms）。偵測器是 debounce 的（高於 −43 dBm0 連續 15 ms 才報 `On`，低於 −48 dBm0 連續 10 ms 才報 `Off`），所以事件是在真實時間後約 15 ms／10 ms 才傳回（測試 `n04_the_events_are_returned_15_ms_and_10_ms_after_the_truth` 量測並斷言）。如果計畫的意思是「事件傳回的延遲」≤ 10 ms，則 `On` 的 15 ms 不滿足，請審核者決定；回報的位置（用於 T.38 `NoSignal` 對照）已符合。
⑥ **N02 的「語音樣本（錄音檔）」**：repo 內沒有錄音檔。以合成的「類語音」代替：110 Hz 附近、變動音高的脈衝列通過隨時間切換母音的三個共振峰濾波器、4 Hz 的音節調幅加呼吸雜訊（`speech_like`），電平 −10／−20／−30 dBm0 各 20 s × 3 個 seed，並驗證替身的電平與頻譜（1100 Hz 附近的能量占比 < 30%）。不等於真實語音；如果之後有授權的錄音檔，可以接進同一個 `assert_no_cng`。
⑦ **N01 的延遲下界**：計畫只要求「< 400 ms」。我加了下界檢查（≥ 300 ms，確認不是在 burst 開始時就誤報）；實測最早約 335 ms（偵測器要連續 17 個 20 ms 區塊，第一個區塊可以只含部分的音）。
⑧ **G02 的「與 G.711 表相同」**：G.711 的表只給 13／14 位元量級，沒有 16 位元負值的取捨規則（見 §3 ③）。G02 以區間包含性（半個步階＋1 的誤差）、單調、idempotent、與 SpanDSP 逐一相同四種方式驗證，不是逐值對照一張 16 位元的「標準」表。
⑨ **G01 的 SpanDSP FFI**：`alaw_to_linear` 等是 inline，沒有匯出；改用匯出的 `g711_encode／decode`（見 §1），行為等價。
⑩ **額外案例**（計畫沒有）：N03 的反向對照與 CNG 對照、J07 的 ±2000／±10 000 ppm、J02 的預設深度、N01 的低 SNR。

**未實作（延到 M5e）**：G05（SDP 協商）、J06（送端 20 ms pacing）。
**不在這次範圍**：計畫 §1 的 fuzz 目標 `rtp_decode`、`hdlc_deframe`、`audio_rx`，以及 criterion 效能量測。

## 5. 觀察到、沒有修的問題（供審核者決定）

- `ToneGenerator::new` 以 `on_ms * 8000` 的 `u32` 乘法計算樣本數，`on_ms` 超過約 537 000（9 分鐘）時在 debug 溢位會 panic、release 會繞回。呼叫端都是常數（CED 3000 ms），proptest 因此把長度限制在 400 ms 以內。
- `RtpPacket::encode` 把 extension 的 word 數寫成 `u16`，資料超過 262 140 byte 時長度欄位被截斷，產生損毀的封包。真實封包最大約 1500 byte，不會發生。
- 播放開始前的 timestamp 跳躍會丟掉新串流的第一包（§4 ④）。
- `pull` 以「最後一個 key ≤ 位置」找覆蓋該位置的封包；從程式碼看，若 buffer 內有時間範圍互相重疊且長度不同的封包（通話中途改 ptime），較長的那一包可能被忽略而播出靜音。**只讀程式碼推論，沒有測試**；G.711 傳真通話的 ptime 固定 20 ms，不太可能發生。
- 以一個 `Pull(0)`（輸出長度 0）開始播放時，buffer 可能暫時超過 `max_depth`，要到下一個被接受的封包才會修剪。無害。
- `CngDetector` 的頻率容許度（T.30 的 1100 ± 38 Hz）沒有測；50 Hz 一格的 Goertzel 在 ±25 Hz 以外靈敏度會明顯下降。
- 過量（快的送端）時的處理是一次丟到起始深度（最多 `max_depth − start_depth`，預設 1920 個樣本＝240 ms），不是計畫 J07 寫的「緩慢丟棄或補一個樣本」；這是 `jitter.rs` 的設計（避免在訊號中間丟單一樣本），在 ±200 ppm 的 10 分鐘內 buffer 最多到 1280／2400 個樣本，不會觸發。

## 6. 涵蓋率

| 項目 | 數字 | 門檻 |
|---|---|---|
| `fax-modem`（行） | **98.64%**（221 行中未涵蓋 3）；`goertzel.rs` 100%、`level.rs` 100%、`tone.rs` 97.96% | ≥ 85% |
| `sip-core` 全部（行） | **93.76%**；`rtp/g711.rs` 91.43%、`rtp/jitter.rs` 99.54%、`rtp/packet.rs` 100% | ≥ 85% |

涵蓋率由 `cargo llvm-cov` 在 Windows 本機量測（含單元測試與整合測試）。

## 7. 執行指令

```sh
cargo test -p sip-core --test m5a_g --test m5a_j          # G01–G04、J01–J05、J07
cargo test -p fax-modem                                    # N01–N04（m5a_n.rs，約 5 秒）
E2E_ONLY="--test g01_g02_g711_spandsp --test n03_ced_spandsp" scripts/e2e-docker.sh   # G01／G02／N03 的 SpanDSP 對照（含 tools/ 的 fmt、clippy、t4 oracle）
cargo llvm-cov -p fax-modem --fail-under-lines 85 --summary-only
cargo llvm-cov -p sip-core --fail-under-lines 85 --summary-only
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

## 8. 本機驗證結果

| 指令 | 結果 |
|---|---|
| `cargo test -p sip-core -p fax-modem` | 全部通過（`sip-core` 單元 50、`m5a_g` 24、`m5a_j` 26、其餘整合測試照舊；`fax-modem` 單元 5、`m5a_n` 28） |
| `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all --check` | 通過 |
| `E2E_ONLY="--test g01_g02_g711_spandsp --test n03_ced_spandsp" scripts/e2e-docker.sh` | 通過（`tools/` 的 fmt、clippy、g01／g02 6 個、n03 5 個、t4 oracle 都通過） |
| `cargo llvm-cov` | 見 §6 |
| 變異驗證（改壞後測試要失敗，還原後通過） | 7 項全部被偵測：jitter 的 late 判斷 `<=`→`<`、gap 時位置少推 1、padding 長度少減 1、A-law 解碼常數 0x108→0x100、`CNG_BLOCKS` 17→10、`OFF_BLOCKS` 2→30、CED 相位步階 ×1.01 |
| 修正前的失敗確認 | 把 `jitter.rs` 還原成修正前：J07 三個案例失敗（見 §3 ①）；CNG 的 −43 dBm0 與 μ-law 的 127 個差異也都是先失敗才修正 |

**沒有執行**：`scripts/ci-local.sh` 與完整 `scripts/e2e-docker.sh`（依指示）、musl 建置、fuzz。Windows 上標 `#[cfg(unix)]` 的測試不執行（這次沒有新增這類測試）。

## 9. Opus 審核（2026-10-26）

**結論：通過。**

- **斷言品質**：逐檔檢查每個測試的斷言。斷言數為 0 的 J03、J07 案例都把檢查放在共用函式（`j03_case`、`check_drift`），內容是逐塊比對與樣本守恆，不是空斷言。G 系列以獨立寫成的 G.711 表公式與 SpanDSP 兩個來源對照；N 系列的訊號由測試自己以 `f64` 產生，不用被測的產生器。報告 §8 的 7 項變異驗證全部被抓到，可信。
- **過度 mock**：沒有。全部是純計算，J 系列的虛擬時鐘與 fax-in 實際用法（M5e 的 `audiosession` 每 20 ms 先 push 再 pull）一致。
- **三個產品修正**：都正確且必要，採納。jitter buffer 見底後永久 gap 是嚴重問題（任一次大抖動都會讓整通劣化），修正後的行為也被 M5e 的 E04（±30 ms 抖動）間接驗證。μ-law 以奇對稱為準、與 SpanDSP 相同，合理。
- **§4 與計畫的差異**：全部接受。① J01 的延遲差一包是 push／pull 順序造成，報告兩種量法都有斷言。② 預設深度 60 ms 在 ±40 ms 抖動下不保證無損：已在 M5e 以 E04（±30 ms 全部收下）確認預設值夠用，不改。⑤ N04 以回報的位置衡量（±5 ms）符合計畫本意：音訊引擎與 T.38 對照用的是訊號起訖的位置，不是事件回傳的時刻；debounce 的 15 ms／10 ms 另有測試鎖住。⑥ 語音以合成替身代替，接受，有授權錄音檔時再補。
- **§2 N04 的遲滯案例**：M5e 的 ADR-066（相對下降 10 dB 即為訊號結束）讓原本 −30→−45 的案例變成兩段訊號，測試已在 M5e 改為 −38 起始；本次把 §2 的描述同步更新。
- **§5 觀察到的問題**：`ToneGenerator::new` 的樣本數乘法改為飽和（`remaining` 也是），`RtpPacket::encode` 的 extension 超過 65 535 個 word 時截斷資料而不是寫出長度不符的封包；兩者都在審核時修正並附單元測試。其餘（播放前 timestamp 跳躍丟一包、ptime 中途改變的重疊、`Pull(0)` 暫時超過 `max_depth`、CNG ±38 Hz 容許度、過量時一次丟到起始深度）影響小或不會發生，維持現狀。
- **漏測**：計畫 §1 的 fuzz 目標 `rtp_decode`、`audio_rx`（`hdlc_deframe` 在 M5b）與 criterion 效能量測沒有做；fuzz 目標由 Opus 在審核時補上（`crates/sip-core/fuzz`、`crates/fax-in/fuzz`，納入 nightly fuzz workflow），效能量測列為 M6 待辦。G05、J06 已在 M5e 完成。
- **涵蓋率**：fax-modem 98.64%（≥ 85%）、sip-core 93.76%（≥ 85%）；CI 的 coverage job 持續把關。
