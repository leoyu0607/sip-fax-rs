# 現場試接指南（v0.4.1-alpha）

這一版的目的有兩個：**實際對接中華電信 CS210 收傳真**，以及**取得 CS210 的真實擷取資料**。

0.4.1 的改善（Web 介面）：
- 右上角點自己的名稱打開帳號選單（我的帳號、語言、外觀、登出）；選單「傳真」旁顯示未讀份數，即時更新。
- 傳真列表點一列任何地方就開啟；時間顯示「今天 14:05」「昨天」等簡短寫法。
- 傳真內容頁把詳細資料、備註與投遞放在預覽旁邊。
- 設定頁中需要重啟才生效的欄位以 ⟲ 標示。

**從 0.4.0 升級**：停止舊版，把新版解壓到新目錄，再把舊目錄的 `conf/sip-faxd.toml`、`conf/secret.key`、`data/` 複製過去後啟動。資料表沒有變更。

0.4.0 新增的功能（M4e、M4f）：
- **設定檔只留啟動設定**：資料庫、Web、存檔目錄、log 與服務管理；SIP、中繼、投遞等在 Web 介面設定（見第 2 節）。新安裝第一次啟動時沒有中繼，所有來電都會被拒絕，log 與 Web 介面會提示到 設定 → 中繼 新增。
- **資料庫分欄位填寫**，密碼原樣填（或 `password_file`）；`./sip-faxd db check` 測試連線並說明連不上的原因；`./sip-faxd db copy` 把全部資料搬到另一個資料庫（例如從 SQLite 改用 SQL Server）。sip-faxd 需要自己專用的資料庫。
- Web 介面：「外觀」可以選 跟隨系統／明亮／深色（0.4.1 起在帳號選單裡）；系統狀態的已執行時間每秒更新。

**從 0.3.0 升級**：停止舊版，把新版解壓到新目錄，再把舊目錄的 `conf/sip-faxd.toml`、`conf/secret.key`、`data/` 複製過去後啟動。舊設定檔裡的執行設定區段（`[sip]`、`[[trunks]]` 等）已經在資料庫裡，不會再匯入；啟動 log 與 `./sip-faxd config check` 會列出可以從設定檔刪除的區段。

0.3.0 新增的功能（M4）：
- **Web 介面**（預設開啟，HTTPS 8443）：即時看到收進來的傳真（接收中會逐頁顯示）、預覽與下載、已讀與備註；管理員可以在網頁上修改設定與帳號。使用說明見壓縮檔內的 `docs/web.md`。
- **設定存在資料庫**：SIP、中繼、投遞等存在資料庫，在 Web 修改，大部分不必重啟就套用。
- **服務管理**：`./sip-faxd start|stop|restart|status|logs`，以及終端機畫面 `./sip-faxd tui`（見第 4 節）。

0.2.0 新增的功能（M3）：
- **資料庫紀錄**：預設 SQLite（`data/sip-faxd.db`，內建，不需要另外安裝）。每通電話一筆：進線號碼（ANI）、撥打號碼（DNIS）、掛線方向與原因、狀態（接收中／完整／部分／失敗／非傳真）、頁數、檔案大小。設定在 `[database]`、`[retention]`。
- **號碼來源**：取自哪個 header 由每個 trunk 的 `[trunks.numbers]` 決定（預設 `ani_sources = ["pai","rpid","from"]`、`dnis_sources = ["ruri","to"]`，這是 AudioCodes 的實測）。**CS210 在哪個 header 帶號碼還不知道**，請回傳 `sip.log`，我們會據此調整 `cht-cs210` 預設。
- **投遞**（選用）：在 `[[routes]]` 設定資料夾、Email 或 Webhook，範例見 `conf/sip-faxd.toml.example`。沒有設定路由時，傳真只存檔與寫入資料庫。
- **清整**：預設檔案保留 90 天、紀錄保留 365 天，每天 03:17 執行。可以先用 `./sip-faxd purge --dry-run` 看會清掉多少。
- 資料庫連不上不影響收傳真，之後可用 `./sip-faxd db backfill` 補回。
- `./sip-faxd config check` 會顯示上述所有設定。
- ECM 已支援但預設關閉（`[trunks.fax] ecm = false`），真機驗證後才會重新評估。這次試接**請維持關閉**。

**從 0.2.0 升級**：停止舊版，把新版解壓到新目錄，再把舊目錄的 `conf/sip-faxd.toml`、`data/`（含 `sip-faxd.db` 與傳真檔案）複製過去後啟動。第一次啟動會升級資料表、把設定檔的執行設定匯入資料庫、建立 `conf/secret.key` 與管理員 `admin`（密碼在 `conf/initial-admin-password`）。之後請在 Web 介面修改設定；`conf/secret.key` 要和資料庫一起備份。防火牆記得開 TCP 8443（第 3 節）。

## 1. 安裝（RHEL 9/10，不需要安裝任何套件）
```sh
tar xzf sip-fax-rs-0.4.1-linux-x86_64.tar.gz
cd sip-fax-rs-0.4.1-linux-x86_64
cp conf/sip-faxd.toml.example conf/sip-faxd.toml
chown -R <執行服務的帳號>: .     # 解壓後的擁有者是 root，改成實際要執行服務的帳號
```

## 2. 設定

### 2.1 設定檔 `conf/sip-faxd.toml`：只有啟動設定

設定檔只放資料庫、Web、存檔目錄、log 與服務管理。預設使用內建的 SQLite，**不用改任何東西**就能啟動。要用其他資料庫時，在 `[database]` 分欄位填寫，密碼原樣填，不用編碼：

```toml
[database]
type = "mssql"
host = "10.1.2.3"
name = "faxdb"
user = "fax"
password = "p@ss:w/rd#1"
trust_server_certificate = true    # 自簽憑證時
```

檢查設定與資料庫連線：

```sh
./sip-faxd config check
./sip-faxd db check
```

### 2.2 SIP 與中繼：啟動後在 Web 介面設定

1. 啟動（第 4 節），以 `conf/initial-admin-password` 的密碼登入 `https://<伺服器>:8443/`（帳號 `admin`），第一次登入要改密碼。
2. 「設定 → SIP」：**對外位址**填 `<本機對 CS210 的 IP>:5060`（會寫進 Via、Contact 和 SDP，一定要填對）。第一次試接**建議**把「來源不在任何中繼時」改成「以 generic 處理並記錄」，這樣即使 CS210 實際發話的 IP 和預期不同，電話也會被接起來，並在 log 記錄對方的實際 IP；確認後改回「直接丟棄」。
3. 「設定 → 中繼」新增 `cht`：預設組合 `cht-cs210`、對端位址 `<CS210 的 IP>:5060`；CS210 有多個來源 IP 時填在「允許的來源 IP／CIDR」。要求註冊時，在「註冊」填入帳號和密碼。
4. 「設定 → 媒體」：T.38 埠範圍預設 `30000-30999`。

沒有任何中繼時，所有來電都會被拒絕，Web 上方與啟動 log 都會提示。SIP 監聽位址、對外位址、T.38 埠範圍、通話擷取改了要重啟（`./sip-faxd restart`），Web 會標示；其他設定下一通來電就套用。

**補充**

- 這些設定存在資料庫。`./sip-faxd config export` 可以備份或檢視（預設不含密碼；`--include-secrets` 含明文，請妥善保管）；要以腳本設定，可以照 `conf/runtime.toml.example` 寫一個檔案，以 `./sip-faxd config import --file <檔案>` 匯入。
- 從 0.2／0.3 升級、設定檔裡還有 `[sip]`、`[[trunks]]` 等區段：資料庫還沒有設定時會匯入一次，之後不再使用，啟動 log 與 `config check` 會提示可以從設定檔刪除。
- `conf/secret.key` 是加密密碼用的金鑰，**要和資料庫一起備份**；遺失的話註冊、Email、Webhook 的密碼要重新輸入。
- `./sip-faxd user list` 查看帳號，`./sip-faxd fax list --unread` 不開 Web 就能看收到的傳真。

## 3. 防火牆
- SIP：UDP 5060，只允許 CS210 的 IP 連入。
- T.38：UDP 30000–30999，也就是 `port_range` 設定的範圍。
- Web 介面：TCP 8443（HTTPS）。**預設開啟**，設定檔沒有 `[web]` 也會啟動；只開放給要使用介面的網段。不需要時在設定檔寫 `[web]` `enabled = false`。第一次啟動時密碼在 `conf/initial-admin-password`。

```sh
firewall-cmd --permanent --add-rich-rule='rule family=ipv4 source address=<CS210 IP> port port=5060 protocol=udp accept'
firewall-cmd --permanent --add-rich-rule='rule family=ipv4 source address=<CS210 IP> port port=30000-30999 protocol=udp accept'
firewall-cmd --permanent --add-rich-rule='rule family=ipv4 source address=<辦公室網段，例如 10.1.0.0/16> port port=8443 protocol=tcp accept'
firewall-cmd --reload
```
5060 是特權埠。非 root 執行時需要先給執行檔授權：
```sh
setcap 'cap_net_bind_service=+ep' ./sip-faxd
```

## 4. 啟動
```sh
./sip-faxd start        # 在背景啟動，等服務就緒後顯示 log；Ctrl+C 只停止顯示，服務繼續執行
./sip-faxd status       # 是否在執行、pid、監聽位址、進行中的通話數（沒有在執行時回傳 3）
./sip-faxd logs -f      # 之後要再看 log：logs/sip-faxd.log（自動輪替，舊檔以 gzip 壓縮）
./sip-faxd stop         # 等進行中的傳真收完再停止（最多 300 秒，期間新的來電回 503）
./sip-faxd stop --force # 要馬上停止時：立刻結束通話，已收到的頁面照樣寫出（partial）
```
- 長時間測試直接用 `start` 即可，不需要 `nohup` 或 `tmux`。`restart` 等於 `stop` 再 `start`。
- 要看更詳細的 SIP log：`RUST_LOG=info,sip_core=debug ./sip-faxd start`（環境變數會傳給背景程序）。
- **用 systemd 管理時**：unit 的 `ExecStart` 使用前景的 `serve`（`ExecStart=/opt/sip-faxd/sip-faxd serve`），並設定 `TimeoutStopSec=330`（要大於 `[service] stop_timeout_secs`，預設 300 秒，否則 systemd 會在傳真收完前強制終止）。停止請用 `systemctl stop sip-faxd`（等同 `stop`：新來電回 503、等通話結束）。這時 `sip-faxd start`／`stop`／`restart` 會提示改用 `systemctl`；`sip-faxd status` 與 `logs` 照常可用。
- 要在前景除錯：`./sip-faxd serve`，log 同時輸出到 stderr 與 `logs/sip-faxd.log`，Ctrl+C 與 `stop` 相同（再按一次 Ctrl+C 就是強制結束）。

## 5. 結果在哪裡
| 路徑 | 內容 |
|---|---|
| `data/faxes/YYYY/MM/DD/<call-id>.tif`、`.pdf` | 收到的傳真 |
| `logs/calls/<call-id>/result.json` | 結果：頁數、速率、對方 TSI、成功或失敗原因 |
| `logs/calls/<call-id>/sip.log` | 這通電話的所有 SIP 訊息原文 |
| `logs/calls/<call-id>/udptl.log` | T.38 封包摘要，含完整的 T.30 控制 frame，但不含影像內容 |
| `data/sip-faxd.db` | 每通電話一筆紀錄；不需要回傳，`logs/calls/` 的內容已足夠 |

## 6. 試接後請回傳（不論成功或失敗）
1. 整個 `logs/calls/` 目錄（壓縮後傳回）。裡面不含傳真影像，但含有雙方的電話號碼和 IP。
2. 終端機的 log 輸出。
3. 傳真是否成功、頁數是否正確、影像是否完整。可以的話，附上發送端的原稿供比對。
4. 如果完全沒有接到電話：附上 `tcpdump -i any -w cs210.pcap udp port 5060` 的擷取。

## 7. 已知限制（alpha）
- T.38 優先。CS210 拒絕 T.38 時，0.5 起改從 G.711 音訊接收（最高 9600 bit/s，不使用 ECM），`result.json` 的 `audio.reason` 記錄原因，`logs/calls/<call-id>/audio.log` 記錄音訊上的 T.30 過程。G.711 只和 SpanDSP 互通驗證過，還沒有和真實 gateway 測過；防火牆要開放整個 `port_range`（RTP 用偶數 UDPTL 埠的下一個奇數埠）。
- ECM 預設關閉。發送端要求 ECM 時，傳送會以 non-ECM 完成。ECM 只和 SpanDSP 互通驗證過，還沒有和真實 gateway 測過。
- 紀錄也可以直接用 SQLite 工具查詢 `data/sip-faxd.db`（欄位說明見壓縮檔內的 `docs/database.md`）。
- localTCF 時不會送 CFR。這是跟 SpanDSP 一致的行為，但還沒有用 CS210 驗證過，這正是這次試接要確認的項目之一。
