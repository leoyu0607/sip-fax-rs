# M5e 測試報告：G.711 整合（RTP pacing、fax-sim 經 RTP 打真的 `sip-faxd serve`）

- 實作：Opus（`crates/fax-in/src/{audiosession,audioengine,controller}.rs`，ADR-066、ADR-067）；測試與本報告：Sonnet（依 Opus 的測試計畫）；§3 的產品修正由測試者做最小修正
- 計畫：`docs/test-plans/M5.md` §1、§3（J06）、§7（M04，M5d 留下的項目）、§8（E01–E08）；ADR-061、ADR-066、ADR-067
- 分支：`m5`
- 範圍：J06、E01–E08、M04（固定 SNR）。**不在範圍**：M5b 測試（H、V 系列）。
- 狀態：（待審核）

## 1. 環境

| 項目 | 內容 |
|---|---|
| 主機 | Windows 11 Pro（Git Bash），Docker Desktop；Rust 1.99.1（`rust-toolchain.toml`） |
| J06 | `crates/fax-in/tests/j06_rtp_pacing.rs`（原生執行，不需 Docker）。tokio 暫停時鐘（`start_paused`），真的 localhost UDP socket：對方是一個不回話的 UDP socket，session 只聽、送靜音，所以量到的是 session 自己的 20 ms tick |
| E01–E08 | `tools/fax-sim/tests/e2e_m5e.rs`（14 個測試，其中 1 個 `#[ignore]`）。真的 `sip-faxd serve`（debug 建置，SQLite，`[capture] enabled`），fax-sim 的 SIP UAC 加 SpanDSP `fax_state`（呼叫端、送 TIFF）經真的 UDP socket 與牆上時鐘送 RTP（`fax_sim::audio_gateway`），每通 25–60 秒。每頁以 libtiff（`tiffcmp -t`、`tiffcp -c none` 逐 byte）與 poppler（`pdftoppm -mono`）各比一次，都有負向對照（`e2e_common::verify_files`，沿用 M2d） |
| 執行環境 | Rocky 9 映像 `sip-fax-rs-test:rocky9`（SpanDSP v3.1.1），`scripts/e2e-docker.sh`（`E2E_NO_PG=1` 不起 PostgreSQL）。伺服器是 debug 建置，所以 E07 的 CPU 數字偏高（見 §2） |
| 通道（E04） | `FaxTx::loss_permille`：兩個方向各以固定 seed 的 PRNG 丟封包；`AudioNet::jitter_ms`：送出的每個封包被隨機扣住 0..=N ms（可能超車）。**計畫的「±30 ms」用 N = 60 實現**（平均值 ±30 ms）；與遺失併用的案例用 N = 30 |
| fax-sim 工具 | `sip_uac::ReinviteBy::AudioThenT38`（E08 用，§3 工具）；`UacReport::audio_run`（T.38 之前的音訊階段） |
| 資料庫 | 測試程式從第二個連線直接讀伺服器的 SQLite（`store::connect_with`，沿用 `e2e_m3b`） |
| 隨機性 | E04 seed `0x5E04_0001/2/3`，E07 seed `0x5E07_000i`；M04 `0x5D04_2000 + snr*16 + seed`；E04c `0xE04_0000 + seed`、`0xE04_8000 + seed`。全部固定 |
| 涵蓋率 | `cargo llvm-cov clean --workspace` 後再量（見 §6） |

## 2. 案例對照表

| ID | 測試位置 | 驗證內容 | 結果 |
|---|---|---|---|
| J06 | `fax-in/tests/j06_rtp_pacing.rs::j06_one_packet_every_20_ms_with_plus_160_and_plus_one` | PCMU 與 PCMA 各 50 個封包：相鄰封包恰 20 ms（虛擬時鐘），seq +1，timestamp +160，SSRC 不變，payload type 正確，160 byte（20 ms），聽的時候送該 law 的靜音 | 通過 |
| J06 | `…::j06_a_late_task_catches_up_without_dropping_packets` | 時鐘一次前進 200 ms（task「卡住」）：**補送 10 個封包，seq 與 timestamp 連續無缺口**，之後恢復 20 ms 一包，整段 220 ms 共 12 包（延遲沒有累積）。變異對照：`MissedTickBehavior::Burst` 改成 `Skip` 時只送 1 包，測試失敗；timestamp 步階改成 161 時兩個測試都失敗 | 通過 |
| E01 | `e2e_m5e.rs::e01_audio_only_pcmu_three_pages` | `t38_policy = "audio-only"`，PCMU，3 頁 fine（260／300／340 列）：SpanDSP 結果 0，TIFF／PDF 逐像素相同（libtiff＋poppler），`result.json` 的 `audio.reason` = `t38_policy = audio-only`、`codec` = PCMU，`t38` 為 null；DB 列 `received`、`pages` 3、`modem` v29、`media` g711、`bit_rate` 9600、`hangup_code` completed、`t30_code` 0、`remote_ident` 0000000001、`ecm` false。通話 36.9 秒；gateway 送 1891 包、收 1843 包，伺服器收 1841、送 1843 | 通過 |
| E02 | `…::e02_t38_refused_with_488_then_g711` | `auto`、`initiate_after_secs = 0.3`：我方的 T.38 re-INVITE 被回 488（`t38_refused` = 1），3 頁經 G.711 收下，逐像素相同；`audio.reason` = 「peer answered 488 to the T.38 re-INVITE」；`result.json`、伺服器 log 都沒有 `g711_passthrough_unsupported`；DB `hangup_code` completed（不是 `t38_rejected`）、`media` g711。43.6 秒 | 通過 |
| E03 | `…::e03_pcma` | gateway 只提供並送 PCMA：`audio.codec` PCMA、`payload_type` 8，200 的 SDP 第一個格式是 8，3 頁逐像素相同，V.29 9600。36.9 秒 | 通過 |
| E04 | `…::e04_jitter_30_ms_three_seeds` | 送出的每個封包被扣 0..60 ms（±30 ms，會亂序），3 seed，無遺失：全部收下，bad_lines 全 0，逐像素相同；**伺服器 jitter buffer 的 `late` = 0、`lost_ms` = 0、`duplicate` = 0**。每通約 51 秒 | 通過 |
| E04 | `…::e04_loss_half_percent_with_jitter_three_seeds_recorded` | 0.5 % 遺失（兩個方向）＋ 0..30 ms 抖動，3 seed：**seed 1 收下**（17＋15 包被丟，伺服器 `lost_ms` 340，bad_lines [0, 8, 8]，限制是每頁列數的 10 %＝50 列）、**seed 2 失敗**（5＋4 包，`t2_expired_image`，0 頁，18 秒；SpanDSP 逾時）、**seed 3 收下**（9＋13 包，bad_lines [7, 8, 0]）。斷言：每通都乾淨結束（收下，或以 T.30 結果失敗，不掛住）、遺失確實被注入、收下的符合 bad_lines 門檻且檔案完整（有 bad_lines 的不比像素，只驗頁數）、至少 1 通收下 | 通過（只記錄；計畫要求未達成，見下一列與 §4 ①） |
| E04 | `…::e04_plan_loss_half_percent_with_jitter_all_three_seeds_unmet`（`#[ignore]`） | 計畫的字面做法：同上 3 seed 都要收下。目前 seed 2 失敗 | **未達成**（`#[ignore]`） |
| E04c | `m5d_audio_engine_spandsp.rs::e04c_random_loss_half_percent_ours_is_no_worse_than_spandsp` | 進程內、兩頁 fine、V.29、0.5 % 隨機遺失（兩個方向），40 個 seed：**我方收端失敗 7／40**（seed 6、13、16、22 為 `t2_expired_command`，26、31、38 為 `t2_expired_image`）；**對照：SpanDSP 對 SpanDSP 在同樣的遺失下失敗 17／40**。收下的 33 通全部符合 bad_lines 門檻。斷言：每通都結束、我方失敗數 ≤ 對照 | 通過（見 §4 ①） |
| E04 | `m5d_audio_engine_spandsp.rs::m05b_one_lost_block_at_any_offset_after_rx_image_is_survived` | 進入 RxImage 之後的 0–1200 ms，每 20 ms 一個位置各丟一個 20 ms 區塊（61 種，涵蓋頁面的 training 與前面的影像），引擎被告知遺失：**61 通全部收下**，SpanDSP 結果 0，2 頁，bad_lines 在門檻內，沒受影響的第 2 頁逐像素相同 | 通過 |
| E05 | `…::e05_receiving_with_climbing_pages_over_g711` | 3 頁 fine（600 列）：每 10 ms 讀 DB，看到 `(Receiving,0)`、`(Receiving,1)`、`(Receiving,2)`、`(Receiving,3)`、`(Received,3)`，頁數只升不降，至少兩個中間值，最後是 `received`／3；通話 56.9 秒；`media` g711、逐像素相同。Web `fax.progress` 見 §4 ② | 通過（Web 部分見 §4 ②） |
| E06 | `…::e06_audio_log_content` | `keep = "all"`：`audio.log` 與 `sip.log`、`result.json` 同目錄；每一行都是「RFC 3339 時間＋`<ms> ms`＋內容」，毫秒偏移只增不減；依序出現：`answering the fax`、`V.21 tx CSI`、`V.21 tx DIS`、`V.21 rx TSI`、`V.21 rx DCS`、`V.29 9600 receiver ready`、`V.29: trained`、`TCF: 1837 octets (need 1440), 14436 of 14696 bits zero`、`V.21 tx CFR`、`trained`、`V.21 rx MPS`、`V.21 tx MCF`、`trained`、`V.21 rx EOP`、`V.21 tx MCF`、`T.30 done: ok after 2 page(s)`；另有 `first RTP packet: PCMU`。**`V.21` 的收送行是 §3 的修正加的** | 通過（§3） |
| E06 | `…::e06_keep_failed_drops_good_calls_and_keeps_failed_ones` | `keep = "failed"`：收下的傳真（1 頁）沒有擷取目錄；另一通 6 頁的在 12 秒時由 gateway 掛斷（`outcome` failed，`T.30 aborted (11) after 0 page(s)`）：保留 `sip.log`、`audio.log`（有 `answering the fax`）、`result.json` | 通過 |
| E06 | `…::e06_max_bytes_per_call_bounds_audio_log` | `max_bytes_per_call = 1500`：`audio.log` 的行（不含註記）≤ 1500 byte（與 `sip.log` 各自的額度），最後一行 `[capture] 7 further line(s) not stored (max_bytes_per_call reached)` | 通過 |
| E07 | `…::e07_ten_concurrent_g711_calls` | 同時 10 通（PCMU 與 PCMA 交錯，1–3 頁，standard 與 fine 交錯，不同 TSI 與影像）：10 通全部收下（共 19 頁），每通 V.29 9600、逐像素相同、TSI 與自己的對得上、TIFF 路徑互不相同（沒有混到別通），伺服器 log 無 `ERROR`。**牆上時間 56.2 秒（最長一通約 56 秒），通話時間合計 344.0 秒，伺服器 CPU 時間 8.06 秒（user＋system，`/proc/<pid>/stat`，debug 建置）＝每通話秒約 23 ms（約 2.3 % 個核心）**。計畫 §1 的效能目標是 release 的收端每秒音訊 < 50 ms；debug 建置已是 23 ms，且含 SIP、DB、檔案、log。release 數字沒有量（criterion 是 M5a 的項目） | 通過 |
| E08 | `…::e08_cng_on_audio_then_our_t38_offer_is_refused` | gateway 在 audio 上送 CNG（SpanDSP 呼叫端的 CNG 不回應地送），不送 re-INVITE；`auto`、`initiate_after_secs = 1.5`：我方在 1.5 秒發 T.38 re-INVITE（`t38_refused` = 1），回 488 後改走 G.711，2 頁收下，逐像素相同。`audio.log`：`first RTP packet` 21 ms、**`CNG heard` 403 ms（在我方 1504 ms 的 `answering the fax` 之前）** | 通過 |
| E08 | `…::e08_cng_on_audio_then_our_t38_offer_is_accepted` | 同上，但 gateway 接受 T.38 re-INVITE：音訊階段送了 75 包（1507 ms），之後 T.38 收下 2 頁（`t38.path` = `our_reinvite`、`media` t38、14400、逐像素相同，SpanDSP 結果 0）；`audio.log` 有 `CNG heard`（402 ms），沒有 `answering the fax`（音訊 session 只聽、從來沒回話），`audio.reason` 為空 | 通過 |
| M04 | `m5d_audio_engine_spandsp.rs::m04_constant_snr_11_db_fails_the_9600_tcf_with_ftt_and_the_call_succeeds_lower`（M5b 審核後改為 `m04_constant_snr_10_5_db_…`，ADR-069） | **計畫的字面做法現在成立**（ADR-066 之後）：SpanDSP 只開 V.29，整通固定 SNR 11 dB，3 seed：第一個 TCF（9600）零位元比例 94.9／94.6／94.8 %（T.30 要 95 %）→ **FTT**；SpanDSP 降速：seed 0 降到 **7200** 收下（bad_lines [13, 26]），seed 1、2 在 7200 的 TCF 也被拒（4 個 TCF）後降到 **V.27ter 2400** 收下（bad_lines [21, 0]、[20, 0]）；都在 10 % 門檻內，SpanDSP 結果 0 | 通過（降到 2400 偏離計畫的「7200 或 4800」，見 §4 ③） |
| M04 | `…::m04_constant_snr_14_db_page_rejected_with_rtn_then_7200_exact` | 14 dB：9600 的 TCF 通過（98 % 為 0），但第 1 頁 32 列壞行 → **RTN**；SpanDSP 重新 training 到 7200，TCF 好，兩頁**逐像素相同**（3 seed） | 通過 |
| M04 | `…::m04_constant_snr_12_db_pages_within_the_bad_line_threshold`、`m04_hopeless_noise_ends_the_call` | 12 dB（TCF 通過，RTN 後 7200，bad_lines [9,7]／[2,2]／[2,5]，3 seed）；8 dB：無法收下，但三個 seed 都在 23–28 秒內結束（`RemoteDisconnect`／`T2ExpiredCommand`，SpanDSP 結果 20／14），不掛住 | 通過 |

## 3. 發現並修正的產品問題

**audio.log 沒有記錄 V.21 frame**（E06 發現，`crates/fax-in/src/audioengine.rs`）。計畫 E06 要求 `audio.log`「每個 V.21 frame、數據機切換、訊號事件與時間」；實作只有數據機切換、TCF 結果、壞 FCS 之類的行，T.30 的控制訊框（TSI、DCS、CFR、MPS、MCF、EOP、DCN、我方的 CSI、DIS）完全沒有，診斷時看不到交握過程（T.38 的 `udptl.log` 有）。最小修正（約 20 行）：新增 `frame_line()`，收到 FCS 正確的 V.21 frame 記一行 `V.21 rx <名稱> <每個 octet 的 hex>`，每次送出的 burst 的每個 frame 記一行 `V.21 tx …`（名稱用 `t30::frame::fcf_name`，octet 是 T.30 記法：位址、控制、FCF、FIF）。FCS 錯誤的 frame 本來就有一行（`frame with bad FCS`）。單元測試：`answers_with_ced_then_csi_and_dis_on_v21` 斷言 `V.21 tx DIS FF C8 …`，`a_dcs_from_the_peer_is_heard_after_our_burst` 斷言 `V.21 rx TSI`、`V.21 rx DCS FF C8 41 00 46 00`（還原就失敗）；E06 的端到端測試檢查整通的順序。依 CLAUDE.md，這是設計層的小變更，建議的 ADR 文字：

> **ADR-068：`audio.log` 記錄每個 T.30 frame（2026-10-26，M5e 測試）**
> 決定：音訊引擎把每個收到（FCS 正確）與送出的 V.21 T.30 frame 記成一行（`V.21 rx|tx <名稱> <hex>`），與數據機切換、TCF 結果、T.30 結束的行一起進 `audio.log`。理由：計畫 E06 要求；G.711 的現場問題只有 `audio.log` 能還原交握，且 T.38 路徑的 `udptl.log` 已有同等資訊。取捨：每通約多 30 行；FCS 錯誤的 frame 仍只記長度（在 V.29 資料期間，V.21 收端會從資料訊號解出雜訊，每頁約 2–5 行，見 §5 ③）。

工具（不是產品）：`tools/fax-sim/src/sip_uac.rs` 新增 `ReinviteBy::AudioThenT38`（audio 上 SpanDSP 送 CNG，等伺服器的 T.38 re-INVITE，接受後停掉音訊 gateway，在同一個 UDP socket 上以新的 T.38 終端機送整份傳真；沒有 re-INVITE 時音訊傳真照常跑完）與 `UacReport::audio_run`（音訊階段的計數）；`finish_call` 把 BYE 的收尾從 `call()` 拆出來。只改 `tools/`。

沒有其他產品問題：E01–E08 在第一次實際執行就通過（E05、E06 是測試的時間與順序假設要修，不是產品）。

## 4. 與計畫不同之處、未達成的項目

① **E04「遺失 0.5 % 仍收下」未達成（沒有修，需要設計決定）**。3 個 seed（遺失 0.5 %、抖動 0..30 ms）中 seed 2 失敗：`t2_expired_image`，0 頁，18 秒；其餘兩個收下，bad_lines [0, 8, 8] 與 [7, 8, 0]。不是抖動造成的：抖動 0..60 ms 沒有遺失時 3 個 seed 都 0 錯，伺服器 jitter buffer 沒有任何封包算作 late。遺失造成的失敗：頁面 training（或 V.21 frame）被丟掉一個 20 ms 區塊，收端沒有進入資料狀態，T.30 的 T2（6 秒）逾時，`peer signal end not seen within 500 ms: answering anyway` 後送 DCN，SpanDSP 放棄（結果碼 20／21／38）。我自己做的量測：
  * 進程內（`e04c_…`）40 個 seed、兩個方向各 0.5 % 隨機遺失：我方失敗 **7／40**（17.5 %），**SpanDSP 對 SpanDSP 在同樣的遺失下失敗 17／40（42.5 %）**。所以音訊通道上 0.5 % 遺失本來就會讓一部分的呼叫失敗（V.21 frame 與 training 沒有錯誤更正，只靠對方重送，雙方都只重試幾次），我方比 SpanDSP 自己的收端穩。
  * 單一區塊的遺失在影像的任何位置都能收下（`m05b_…`，61 個位置，0 失敗）；失敗發生在多個遺失碰上握手的時候（seed 6 的 TCF training 前後在 2.5 秒內有 5 個被丟的區塊）。
  * 兩者的失敗樣態：T2ExpiredCommand（TCF 前後的握手，4 個 seed）與 T2ExpiredImage（頁面 training 被丟後收端沒進入資料狀態，3 個 seed）。後者是可以改善的：training 失敗時收端仍看得到影像訊號的能量，可以等訊號結束再回 RTN／PIN 讓對方重送，而不是等 T2 到了就 DCN。這是設計選擇，沒有做。
  
  * 端到端每次執行的結果不完全相同（遺失的圖樣由 seed 固定，但 gateway 跑在牆上時鐘，被丟的是哪個封包會差一點）：遺失 0.5 %、抖動 0 ms 的第一次探索是 seed 1 收下（bad_lines [4, 6, 6]）、seed 2 失敗（`t2_expired_image`）、seed 3 收下（[7, 8, 0]）；遺失 0.5 %、抖動 0..30 ms 的第一次探索是 seed 1 失敗（`t2_expired_command`，1 頁，SpanDSP 結果 38）、seed 2 失敗、seed 3 收下；之後兩次正式執行都是 seed 1 收下、seed 2 失敗、seed 3 收下。seed 2 每次都失敗。
  
  計畫的字面斷言留在 `e04_plan_loss_half_percent_with_jitter_all_three_seeds_unmet`（`#[ignore]`，目前會失敗）；`e04_loss_half_percent_with_jitter_three_seeds_recorded` 斷言實際成立的事。建議：把計畫的 E04 改成「抖動全收；遺失下收下者符合門檻、不掛住、不比 SpanDSP 的收端差」，或決定要不要做上面的 RTN 改善。

② **E05 的 Web `fax.progress` 沒有端到端觀察到**。tools 沒有 WebSocket 客戶端與登入的流程（`e2e_m4*` 也沒有；M4 的事件測試在 `crates/web/tests/events.rs` 用假的 recorder）。我驗證的是它的來源：`apps/sip-faxd/src/web_service.rs` 把 recorder 的 `RecordWritten::Pages` 轉成 `fax.progress`，而 recorder 寫的就是 DB 的 `pages`——E05 看到 DB 的 `receiving` 頁數逐頁上升（0、1、2、3，然後 `received`），G.711 與 T.38 走同一個 `ctx.progress` 通道（`report_progress`）。所以只缺 WebSocket 這一段。要補的話需要在 `tools` 加 `tokio-tungstenite` 與登入。

③ **M04 的降速最終速率**：計畫「降到 7200 或 4800」。V.29 只有 9600／7200；SpanDSP 在 11 dB 的 3 個 seed 中有 2 個連 7200 的 TCF 也被拒（TCF 零位元比例 94.4／94.8 %，邊界），再降到 V.27ter 2400（不是 4800）。FTT 的條件只在 11 dB 附近很窄：12–14 dB 時 9600 的 TCF 通過（96.6 % 以上為 0），降速是第 1 頁被 RTN 之後（兩個都有測）。完整掃描（SNR 8–20 dB，各 3 seed）的數字寫在測試的註解裡。

④ **E08 的「V.21」沒有在 re-INVITE 之前出現**。SpanDSP 的呼叫端在收到 DIS 之前只送 CNG（從來不先送 V.21），所以「re-INVITE 前聽到 CNG／V.21」中的 V.21 這一段沒有端到端測。V.21 在 CED 期間被聽到的情形是 M08 的範圍（進程內，已知引擎送任何訊號時不聽，M5d §4 ⑤）。另外，`AudioThenT38` 在接受 re-INVITE 後以**全新的** T.38 終端機從頭送，不是接續音訊階段 SpanDSP 的狀態——對這個案例等價（音訊階段 SpanDSP 還在送 CNG、沒有進入 DCS）。

⑤ **E04 的「抖動 ±30 ms」** 用「每個封包被扣 0..60 ms」實現（平均值 ±30 ms，相鄰封包最多差 60 ms 而亂序）；與遺失併用的案例用 0..30 ms。

⑥ **E07 的 CPU 時間是 debug 建置的伺服器**（`faxd_binary()` 是 `cargo build` 的 debug）；release 沒量。

⑦ **其他小偏離**：E05 的頁高從 1500 列降到 600 列（3 頁 fine 在 9600 約 57 秒；1500 列超過觀察者原本的 110 秒上限，觀察者改為 170 秒）；E06 多測 `max_bytes_per_call`；M04 的舊測試 `m04_constant_noise_that_fails_9600_is_recorded` 刪除，換成上表的四個；新增 `m05b_…`、`e04c_…`（進程內，在 `m5d_audio_engine_spandsp.rs`）。

## 5. 觀察到、沒有修的問題（供審核者決定）

1. §4 ①（遺失下的失敗，特別是頁面 training 被丟之後等 T2）。
2. `audio.log` 的 `answering the fax (G.711)` 這一行不帶原因（session 建立時 `AudioNegotiated.reason` 還是 `None`，之後才由 controller 設）；原因只在 `result.json` 的 `audio.reason`。想在 `audio.log` 看到 488／`audio-only`，要讓 `go` 一併傳原因。
3. V.29 資料期間 V.21 收端會從資料訊號解出雜訊，`audio.log` 每頁多 2–5 行 `frame with bad FCS (N octets) ignored`（M5c §5 已預告；FCS 擋下，沒有影響）。
4. gateway 端的 `rx_gaps`（約 47）是伺服器掛斷後 gateway 繼續聽的 linger 期間的空白，不是封包遺失（E01 的 gateway 送 1891、收 1843，伺服器收 1841、送 1843，差額是掛斷前後的 linger）。
5. `e04_loss_half_percent_with_jitter_three_seeds_recorded` 要求「至少 1 通收下」，seed 3 在目前所有的執行都收下，但端到端的遺失落點每次不同，理論上有偶發失敗的可能（要改成更穩，可以降成「每通都乾淨結束」）。
6. 伺服器在負載下（E04 三通並行）第一個 RTP 封包比 session 開始晚約 0.5 秒，不影響結果。
7. M5b 的測試（H、V 系列）仍未做；`J06` 與 `E`／`M04` 已在這份報告。

## 6. 涵蓋率

| 項目 | 數字 | 門檻 |
|---|---|---|
| `fax-in`（行）`cargo llvm-cov clean --workspace` 後 `cargo llvm-cov -p fax-in --summary-only` | **93.78%**（`audioengine.rs` 95.27%，`audiosession.rs` 87.04%） | ≥ 70% |

SpanDSP 與 E 的測試（`tools/`）不計入涵蓋率；`audiosession.rs` 的涵蓋來自 `m5e_audio.rs`、J06 與單元測試。`fax-modem`、`sip-core` 這次沒有改，沒有重量。

## 7. 執行指令

```sh
# J06（原生）與 fax-in 的所有測試
T4_ORACLE=skip cargo test -p fax-in --test j06_rtp_pacing
T4_ORACLE=skip cargo test -p fax-in
# E01–E08（Docker，每個測試各自起一個 sip-faxd；--test-threads=1 讓輸出不交錯）
E2E_ONLY="--no-fail-fast --test e2e_m5e" E2E_SKIP_CHECKS=1 E2E_NO_PG=1 scripts/e2e-docker.sh --test-threads=1
# 單一案例，例如 E04 或 E07
E2E_ONLY="--no-fail-fast --test e2e_m5e" E2E_SKIP_CHECKS=1 E2E_NO_PG=1 scripts/e2e-docker.sh e04 --test-threads=1
# M04、M05b、E04c（進程內 SpanDSP）
E2E_ONLY="--no-fail-fast --test m5d_audio_engine_spandsp" E2E_SKIP_CHECKS=1 E2E_NO_PG=1 scripts/e2e-docker.sh m04 m05b e04c --test-threads=1
# 計畫字面的 E04（會失敗）
E2E_ONLY="--no-fail-fast --test e2e_m5e" E2E_SKIP_CHECKS=1 E2E_NO_PG=1 scripts/e2e-docker.sh --test-threads=1 -- --ignored
# 含 tools/ 的 fmt、clippy
E2E_ONLY="--no-fail-fast --test e2e_m5e --test m5d_audio_engine_spandsp" E2E_NO_PG=1 scripts/e2e-docker.sh --test-threads=1
cargo llvm-cov clean --workspace && T4_ORACLE=skip cargo llvm-cov -p fax-in --summary-only
```

## 8. 本機驗證結果

| 指令 | 結果 |
|---|---|
| `cargo fmt --all`（根）與 `cd tools && cargo fmt --all` | 無差異 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 無問題 |
| `T4_ORACLE=skip cargo test -p fax-in` | 全部通過（lib 29 個、`j06_rtp_pacing` 2 個、`m5e_audio` 3 個等） |
| `E2E_ONLY="--no-fail-fast --test e2e_m5e --test m5d_audio_engine_spandsp" E2E_NO_PG=1 scripts/e2e-docker.sh --test-threads=1`（不含 `E2E_SKIP_CHECKS`，tools 的 `cargo fmt --check`、`cargo clippy -D warnings`、t4 oracle 都在內） | exit 0：`e2e_m5e` 13 通過＋1 ignored（787 秒）、`m5d_audio_engine_spandsp` 19 通過（14.4 秒）、t4 oracle 7＋3 通過 |
| `m01_audio_engine_spandsp` | 2 通過（V.21 frame 記錄沒有影響其他測試） |
| J06 變異對照 | `Burst`→`Skip`：catch-up 測試失敗（1 包，要 10 包）；timestamp 步階 +161：兩個測試都失敗 |
| 修正前的失敗確認（§3） | 沒有實際還原；新增的 `V.21 rx／tx` 斷言比對的是只有 `frame_line()` 會產生的行 |
| `scripts/ci-local.sh` | **沒有跑**（依指示；這台機器記憶體不夠）。請在 push 前於別台機器或空閒時跑 |

## 9. Opus 審核（2026-10-26）

**結論：通過。** E04 的計畫門檻改由 ADR-068 定案（見下），其餘案例照計畫達成。

- **斷言品質**：抽查 J06、E01–E08、M04、M05b、E04c。沒有空斷言或只印不驗的案例：影像一律以 libtiff 與 poppler 逐像素比對，DB 列、`result.json`、`audio.log` 都有逐欄斷言；J06 附變異對照（`Skip`、+161 都會被抓到）。E04 的「記錄」測試仍有實質斷言（失敗必須乾淨結束、收下的頁必須在壞行門檻內），計畫字面版以 `#[ignore]` 保留，沒有被放寬後當成通過。
- **過度 mock**：沒有。E 系列是真的 `sip-faxd serve`、真的 UDP、SpanDSP 的音訊數據機；J06 用暫停時鐘但 socket 是真的。
- **產品修正**（`audio.log` 記 V.21 frame）：小而正確，有單元測試；採納，併入 ADR-068。
- **E04（遺失 0.5 %）**：接受為未達成，不修。SpanDSP 對 SpanDSP 在同樣條件下失敗 17/40，我方 7/40，可見這是 G.711 沒有 ECM 時的本質，不是我方收端的缺陷；計畫的「全部收下」是 Opus 寫計畫時的估計。T.38 預設優先（ADR-061）正是為此。報告提出的「頁面 training 遺失後等影像訊號結束就回 RTN，不等 T2」列為後續改善（TODO M6），不擋發版。
- **E05 的 Web `fax.progress`**：接受。`fax.progress` 與 DB 的 `pages` 來自同一個 `RecordWritten::Pages`，T.38 路徑已有 Playwright U11 端到端驗證，傳輸方式不影響這段。
- **偶發風險**：`e04_loss_half_percent_with_jitter_three_seeds_recorded` 的「至少 1 通收下」依實際時序而定；目前 seed 3 每次收下。若 CI 出現偶發失敗，改為只斷言乾淨結束。
- **M04**：最終速率降到 2400（計畫寫 7200 或 4800），是 SpanDSP 的降速路徑，接受。
- **報告 §5 的 `answering the fax (G.711)` 沒帶原因**：`go_audio` 有記原因，但 session 的 `ctx.audio.reason` 是開始監聽時的副本（`None`）。小問題，Opus 已在審核時修正（`AudioContext` 改在 `go` 時以 `go` 通道一併帶入原因，見下一個 commit）。
- **涵蓋率**：fax-in 93.78%（門檻 70%），fax-modem 維持 ≥ 85%。

