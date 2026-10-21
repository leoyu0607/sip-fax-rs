# M3d 測試報告：ECM（接收端）、MMR、解析度設定、T.30 原文核對

- 實作與執行：Sonnet（M3d）
- 計畫：`docs/test-plans/M3.md` §7（E01–E07）與 I04 的 DIS bit 部分；規格：`docs/design/m3-store.md` §8、§10，`docs/design/fax-stack.md` §3
- 狀態：**待 Opus 審核**。沒有 commit、沒有 push。
- `scripts/ci-local.sh` 的結果：它在 `cargo clippy --workspace --all-targets` 停下，原因只有 `crates/store`（另一個 agent 進行中的工作）的 clippy 錯誤（`identity_op`、`bool_assert_comparison`、`double_comparisons`，在 `crates/store/src/tests/*.rs`），**未修**。我自己的 crate 另外驗證（見 §2）：`cargo clippy -p t30 -p t4 -p fax-in -p sip-faxd --all-targets -D warnings` 無問題、`cargo fmt --all --check` 乾淨、`cargo test --workspace --exclude store --all-targets` 596 通過／1 略過、涵蓋率門檻（t30、fax-in）、`scripts/e2e-docker.sh` 全部通過（exit 0，含 SpanDSP／SIPp／t4 oracle／M2d E01–E07／F1／本階段新增）。musl 靜態建置（ci-local 最後一步）因停在 clippy 而**沒跑**。
- 沒有新增依賴。

## 1. 交付內容
| 項目 | 位置 |
|---|---|
| t30 的 ECM 接收：新狀態 `EcmWaitBlock`／`EcmRxBlock`／`EcmWaitPps`；FCD／RCP；PPS-NULL／MPS／EOP／EOM（含 PRI 變體）；PPR bitmap；CTC→CTR（更新速率）；EOR→ERR（缺的 frame 補零）；RR；重送命令時重送上一個回應；`ecm_max_ppr` 保護 | `crates/t30/src/receiver.rs`（新 `impl Receiver` 的 ECM 區塊）、`frame.rs`（`fcf::FCD/RCP`、`fcf2`、`lsb_first`）、`caps.rs`（`parse_rate`、`Dcs.frame_octets`、`unsupported_with`）、`config.rs`（`ecm`、`ecm_max_ppr`）、`result.rs`（`ecm`、`ecm_ppr_sent`、`ecm_frames_lost`、`EcmFailed`、`EcmFramesLost`） |
| DIS 宣告 ECM（bit 27）；MMR（bit 31）只在 ECM 時宣告 | `caps.rs::build_dis_fif` |
| 引擎：image-rate 的 `hdlc-data`／`hdlc-fcs-OK/BAD(-sig-end)`／`hdlc-sig-end` 變成 `Event::Frame`；缺 training 指示時以第一個快速 HDLC 欄位起頭 | `crates/fax-in/src/t38engine/rx.rs` |
| 設定：`[trunks.fax] resolutions`、`ecm`（含驗證）；範例檔 | `crates/fax-in/src/config.rs`（`FaxSection`）、`conf/sip-faxd.toml.example` |
| 接線（見偏離 1）：`SessionContext.fax`、controller 傳入、`engine_config` 套用 `fine`／`ecm`／`mmr` | `crates/fax-in/src/{session,controller,t38session}.rs`（各一處小改動） |
| 與 SpanDSP（ECM 開啟）互通：`EngineRunConfig.ecm`／`drop_frame`／`loss_image_data_only`、`FAXSIM_TRACE`；gateway `FaxTx.ecm`、`sip_uac --ecm on|off` | `tools/fax-sim/src/{engine_loopback,gateway}.rs`、`src/bin/sip_uac.rs` |
| 測試 | `crates/t30/tests/{e_ecm,e05_ecm_table,e06_t30_audit}.rs`（新）、`properties.rs`／`caps.rs`／`r01_table.rs`／`misc.rs`／`common/mod.rs`（更新）、`crates/fax-in/tests/{ecm_engine,config,t38engine}.rs`、`tools/fax-sim/tests/{e_ecm,e2e_m3d}.rs`、`tools/fax-sim/tests/e2e_common/mod.rs`（由 `e2e_m2d.rs` 抽出的共用函式，行為不變） |
| fuzz | `crates/t30/fuzz/fuzz_targets/{receiver_events,frame_parse}.rs`（加入 ECM frame、DCS-ECM、PPS／EOR／CTC／RR／RCP、`parse_rate`、`lsb_first`） |

## 2. 指令
```sh
cargo test -p t30 -p fax-in                                  # 主機
cargo clippy -p t30 -p t4 -p fax-in -p sip-faxd --all-targets -- -D warnings
cargo llvm-cov -p t30 --summary-only ; cargo llvm-cov -p fax-in --summary-only
scripts/e2e-docker.sh                                        # tools workspace：E01–E04、M3d E2E、F1、M2d …
docker run … cargo test -p fax-sim --test e_ecm -- --nocapture   # 只跑 ECM 互通，印出每格證據
FAXSIM_TRACE=1 …                                             # 印出 SpanDSP 送出的每個 IFP（除錯）
cd crates/t30 && cargo +nightly fuzz run receiver_events -- -max_total_time=120   # 同樣 frame_parse
```
測試數：t30 新增 `e_ecm` 27、`e05_ecm_table` 3（11 個情境 × 28 個事件）、`e06_t30_audit` 8、`properties` +1；fax-in 新增 `ecm_engine` 6、`config` +2；tools：`e_ecm` 11、`e2e_m3d` 5。

## 3. 案例結果
### 3.1 E 系列（計畫 §7）
| ID | 結果 | 證據 |
|---|---|---|
| E01 | 通過（有一格做不到，見下） | SpanDSP（ECM 開）當 caller，我方引擎當 answerer，`tools/fax-sim/tests/e_ecm.rs`：頁數 1／3 × standard／fine × MH／MR × 4800（V.27ter）／9600（V.29）／14400（V.17）＝**24 通全部成功**。每通：SpanDSP＝OK、我方＝`ok`、`ecm=true`、`ecm_ppr_sent=0`、`bad_lines` 全 0、TSI、`bit_rate`、`compression` 皆符合、**逐像素相同**（t4 解回 MMR 與來源位圖逐 octet 比）。模擬時間（ms）：1 頁 MH 4800／9600／14400＝38980／27200／24660；MR（標準）＝31500／23480／22180；3 頁 MH＝101620／61340／49400。另：標準／fine／標準三頁（解析度改變，SpanDSP 用 **PPS-EOM**）通過，52500 ms，3 頁逐像素相同；T.38 v0／v1／v2／v3 皆通過；ECM 的 SpanDSP 對我方關閉 ECM 的 DIS、以及 ECM 關閉的 SpanDSP 對我方開啟 ECM 的 DIS，皆回到 non-ECM 並通過。**frame 大小 64：SpanDSP 3.1.1 做不到**（`t30.c`：「Always use 256 octets per ECM frame, whatever the other end says」，且沒有 API）。64 octet 的路徑改由 `crates/fax-in/tests/ecm_engine.rs`（手寫的 caller，frame 依 T.30／T.4 原文組出，2 頁、每塊 40 個 frame、PPS-NULL／MPS／EOP，逐像素相同）與 t30 單元測試（`e01u_*`，DCS bit 28）涵蓋——這一格**沒有第三方互通**。ECM 搭配 localTCF：SpanDSP 的 T.38 terminal 在 `NO_TCF`＋ECM 下只送出一個空的 `hdlc-data` 就停住（觀察到，見 §5），所以也沒有互通，只有 t30 單元測試（`e01u_local_tcf_with_ecm_…`）。 |
| E02 | 通過 | M2c X01 留下的 MMR 格：ECM 開、SpanDSP 遮罩只留 T.6，頁數 1／3 × standard／fine × 3 種速率＝**12 通全部成功**，`compression=mmr`，逐像素相同（1 頁 4800／9600／14400＝27480／21460／20820 ms；3 頁＝60560／40800／35700 ms）。 |
| E03 | 通過（以「只讓 ECM 影像資料遺失」的定義；全部封包遺失的版本失敗，見 §5） | **無 UDPTL redundancy**（`ErrorRecovery::None`），caller→engine 方向的 image-rate 資料包有 5 % 遺失（兩個方向各自獨立、帶種子的 PRNG），5 個 seed，2 頁 MMR：**5 個 seed 全部成功且逐像素相同**，`recovered_by_udptl=0`（UDPTL 層沒有補回任何東西）、`frames_lost=0`。PPR 次數（我方送出）：seed1＝4、seed2＝4、seed3＝7、seed4＝5、seed5＝3（合計 23）；掉的 datagram 21／20／35／21／21（佔所有 datagram 的 2.1–2.7 %，因為只計影像資料包），UDPTL gap（引擎側）20／18／33／21／20。模擬時間 42–65 s（無遺失約 28 s）。**對照組**（同 seed、同遺失、SpanDSP 不用 ECM、無 recovery）：5 個 seed 沒有一個乾淨（壞行 [10,8]、[14,17]，或 `t2_expired_command`／SpanDSP 失敗碼 38、14、21）。**補充**：遺失加在「每一個封包、兩個方向」且無 redundancy 時，5 個 seed **全部失敗**（0/5），失敗原因與 ECM 無關，見 §5-1；同樣遺失但 redundancy 3 時 5 個 seed 全部成功（回補 20／20／27／22／21 個封包、PPR＝0）。 |
| E04 | 通過 | 固定丟掉第 3 號 frame 的每一次傳送（`drop_frame`；datagram 仍編號，所以接收端看到真正的序號缺口）。SpanDSP 的行為：4 次 PPR → **CTC**（SpanDSP 因為第一輪有進展，降一級速率：結果的 `bit_rate`＝**12000**，我方正確更新）→ CTR → 再 4 次 PPR → **EOR** → ERR。1 頁（EOR-EOP）：我方結果 `ecm_frames_lost`、`pages=1`、`ppr_sent=8`、`frames_lost=1`、frame 3 被丟 8 次（1＋3＋4），57200 ms 結束，沒有卡住；頁面送出，缺的 frame 補零。2 頁：SpanDSP 在 ERR 之後送 DCN（`t30.c` 的 `process_state_iv_eor` 寫著 TODO：不會接著傳下一頁），所以我方結果是 `partial_dcn`、`pages=1`、`frames_lost=1`。主機側（手寫 caller）另有 EOR-NULL／MPS／EOP／EOM 四種後續與 `ecm_max_ppr` 超限（送 DCN，`ecm_failed`）。 |
| E05 | 通過 | `crates/t30/tests/e05_ecm_table.rs`：11 個情境（`WaitTcf`／`SendingCfr`（ECM 的 DCS）、`WaitBlock`、`WaitBlockHeld`（PPR 之後、區塊保留）、`WaitBlockAfterMcf`、`RxBlock`、`WaitPps`、`WaitPpsPartial`、`SendingMcfNull`、`SendingPpr`、`WaitDcn`）× 28 種事件（含 FCD、壞 FCS 的 FCD、RCP、PPS-NULL／EOP／EOM、EOR-NULL／EOP、CTC、RR、DCN、CRP、非 ECM 的 MPS、各計時器、各信號）＝308 格，其中 103 格寫成字面表（`EXPECTED`，**逐列對照 T.30 Annex A 手寫，不是把程式輸出貼上**；寫完第一版後有 25 格與實作不同，逐格檢查：19 格是我手寫表格的錯誤（計時器的 start／cancel 合併方式、頁面行數、重送命令的情況），6 格（`RxBlock` 收到 PPS／CTC／EOR 時）是實作多餘的 `start T2`＋`cancel T2`，已修實作），其餘 205 格必須「狀態不變、零動作、計時器不變」，且重複送入第二次仍不變。R01（非 ECM，345 格）**一字未改**仍通過（`State::ALL` 維持 15 個非 ECM 狀態，ECM 狀態在 `State::ECM`；`ALL_WITH_ECM` 是全部）。另有 proptest `ecm_receiver_is_total_and_consistent`（1500 例：任意 FCD／命令／信號／計時器序列；Done 後無動作、取消的計時器必在執行中、PPR 必為 32 octet 的 FIF、頁位圖大小正確）。 |
| E06 | 通過，**取得 ITU 原文**，沒有發現 bit 位置差異 | 見 §4。`crates/t30/tests/e06_t30_audit.rs`（8 個測試）把 T.30 表 2 的 bit 號碼、5.3.6.1.x 的二進位格式、圖 A.1 的 FCF2、T.4 A.3.5 的 FCD／RCP、速率位元表、最小掃描時間表、寬度／長度表全部以「照原文抄」的字面值檢查。 |
| E07 | 通過 | F1 的重播與現場回歸（ECM 關閉的發送端）：`f1_trace_replay`／`f1_turnaround`／`f1_repeat_command`（主機）與 e2e 的 `f1_05_eight_pages_from_a_gateway_that_splits_fcs_ok_and_sig_end`（8 頁，`result.json` code 0，split frames 10）都通過；**整個 M2d E01–E07（真實 `sip-faxd`，預設就宣告 ECM，gateway 不用 ECM）也都沒改就通過**，這是「發送端不用 ECM 時行為不變」的證據。 |

### 3.2 I04（DIS bit 部分）與 E2E
`tools/fax-sim/tests/e2e_m3d.rs`，真實 `sip-faxd serve`，fax-sim gateway（SpanDSP ECM 開）；每個圖像檔以 libtiff（`tiffcmp -t`＋`tiffcp -c none` 逐 byte）與 poppler（`pdftoppm -mono`）**各比一次**，兩者都有負向對照（ADR-025）；DIS／DCS 取自 `udptl.log` 的 summary（ADR-026：影像資料只顯示長度，測試斷言 image-rate 的行內沒有 `)=`）。

| 案例 | 結果與證據 |
|---|---|
| `m3d_e2e_01` ECM＋MMR，3 頁 fine | 通過。SpanDSP＝0；DIS bit 27、31 為 1；DCS bit 27、31 為 1、bit 28 為 0（256 octet）；`result.json`：`compression=mmr`、pages 3、`bit_rate` 14400；TIFF 19418 B、PDF 20426 B；3 頁 × libtiff／poppler 皆逐像素相同；duration 34369 ms。 |
| `m3d_e2e_02` ECM＋MH、ECM＋MR | 通過，各 2 頁，`compression` 為 `mh`／`mr`，逐像素相同（35287／29783 ms）。 |
| `m3d_e2e_03` ECM＋5 % 遺失＋redundancy 3 | 通過；gateway 掉 18＋5 個 datagram，server 以 redundancy 補回 18，`lost=0`，逐像素相同。 |
| **I04** `m3d_i04…` `resolutions = ["standard"]` | 通過。DIS 的前 3 個 octet＝`00 75 1F`（**bit 15 為 0**；bit 27、31 仍為 1）；DCS bit 15 為 0；SpanDSP 持有 fine 頁面，成功送出（code 0）；結果 `resolution=standard`，`tiffinfo`：204×98；SpanDSP 把 fine 降成 standard 的方式是「取偶數列」，儲存的頁面與來源偶數列**逐 byte 相同**（測試依序比對「偶數列／奇數列／相鄰兩列 OR」，只有第一個相符）。 |
| `m3d_e2e_04` `ecm = false` | 通過。DIS＝`00 77 1E`（沒有 bit 27／31）；ECM 開的 SpanDSP 退回 non-ECM（DCS bit 27 為 0）；2 頁逐像素相同。 |

## 4. T.30 原文核對（E06）
### 4.1 取得來源
- **取得了 ITU-T T.30 (09/2005) 全文**（105 頁 PDF，2 583 878 bytes，SHA-256 `3de4b9ed…360f`），下載網址：`https://www.itu.int/rec/dologin_pub.asp?lang=e&id=T-REC-T.30-200509-I!!PDF-E&type=items`（`https://www.itu.int/rec/T-REC-T.30` 頁面列出 09/2005 為現行版，並有 2007/01 的 Amendment 1，只涉及 V.34 降速時的靜默時間，與本階段無關）。同法取得 **ITU-T T.4 (07/2003)**（SHA-256 `84c66fab…74f8`），因為 FCD／RCP 的格式在 T.4 附錄 A。用 `pdftotext`（Docker 映像內）轉成文字，圖 A.1 另以 `pdftoppm` 轉成圖片目視確認。
- 另外交叉對照 SpanDSP 3.1.1 原始碼（`t30_fcf.h`、`t30.c` 的 `send_pps_frame`、`process_rx_pps`、`process_rx_ppr`、`get_partial_ecm_page`、`send_next_ecm_frame`）：SpanDSP 的 FCF 在記憶體內是位元反轉，逐一反轉後與原文一致（例如 `T30_PPS 0xBE`→`0x7D`、`T4_FCD 0x06`→`0x60`、`T4_RCP 0x86`→`0x61`）。

### 4.2 對照表（我方的值 vs 原文；「位元 n」＝表 2 的 bit n）
| 項目 | 原文位置 | 原文 | 我方 | 結果 |
|---|---|---|---|---|
| DIS 位元 3 | 表 2、註 61 | Real-time Internet fax（T.38） | `bits::T38`＝3 | 相符 |
| 位元 6、7 | 表 2、註 23、42 | 6＝V.8；7＝DIS 的 frame 大小偏好（0＝256、1＝64） | 6 不設；7 不設（偏好 256） | 相符 |
| 位元 9、10 | 表 2、註 18–20 | 9＝polling；10＝接收能力（DCS：接收命令） | 9 不設；10 設 | 相符 |
| 位元 11–14 | 表 2、註 31–33 | `0000` 2400 V.27ter；`0100` 4800；`1000` 9600 V.29；`1100` 7200；`0001` 14400 V.17；`0101` 12000；`1001` 9600；`1101` 7200；DIS `1101`＝V.27ter＋V.29＋V.17 | `parse_rate`／`build_dis_fif`；DIS 的 V.17 一律連同 11、12 | 相符（16 列逐列測） |
| **V.33（M2c 待查）** | 表 2 DCS 欄、註 31、32 | DCS 的 `0010`、`0110` 在 2005 版是 **Invalid**；註 31：1994 年版以前的終端機用它們表示 V.33（14400／12000），具 V.17 的接收端「may optionally support」 | 位元 13 的位置確認是 V.33 的位置；我方**不宣告** V.33，DCS 收到 `0010`／`0110` 仍當作 V.33 接受（舊機器相容）；其餘 `1010`、`1110`、`0011`、`0111`、`1011`、`1111` 為保留→FTT | **已解決**：位置正確，行為符合註 31 |
| 位元 15 | 表 2、註 11 | R8×7.7 與／或 200×200；沒有 41–46 時＝R8×7.7 | `FINE`＝15；不設 44／45 | 相符 |
| 位元 16 | 表 2 | 二維編碼（MR） | `TWO_D`＝16 | 相符 |
| 位元 17、18 | 表 2、註 6 | `00` 215 mm；DIS `01`＝A4＋B4＋A3、DCS `01`＝303 mm；`10`＝255 mm；`11` 無效（當 `01`） | B4→bit 17；A3→bit 18；解析 `11`→A3 | 相符 |
| 位元 19、20 | 表 2 | `00` A4；`01` 無限；DIS `10`＝A4＋B4、DCS `10`＝B4 | `LENGTH_1`＝19（B4）、`LENGTH_2`＝20（無限） | 相符 |
| **位元 21–23（M2c 待查）** | 表 2、註 4、8 | 依印出順序 b21 b22 b23：`000` 20 ms；`001` **40 ms**；`010` **10 ms**；`100` **5 ms**；`011` 10 ms（T7.7＝½T3.85）；`110` 20 ms（½）；`101` 40 ms（½）；`111` 0 ms | 碼＝b21＋2·b22＋4·b23：`Ms5`＝1（b21）、`Ms10`＝2（b22）、`Ms40`＝4（b23）、`Ms0`＝7；其餘為 `Other(碼)` | **已解決**：8 列全部相符，SpanDSP 的對應是對的 |
| 位元 24、32、40… | 表 2、註 5 | 延伸位元 | 只從第 3 個 octet 起 | 相符 |
| 位元 26 | 表 2 | Uncompressed mode | `UNCOMPRESSED`＝26，不宣告、DCS 要求→拒絕 | 相符 |
| **位元 27（ECM）** | 表 2、註 17 | Error correction mode | `ECM`＝27；`[trunks.fax] ecm` 為真才設 | 相符 |
| **位元 28** | 表 2、註 7、24 | DIS：「Set to 0」；DCS：frame 大小 0＝256、1＝64 octet（僅在 bit 27 為 1 時有效） | DIS 不設；DCS 讀 `frame_octets` | 相符 |
| **位元 31（T.6）** | 表 2、註 9、17 | T.6 只在 bit 27 為 1 時有效；「bit 31 為 1 時 bit 27 必須為 1」 | 只在 ECM 為真時設 31（**舊行為：不管 ECM 都能設，已改**；R02 與 proptest 的期望值已更新） | 相符（修正了設定組合） |
| 位元 41–45 | 表 2、註 10–14 | 41＝R8×15.4；42＝300×300；43＝R16／400×400；44／45＝inch／metric；DCS 44：0 metric、1 inch | 常數與解析皆相符；不宣告 | 相符 |
| FCF（DIS/CSI/NSF/DTC/CIG/DCS/TSI/NSS/SUB/SID/CFR/FTT/EOM/MPS/EOP/PRI-*/EOS/MCF/RTP/RTN/PIP/PIN/RNR/DCN/CRP/FNV） | 5.3.6.1.1–8 | 二進位格式（X 印在最前） | `frame::fcf` | 相符（35 項逐項以印出的格式字串測） |
| **PPS** `X111 1101`、**EOR** `X111 0011`、**RR** `X111 0110` | A.4.3 | | `PPS 7D`、`EOR 73`、`RR 76` | 相符 |
| **PPR** `X011 1101`、**RNR** `X011 0111`、**ERR** `X011 1000` | A.4.4 | PPR 的 FIF＝固定 256 bit，第一個 bit 對應 frame 0，正確＝0、錯誤／未收＝1，超出區塊的位元為 1（圖 A.5） | `PPR 3D`、`RNR 37`、`ERR 38`；PPR bitmap 照此建 | 相符 |
| **CTC** `X100 1000`、**CTR** `X010 0011` | A.4.1、A.4.2 | CTC 的 FIF 是 2 octet（DCS 的 bit 1–16），接收端只用 bit 11–14 | `CTC 48`、`CTR 23`，`parse_rate` 取 bit 11–14 | 相符 |
| **FCF2**（PPS／EOR 的第一個 FIF octet） | 圖 A.1、A.2 | NULL `0000 0000`；EOM `1111 0001`；MPS `1111 0010`；EOP `1111 0100`；EOS `1111 1000`；PRI-EOM／MPS／EOP `1111 1001／1010／1100` | `fcf2::*`；PPS／EOR 解析時忽略 X 位元（其他終端機可能不設） | 相符 |
| **PPS 的計數器** | 圖 A.1 註 2–5 | I1＝頁計數（mod 256）、I2＝區塊計數、I3＝**frame 數 −1**；「least significant bit 先送」 | `lsb_first`（位元反轉）；範例頁 1、區塊 2、10 個 frame → `80 40 50`（以圖中的位元串測） | 相符 |
| **FCD／RCP** | T.4 A.3.5 | FCF `0110 0000`／`0110 0001`；控制欄 `1100 X000`，X＝0；每個 frame 的 FIF＝frame 號（LSB 先送）＋64 或 256 octet；RCP 連送 3 個，無 FIF | `fcf::FCD＝0x60`、`RCP＝0x61`；frame 號以 `lsb_first`；兩種控制欄值都接受 | 相符（SpanDSP 互通證明 frame 號的位元順序） |
| 位址／控制欄 | 5.3.4、5.3.5 | `1111 1111`、`1100 X000` | `FF`、`C0`／`C8` | 相符 |
| 識別字串 | 5.3.6.2.4–6、表 3 | 20 位數字，「最低位數字的最低位元先送」 | `encode_ident`：反序、補空白、每個 octet 位元反轉 | 相符（`1`＝`8C`、空白＝`04`） |
| ECM frame 大小 | A.3.2、A.3.3 | 256 或 64 octet（不含 FCF 與 frame 號），接收端必須兩者都能收；區塊最多 256 個 frame | `frame_octets` 64／256；一個區塊最多 256 個 frame | 相符 |
| 重送上限 | A.1.3 | 同一區塊收到 4 次 PPR，發送端送 EOR 或 CTC | 接收端不計 4 次，只在連續 `ecm_max_ppr`（12）次 PPR 都沒有 CTC／EOR 時放棄 | 符合；上限是我的保護值 |

**結論**：沒有發現任何 bit 位置或 FCF 值與原文不符。M2c 兩個待查項目都有了原文依據（V.33：位置無誤，原文把它列為「Invalid／舊版相容」；掃描時間碼：8 列全部與 SpanDSP 的對應相同）。唯一被修正的是**組合規則**：T.6（bit 31）不得在沒有 ECM（bit 27）時宣告（註 9、17）。M2c 報告 §7-4 與 ADR-019 的「原文離線無法取得」可以結案。

## 5. 失敗與根因（原始輸出摘要）
1. **E03「全部封包遺失、無 recovery」0/5 失敗**（保留成測試 `e03_five_percent_loss_on_everything_without_recovery_…`，只報告、不要求成功）。例：seed1 SpanDSP＝「No response after sending a page」、我方 `t2_expired_command`、`ppr_sent=1`；seed3 SpanDSP＝「Received no response to DCS or TCF」、我方 `t2_expired_image`。根因（量測加推算）：引擎沿用 SpanDSP 的 pacing，V.21 frame 每個封包只放 1 octet（`hdlc_chunk_ms=30`），一個 PPR（FF C8 3D＋32 octet＋FCS）要 35 個封包，每個 5 % 遺失 → 整個 frame 完好的機率約 17 %；SpanDSP 的重試上限是 3 次；而 T.30 的 T2（6 s）只容許發送端把遺失的命令重送一次（T4 3 s＋前導 1 s）。這與 ECM 無關：同樣條件下對照組（SpanDSP 不用 ECM）也是 5 個 seed 都不乾淨。**我試過 `hdlc_chunk_ms = 400`（PPR 只要 3 個封包）**，仍 0/5（不同原因：發送端的 DCS／PPS 等 V.21 frame 也是 1 octet/封包，失敗點移到那裡），所以沒有採用。因此 E03 的「PPR 補回」只能用「遺失只加在 ECM 影像資料包」的版本證明（5/5 成功）；「全部封包遺失」在有 redundancy 時才成立（5/5）。**這是計畫的 E03 與現實的落差，請 Opus 確認這個詮釋**。
2. 第一版 E05 表格 25 格與實作不同：19 格是我寫錯的字面值，6 格是實作的多餘動作（見 E05 列）。
3. E03 的三個測試共用同一組暫存檔名，並行時互相覆蓋來源 TIFF，造成一次「偶發」失敗（`…redundancy_3_also_works`）。根因是測試碼；已加上標籤區分檔名，連續 3 次完整執行穩定。
4. SpanDSP 的 T.38 terminal 在 ECM＋`T30_IAF_MODE_NO_TCF` 下：DCS 後送 `v17-14400-short-training`＋一個空的 `hdlc-data`，之後什麼都不送（`FAXSIM_TRACE` 的輸出），我方 6 s 後 `t2_expired_image`。非我方行為；該組合只有單元測試。
5. clippy：`collapsible_match`、`explicit_counter_loop`（`ecm_engine.rs`）、`len_zero`（`e2e_m3d.rs`）、`if_same_then_else`（`engine_loopback.rs`）已修。
6. R01 的 `setup()` 對 `State` 是窮盡 `match`，加入 ECM 狀態後必須多一個分支；加的是 `panic!("covered by the E05 matrix")`，R01 的 345 格與預期一字未動。

## 6. 涵蓋率（`cargo llvm-cov`，行）
| crate／檔案 | 行涵蓋率 |
|---|---|
| **t30（門檻 85）** | **97.86 %**（1308 行、漏 28） |
| receiver.rs | 97.28 % |
| caps.rs／config.rs／event.rs／frame.rs／result.rs | 99.48／100／100／97.73／100 % |
| **fax-in（門檻 70）** | **93.95 %**（2977 行、漏 180） |
| t38engine/rx.rs／mod.rs | 97.84／94.93 % |
| config.rs | 93.27 % |

fuzz：`receiver_events` 跑 120 s（836 580 次）、`frame_parse` 跑 60 s（10 411 401 次），皆無 crash／斷言失敗；receiver_events 現在涵蓋 ECM 設定（`ecm_max_ppr=3`）、FCD frame、PPS／EOR／CTC／RR／RCP、符合格式的 ECM DCS。

## 7. 與規格／計畫的偏離與取捨
1. **動到計畫範圍外的三個檔案**（各一處小改動）：`crates/fax-in/src/session.rs`（`SessionContext.fax`）、`controller.rs`（填入 `trunk.cfg.fax.clone()`）、`t38session.rs`（`engine_config` 套用 `fine`／`ecm`／`mmr`）與 `tests/t38session.rs`（兩個 `SessionContext` 字面值加 `fax: Default::default()`）。沒有這些接線，`[trunks.fax]` 不會生效、也無法做真實 `sip-faxd` 的 E2E。若 store 那邊也改 `SessionContext`，會有合併衝突（欄位新增處）。`apps/sip-faxd` 的 `config check` 輸出沒有顯示 resolutions／ecm（不在授權範圍）。
2. **E03 的定義**（見 §5-1）：遺失只加在 image-rate 資料包。
3. **`[trunks.fax] resolutions = ["fine"]`**：T.30 無法表達「只有 fine」，standard 是所有終端機的基本解析度；列 `["fine"]` 只宣告 bit 15，收到 standard 的 DCS 仍接受。`["standard"]` 才是不宣告 bit 15（I04）。DCS 要求 DIS 沒宣告的 fine 時我方一律接受（寬鬆）。
4. **ECM 頁面品質**：ECM 沒有 RTN／RTP；整頁一律 MCF，`bad_lines` 照 t4 解碼器統計。EOR 放棄的 frame 以零補到協商的 frame 大小再送進解碼器（MH／MR 會在下一個 EOL 重新同步，MMR 會放棄該頁其餘部分），並累計 `ecm_frames_lost`。行數為 0 的 ECM 頁不輸出（記 log）。
5. **DCN 在區塊進行中**：已完成的頁面保留（`partial_dcn`），進行中的頁面丟棄（與 non-ECM 的 RxImage 相同）；沒有嘗試救回半頁。
6. **MMR**：DIS 只在 ECM 時宣告（bit 31）；DCS 若在沒有 ECM 時選 T.6，仍照舊解碼（寬鬆，M2c 就是如此）。
7. **區塊結束的判定**：第一個 RCP、訊號結束、PPS／CTC／EOR 本身、或 15 s 的閒置計時器（`Timer::Data`）。frame 數取 PPS 的 I3＋1，多次 PPS 取最大值（SpanDSP 在重送時把 I3 填成「本次重送的 frame 數」，所以不能以最後一個為準）。
8. **重送偵測**：同一個 PPS／CTC／EOR（FCF＋FIF 完全相同），且自上次回應後沒有新的 training 指示、FCD 或 RCP，視為回應遺失，原樣重送上一個回應（不增加 PPR 計數）。
9. **RNR 不送、RR 當作狀態查詢**（重送上一個回應）；T5（60 s）、PIP／PIN、EOS、流量控制模式都沒有做。
10. **新的 `ResultCode`**：`EcmFailed`（13）、`EcmFramesLost`（14）；`T30Result` 多 `ecm`、`ecm_ppr_sent`、`ecm_frames_lost`。ECM 結果的 `FaxReport`（fax-in 的 `event.rs`）**沒有**帶出這三個欄位（不在授權範圍），`result.json` 目前看不到 PPR 次數；`EcmFramesLost` 會讓 `fax.code≠0` → 通話被判 `Failed(Session)`，但頁面仍輸出並標 `partial`。

## 8. 擬議 ADR（請 Opus 合併到 `docs/DECISIONS.md`，編號自行調整）
### ADR-031：ECM 接收的設計與範圍（2026-10-21）
- 依 T.30 Annex A／T.4 Annex A；`t30` 新增狀態 `EcmWaitBlock`／`EcmRxBlock`／`EcmWaitPps`，區塊在 PPS 確認時一次交給 `t4` 解碼器；PPS-NULL／MPS／EOP／EOM、PPR（256 bit，區塊外的位元為 1）、CTC→CTR（更新速率）、EOR→ERR（缺的 frame 補零並計入 `ecm_frames_lost`）、RR 重送上一個回應；重複的命令重送上一個回應；`ecm_max_ppr`（預設 12）保護。結果碼 `EcmFramesLost`（頁面有洞）、`EcmFailed`。
- FCD／RCP 的 frame 號與 PPS 計數器採 LSB first，在本專案 octet 慣例（第一個傳送的位元為 MSB）下是位元反轉值（ADR-019 的延伸）。
- 引擎：image-rate 的 HDLC 欄位與 V.21 走同一條路徑變成 `Event::Frame`；送出端不變（回應都在 V.21）。
- 不做：RNR、T5、PIP／PIN、EOS、流量控制模式、半頁救回。
### ADR-032：DIS 宣告規則與 `[trunks.fax]`（2026-10-21）
- `ecm`（預設 true）決定 bit 27；MMR（bit 31）只在 ECM 時宣告（T.30 表 2 註 9、17）；`resolutions`（預設 `["standard","fine"]`，不得為空、只允許這兩個值）決定 bit 15；DIS bit 7（frame 大小偏好）不設、bit 28 不設。
### ADR-033：T.30 原文核對結論（2026-10-21）
- 已以 ITU-T T.30 (09/2005) 與 T.4 (07/2003) 原文核對 DIS／DCS 位元與 ECM 相關 FCF（`crates/t30/tests/e06_t30_audit.rs`），無差異。V.33：DCS `0010`／`0110` 在 2005 版為 Invalid，依註 31 作為舊機器相容接受、不宣告。掃描時間碼依表 2。ADR-019 末段的「原文離線無法取得」作廢。
### ADR-034：SpanDSP ECM 互通的限制（2026-10-21）
- SpanDSP 3.1.1 一律送 256 octet 的 ECM frame；ECM＋`NO_TCF` 的 T.38 terminal 會停住；EOR/ERR 之後不會接著傳下一頁（直接 DCN）。64 octet frame 與 ECM＋localTCF 只由單元／引擎測試涵蓋。E03 的 PPR 證明以「只讓影像資料包遺失」進行（見本報告 §5-1）。

## 9. 未解事項（誠實清單）
- **對真實 gateway 的 ECM 完全沒有驗證**（只有 SpanDSP 與自己寫的 caller）。預設 `ecm = true`，所以部署後凡是支援 ECM 的傳真機都會改走 ECM；AudioCodes 類 gateway 對 image-rate `hdlc-data` 的拆包（`hdlc-fcs-OK` 與 `hdlc-sig-end` 分開）已有單元測試（裸 `hdlc-sig-end` 結束區塊），但沒有現場擷取。建議 M6 用現場擷取驗證，必要時先把預設改成 false。
- **V.21 frame 每包 1 octet 的遺失暴露**（§5-1）：在沒有 UDPTL 保護且遺失率高的網路上，長的 V.21 frame（PPR、含 TSI 的 DCS）容易整個失敗。`Pacing.hdlc_chunk_ms` 可調但沒有設定檔對應；是否對長 frame 合併封包需要現場資料。
- 64 octet frame、ECM＋localTCF 沒有第三方互通對象。
- 與 SpanDSP 作為「接收端」的方向（我方當 caller）仍屬後續。
- `FaxReport`／`result.json` 沒有 ECM 欄位；`config check` 不顯示 `[trunks.fax]`；兩份 README 與 `docs/TODO.md`／`ROADMAP.md`／`DECISIONS.md` 未更新（不在授權範圍，待 Opus）。
- `scripts/ci-local.sh` 因 `crates/store` 的 clippy 錯誤沒有一次跑完；musl 靜態建置沒跑。
- 區塊中途 DCN 時不救回半頁。

## 10. 異動的檔案
- 新增：`crates/t30/tests/{e_ecm,e05_ecm_table,e06_t30_audit}.rs`、`crates/fax-in/tests/ecm_engine.rs`、`tools/fax-sim/tests/{e_ecm,e2e_m3d}.rs`、`tools/fax-sim/tests/e2e_common/mod.rs`、`docs/test-reports/M3d.md`。
- 修改：`crates/t30/src/{lib,frame,caps,config,receiver,result}.rs`、`crates/t30/tests/{caps,properties,r01_table,misc,common/mod}.rs`、`crates/t30/fuzz/fuzz_targets/*.rs`、`crates/fax-in/src/{config,session,controller,t38session}.rs`、`crates/fax-in/src/t38engine/rx.rs`、`crates/fax-in/tests/{config,t38engine,t38session}.rs`、`conf/sip-faxd.toml.example`、`tools/fax-sim/src/{engine_loopback,gateway}.rs`、`tools/fax-sim/src/bin/sip_uac.rs`、`tools/fax-sim/tests/e2e_m2d.rs`（共用函式抽到 `e2e_common`）。

## Opus 審核結論（2026-10-21）
**結論：通過，並調整預設值。**

已檢查：
- **E01/E02**：和 SpanDSP 互打 36 種組合，每一頁都逐像素相同。
- **E05**：ECM 表格共 308 格，依據 T.30 Annex A 手寫預期值，不是從程式輸出複製來的。
- **E06**：實際取得 T.30（2005）和 T.4 原文核對，M2c 留下的待查項目都已結案。另外修正一個真的不符合規範的行為：沒有 ECM 時不得宣告 MMR。
- **F1 回歸**：現場重播測試仍然通過，M2d 的 E2E 也不受影響。

確認 E03 的重新定義：在每個封包都注入遺失、又沒有 UDPTL recovery 時，失敗的原因是 PPR 本身被拆成 35 個小封包、SpanDSP 只重試 3 次，和 ECM 的正確性無關。所以改成「只對 ECM 影像資料注入遺失」來驗證 PPR 的補回能力，這個做法合理。對全部封包注入遺失的版本保留為只記錄、不判定的測試，可以接受。

**審核時的調整：ECM 改為預設關閉（ADR-035）。** ECM 只和 SpanDSP 互通過，而 AudioCodes 現場是用 non-ECM 打通的，不應該把真機沒驗證過的路徑設成預設。測試已同步調整：E2E 中需要 ECM 的案例都明確設定 `ecm = true`；預設值的測試改為斷言 `ecm = false`。

列入 M3b：
- `result.json`、DB 欄位 `ecm`、`config check` 的輸出，都要顯示 ECM 和協商結果。
- 64-octet frame 只用我們自己手寫的發送端測過。要等真機驗證（M6）。
