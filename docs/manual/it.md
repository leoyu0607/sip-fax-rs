# sip-fax IT 安裝維運手冊

適用版本：v0.4.1-alpha

sip-fax 是收傳真用的 SIP 伺服器：VoIP gateway 或電信交換機把傳真來電以 SIP 送進來，sip-fax 以 T.38 接收，存成 TIFF 與 PDF、寫入資料庫，並可投遞到資料夾、Email 或 Webhook。座席人員以 Web 介面查看。這份手冊給負責安裝與維運的人員；座席的操作請見《座席使用手冊》（`agent.md`）。

## 1. 系統需求

| 項目 | 需求 |
|---|---|
| 作業系統 | RHEL 9／10 或相容版本（Rocky、Alma），x86_64 或 aarch64 |
| 套件 | 不需要安裝任何套件：發佈的是靜態連結的單一執行檔 |
| 資料庫 | 預設內建 SQLite，不需安裝；也支援 PostgreSQL、MySQL／MariaDB、Microsoft SQL Server |
| 硬碟 | 一頁傳真的 TIFF＋PDF＋預覽圖約 100–300 KB；依每天的頁數與保存天數估算 |
| 網路 | SIP（UDP／TCP 5060）、T.38 與 RTP（UDP，預設 30000–30999，偶數與奇數埠都要開）、Web（TCP 8443） |

以 **T.38** 為主。對方不接受 T.38 時，改從 **G.711** 音訊接收（最高 9600 bit/s，不使用 ECM）。

## 2. 安裝

```sh
tar xzf sip-fax-rs-0.4.1-linux-x86_64.tar.gz -C /opt
mv /opt/sip-fax-rs-0.4.1-linux-x86_64 /opt/sip-fax
cd /opt/sip-fax
cp conf/sip-faxd.toml.example conf/sip-faxd.toml
useradd --system --home /opt/sip-fax --shell /sbin/nologin sipfax
chown -R sipfax: /opt/sip-fax
setcap 'cap_net_bind_service=+ep' ./sip-faxd     # 非 root 也能監聽 5060
```

目錄結構：

| 路徑 | 內容 |
|---|---|
| `sip-faxd` | 執行檔（伺服器、指令工具、TUI、Web 介面都在裡面） |
| `conf/sip-faxd.toml` | 設定檔 |
| `conf/secret.key` | 加密密碼用的金鑰（第一次啟動時產生，**要備份**） |
| `conf/tls/` | 自簽憑證（第一次啟動時產生） |
| `conf/initial-admin-password` | 第一個管理員的密碼（第一次登入改密碼後刪除） |
| `data/sip-faxd.db` | SQLite 資料庫 |
| `data/faxes/YYYY/MM/DD/` | 收到的傳真：`.tif`、`.pdf` 與每頁的 `.png` 預覽 |
| `logs/sip-faxd.log` | 服務 log（自動輪替） |
| `logs/calls/<call-id>/` | 每通電話的擷取：`sip.log`、`udptl.log`、`result.json` |
| `run/` | PID 檔與控制 socket |

所有相對路徑都相對於執行檔所在的目錄。

## 3. 設定

### 3.1 兩種設定

| 種類 | 內容 | 存放與修改 |
|---|---|---|
| 啟動設定 | `[database]`、`[web]`、`[storage]`、`[log]`、`[service]` | 設定檔 `conf/sip-faxd.toml`；修改後要重啟 |
| 執行設定 | SIP、媒體、通話、通話擷取、中繼、投遞路由、投遞參數、保存期限 | 存在資料庫，在 Web 介面「設定」修改 |

設定檔**只放啟動設定**。第一次啟動時，資料庫以預設值建立執行設定（沒有中繼）；之後在 Web 介面設定。

執行設定在 Web 介面修改後，**下一通來電就套用**，不必重啟；只有 SIP 監聽位址與傳輸協定、對外位址、媒體位址與埠範圍、`[call] fax_session`、重送逾時與 491 重試次數、`[capture]` 需要重啟。介面在這些欄位名稱旁標示 ⟲，儲存後列在「等待重啟」。

![設定：SIP（標示需要重啟的欄位）](images/settings-sip.png)

![設定：中繼](images/settings-trunk.png)

**以腳本設定**：照 `conf/runtime.toml.example`（說明每個選項）寫一個檔案，以 `./sip-faxd config import --file <檔案>` 匯入（資料庫已有設定時加 `--force` 取代，會記稽核，之後重啟服務）。`./sip-faxd config export` 以同樣格式輸出資料庫中的設定，可以拿一台設好的機器當範本。

**舊設定檔**：0.2／0.3 的設定檔裡若還有 `[sip]`、`[[trunks]]` 等區段，資料庫還沒有執行設定時會匯入一次；之後不再使用，啟動 log 與 `config check` 會提示可以刪除。

### 3.2 第一次的設定

1. 設定檔：預設的 SQLite 不用改；使用其他資料庫時見 3.4。檢查：

   ```sh
   ./sip-faxd config check
   ./sip-faxd db check
   ```

2. 啟動（第 6 節），以 `admin` 與 `conf/initial-admin-password` 的密碼登入 Web 介面（第 4 節）。
3. 「設定 → SIP」：**對外位址**填 `<本機對 gateway 的 IP>:5060`（寫進 Via、Contact、SDP，一定要正確）。
4. 「設定 → 中繼」新增中繼：名稱、預設組合（一般 gateway 用 `generic`，中華電信 CS210 用 `cht-cs210`）、對端位址 `<gateway 的 IP>:5060`；有多個來源 IP 時填「允許的來源 IP／CIDR」（預設只允許對端位址的 IP）。
5. 「設定 → 媒體」：T.38 埠範圍（預設 `30000-30999`，每通電話一個偶數埠）。

沒有任何中繼時，所有來電都會被拒絕；Web 上方與啟動 log 都會提示。

### 3.3 Gateway 對接要點

- **來源 IP**：只有中繼 `allowed_sources`（預設是 `peer` 的 IP）內的來源可以打進來，其他依 `[sip] unknown_source` 處理（預設 `drop` 直接丟棄）。第一次對接建議設成 `log`：任何來源都會接聽，並在 log 記下對方實際的 IP，確認後改回 `drop`。
- **T.38 建立方式**（`t38_policy`）：`auto`（預設，適合大多數 gateway：等 `initiate_after_secs` 後由我方發 T.38 re-INVITE，對方拒絕就改用 G.711）、`wait-peer`（只等對方發起 re-INVITE，`initiate_after_secs` 內沒等到就改用 G.711）、`initiate`（同 `auto`）、`audio-only`（一律用 G.711）。改用 G.711 的通話在 `result.json` 的 `audio.reason` 記錄原因，Web 傳真內容頁的「傳送方式」顯示 G.711。
- **號碼來源**（`[trunks.numbers]`）：發話號碼依序取 P-Asserted-Identity、Remote-Party-ID、From；受話號碼依序取 Request-URI、To。可移除國碼前綴（`ani_strip_prefix = "+886"`）。不確定時，先收一通，看 `logs/calls/<call-id>/sip.log` 再調整。
- **註冊**：對端要求 REGISTER 時，開啟 `[trunks.register]` 並填入帳號密碼；狀態可在 Web「系統狀態」或 `./sip-faxd status` 查看。
- **ECM**：預設關閉，與真實 gateway 驗證前請維持關閉。

詳細的試接步驟與要回傳的資料見 `docs/field-test.md`。

### 3.4 資料庫

預設是內建的 SQLite（`data/sip-faxd.db`），不用設定。使用 PostgreSQL、MySQL／MariaDB 或 SQL Server 時，先建立一個**sip-faxd 專用的空資料庫**（程式只建立資料表；不要與其他系統共用，`users`、`settings` 等表名可能相同，MySQL／MariaDB 會悄悄沿用別的系統的表），再分欄位填寫，**密碼原樣填，不用編碼**：

```toml
[database]
type = "mssql"                  # sqlite | postgres | mysql | mariadb | mssql
host = "10.1.2.3"
# port = 1433                   # 省略時：postgres 5432、mysql／mariadb 3306、mssql 1433
name = "faxdb"
user = "fax"
password = "p@ss:w/rd#1"
# password_file = "conf/db-password"   # 改從檔案讀（第一行），設定檔就不放密碼；請 chmod 600
trust_server_certificate = true        # 只有 mssql：伺服器是自簽憑證時
auto_migrate = true
```

- 也可以寫成一個 URL：`url = "postgres://user:password@host:5432/faxdb"`，這時密碼中的特殊字元要 percent-encode（`@` 寫成 `%40`）。`url` 與分欄位不能同時使用。
- **測試連線**：`./sip-faxd db check`。只連線、不修改資料庫，顯示資料庫類型、伺服器版本、資料表版本是否相符；連不上時會說明原因（主機連不到、帳密錯誤、資料庫不存在、憑證不被信任），並回傳非 0。要測試另一個資料庫：`./sip-faxd db check --url "mssql://fax@host:1433/faxdb" --password-stdin`（密碼從標準輸入讀，不用編碼）。
- 啟動時自動建立或升級資料表（`auto_migrate = true`）；設為 `false` 時要先執行 `./sip-faxd db migrate`。
- **資料庫中斷不影響收傳真**：寫入會排隊重試；仍未寫入的通話留在 `logs/calls/*/result.json`，恢復後以 `./sip-faxd db backfill` 補回。
- 換資料庫見第 11 節「換資料庫」；資料表說明見 `docs/database.md`。

## 4. Web 介面

- 預設開啟：HTTPS，`0.0.0.0:8443`。設定檔沒有 `[web]` 也會啟動；不需要時寫 `[web]` `enabled = false`。
- **憑證**：沒有設定自己的憑證時，第一次啟動在 `conf/tls/` 產生自簽憑證（有效期 825 天，到期前 30 天自動更新）。每次啟動在 log 印出 SHA-256 指紋，讓使用者比對瀏覽器的警告。要使用公司的憑證：

  ```toml
  [web]
  cert = "conf/tls/web.crt"     # PEM，含中繼憑證
  key  = "conf/tls/web.key"
  tls_names = ["fax.example.local"]   # 自簽憑證額外的名稱（使用自己的憑證時不需要）
  ```

- `scheme = "http"` 只建議用在本機或前面另有 HTTPS 反向代理的情況；以 http 監聽非 loopback 位址時，啟動 log 會警告。
- 反向代理需要支援 WebSocket（路徑 `/api/v1/events`），否則介面不會即時更新。
- **第一個管理員**：第一次啟動建立 `admin`，密碼寫在 `conf/initial-admin-password`（權限 0600；log 只提示檔案位置），第一次登入必須改密碼，改完檔案自動刪除。
- Web 介面各頁的說明見 `docs/web.md`。

## 5. 防火牆

```sh
firewall-cmd --permanent --add-rich-rule='rule family=ipv4 source address=<gateway IP> port port=5060 protocol=udp accept'
firewall-cmd --permanent --add-rich-rule='rule family=ipv4 source address=<gateway IP> port port=30000-30999 protocol=udp accept'
firewall-cmd --permanent --add-rich-rule='rule family=ipv4 source address=<辦公室網段> port port=8443 protocol=tcp accept'
firewall-cmd --reload
```

使用 TCP 的中繼另外開 TCP 5060。Web 只開放給需要使用介面的網段。

## 6. 服務管理

### 6.1 直接以指令管理

Web 介面的「系統狀態」顯示同樣的資訊：

![系統狀態](images/status.png)

```sh
./sip-faxd start          # 背景啟動，就緒後顯示 log；Ctrl+C 只停止顯示，服務繼續執行
./sip-faxd start -d       # 只啟動，不顯示 log
./sip-faxd status         # 執行中／版本／uptime／監聽位址／進行中的通話／各中繼註冊狀態
./sip-faxd status --json  # 給監控系統用；沒有在執行時回傳 3
./sip-faxd logs -f        # 看 log（跨輪替持續跟隨）；-n 200 先顯示最後 200 行
./sip-faxd stop           # 不接新來電（回 503），等進行中的傳真收完再停（最多 300 秒）
./sip-faxd stop --force   # 立即停止；已收到的頁面照樣寫出（部分完成）
./sip-faxd restart        # stop 再 start
```

### 6.2 以 systemd 管理

`/etc/systemd/system/sip-faxd.service`：

```ini
[Unit]
Description=sip-fax SIP fax server
After=network-online.target
Wants=network-online.target

[Service]
User=sipfax
Group=sipfax
WorkingDirectory=/opt/sip-fax
ExecStart=/opt/sip-fax/sip-faxd serve
AmbientCapabilities=CAP_NET_BIND_SERVICE
# 要大於 [service] stop_timeout_secs（預設 300），讓進行中的傳真收完
TimeoutStopSec=330
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

```sh
systemctl daemon-reload
systemctl enable --now sip-faxd
systemctl stop sip-faxd        # 與 sip-faxd stop 相同：等進行中的傳真收完
```

由 systemd 管理時，`sip-faxd start／stop／restart` 會提示改用 `systemctl`；`status`、`logs`、`tui` 照常可用。

### 6.3 TUI

`./sip-faxd tui` 在終端機顯示傳真、進行中的通話與服務狀態（需要服務在執行才有即時資料）。按鍵見《座席使用手冊》第 11 節。

## 7. Log 與通話擷取

- **服務 log**：`logs/sip-faxd.log`，超過 50 MB 或換日時輪替，保留 14 份並以 gzip 壓縮（`[log]` 可調整）。詳細的 SIP log：`RUST_LOG=info,sip_core=debug ./sip-faxd start`。
- **每通電話的擷取**：`logs/calls/<call-id>/`
  - `sip.log`：這通電話所有 SIP 訊息原文；
  - `udptl.log`：T.38 封包摘要（含 T.30 控制 frame，不含影像內容）；
  - `result.json`：結果（頁數、速率、對方識別、成功或失敗原因）。
- `[capture] keep = "failed"` 只保留失敗的通話；`udptl = "full"` 會把傳真影像寫進 log，只在診斷時使用並注意保護。
- 擷取目錄不會自動清除，請定期清理或納入既有的 log 管理。

## 8. 帳號

在 Web 介面「帳號」管理（按「+ 新增帳號」打開表單；每一列的「編輯」「刪除」），或使用指令：

![帳號管理](images/users.png)


```sh
./sip-faxd user list
./sip-faxd user add alice --role agent          # 互動輸入密碼；或 --password-stdin
./sip-faxd user passwd alice                     # 重設密碼（該帳號所有登入結束）
./sip-faxd user disable alice
./sip-faxd user enable alice
```

- 角色：`admin`（管理員，可改設定、帳號、重新投遞）、`agent`（座席）。
- 密碼至少 10 個字元，不能和帳號相同；連續輸錯 5 次鎖定 15 分鐘，同一來源 IP 每分鐘最多 20 次嘗試。
- 不能停用、降級或刪除最後一個管理員。
- 以指令停用帳號時，該帳號已開啟的瀏覽器頁面要到下次重新連線才會被拒絕；要立即生效請在 Web 介面停用。

## 9. 投遞

在 Web「設定 → 投遞路由」設定（或第一次啟動前寫在設定檔的 `[[routes]]`，範例見 `conf/sip-faxd.toml.example`）。

- 路由依序比對，第一條符合的路由執行它全部的投遞；條件（受話號碼、受話號碼開頭、中繼）全部成立才符合；`default` 路由放最後。
- 投遞方式：資料夾（可指定檔名範本與 PDF／TIFF）、Email（SMTP，STARTTLS／SMTPS）、Webhook（JSON POST，可加 HMAC-SHA256 簽章與附 PDF）。
- 失敗會自動重試：1、5、15、60 分鐘後，之後每 4 小時，最多 `[delivery] max_attempts` 次（預設 10），之後標為「已放棄」。管理員可在傳真內容頁按「重新投遞」，或 `./sip-faxd fax redeliver <id>`。
- 投遞至少送達一次：服務異常中斷後，同一份傳真可能送兩次（Webhook 帶紀錄 id，接收端可以去重）。

## 10. 保存期限與清整

Web「設定 → 保存期限」：

- 檔案（TIFF、PDF、預覽圖）保存 `files_days` 天（預設 90），之後刪除檔案、保留紀錄；
- 紀錄保存 `records_days` 天（預設 365）；`0` 表示永遠保存；
- 每天在 `run_at`（伺服器本地時間，預設 03:17）執行。

```sh
./sip-faxd purge --dry-run     # 看會刪除哪些
./sip-faxd purge               # 立即執行
```

## 11. 備份與還原

**要備份的檔案**

| 項目 | 說明 |
|---|---|
| 資料庫 | SQLite：`data/sip-faxd.db`（建議先停止服務，或以 `sqlite3 data/sip-faxd.db ".backup /backup/sip-faxd.db"` 線上備份）；其他資料庫用各自的備份工具 |
| `conf/secret.key` | **一定要和資料庫一起備份**。遺失時，註冊、Email、Webhook 的密碼無法解密，需要重新輸入（其他功能照常，`config check` 與 Web 會列出要重填的欄位） |
| `conf/sip-faxd.toml`、`conf/tls/` | 啟動設定與憑證 |
| `data/faxes/` | 傳真檔案 |

另外可以定期輸出設定，方便比對與在新環境重建：

```sh
./sip-faxd config export > /backup/sip-faxd-settings.toml                     # 不含密碼
./sip-faxd config export --include-secrets > /backup/sip-faxd-secrets.toml     # 含明文密碼，請妥善保管
```

### 換資料庫

例如一開始用內建的 SQLite，之後改用 MSSQL（任何兩種資料庫之間都可以）：

1. 在新的資料庫伺服器建立一個空的資料庫（程式只建立資料表，不建立資料庫）。
2. 停止服務：`./sip-faxd stop`（或 `systemctl stop sip-faxd`）。服務執行中時 `db copy` 會拒絕。
3. 先測試新的資料庫：`./sip-faxd db check --url "mssql://fax@host:1433/faxdb?trust_server_certificate=true" --password-stdin`。再複製：`./sip-faxd db copy --to "mssql://fax@host:1433/faxdb?trust_server_certificate=true" --to-password-stdin`（密碼從標準輸入讀，不用編碼），或把新資料庫寫在另一個設定檔的 `[database]`（分欄位），以 `--to-config <檔案>` 指定。來源是設定檔的 `[database] url`；目標會先建立資料表，而且必須是空的。全部資料表（傳真、投遞、設定、稽核、帳號、登入 session）原樣複製，`id` 不變，在一個交易內完成：失敗時目標不會留下任何資料；完成後逐表核對筆數。
4. 把設定檔的 `[database]` 改成新的資料庫（分欄位或 URL），**沿用同一份 `conf/secret.key`**（設定中的密碼以它加密），再啟動服務。
5. 資料庫只存傳真檔案的**路徑**（`tiff_path`、`pdf_path`，相對於 `[storage] data_dir`），檔案本身在 `data/faxes/`。只換資料庫、不換機器時檔案不用動；同時搬到另一台機器或目錄時，把 `data/faxes/` 一起複製過去，並讓 `data_dir` 指向它（相對路徑，目錄換了也有效）。原本的資料庫留著當備份即可。

**還原**：停止服務，放回上表的檔案，再啟動。只有設定、沒有資料庫時，可在新的資料庫以 `./sip-faxd config import --force --file /backup/sip-faxd-secrets.toml` 匯入（含密碼的版本；不含密碼的版本匯入後要在 Web 重新輸入密碼），再重啟。

## 12. 升級

1. `./sip-faxd stop`（或 `systemctl stop sip-faxd`），等進行中的傳真收完。
2. 備份（第 11 節）。
3. 解壓新版到新目錄，把舊目錄的 `conf/`（含 `secret.key`、`tls/`）與 `data/` 複製過去；或直接以新版的 `sip-faxd` 取代舊的執行檔（記得重新 `setcap`）。
4. 啟動。資料表會自動升級（`auto_migrate = true`）；資料庫比執行檔新時會拒絕啟動。
5. 確認 `./sip-faxd status` 與 Web 的「系統狀態」。

從 0.2.0 升級：第一次啟動會把設定檔的執行設定匯入資料庫、建立 `conf/secret.key` 與管理員 `admin`，並開始監聽 TCP 8443（記得開防火牆，或設 `[web] enabled = false`）。

## 13. 疑難排解

| 狀況 | 檢查 |
|---|---|
| 完全沒有接到電話 | `[sip] advertised`、防火牆 5060；`unknown_source = "log"` 後看 log 有沒有被丟棄的來源；`tcpdump -i any -w sip.pcap udp port 5060` |
| 接通但傳真失敗 | `logs/calls/<call-id>/result.json` 的失敗原因、`udptl.log`；防火牆是否開了 `port_range` 的 UDP |
| 對方拒絕 T.38（「對方不接受 T.38」） | 只在 `[call] fax_session = "hold"` 時出現；正式設定會改用 G.711 接收 |
| G.711 傳真失敗 | `audio.log` 看在哪一步停住（沒有 DCS、TCF 一直 FTT、頁面中斷）；`result.json` 的 `fax.rtp`（`lost_ms`、`late`）看網路是否掉封包；防火牆是否開了奇數埠 |
| 號碼不對或空白 | 看 `sip.log` 號碼在哪個 header，調整中繼的「號碼擷取」 |
| 新來電回 503 | 服務正在停止，或 `port_range` 的埠用完（同時通話數太多） |
| Web 打不開 | `status` 是否執行中、防火牆 8443、log 中 Web 的錯誤（憑證檔無法讀取、埠被占用） |
| Web 不會即時更新 | 反向代理或防火牆擋了 WebSocket（`/api/v1/events`） |
| 設定改了沒生效 | 是否為需要重啟的欄位（Web 會列出等待重啟的設定）；是否改到設定檔中已匯入資料庫的區段 |
| 「資料庫暫時無法連線」 | 檢查資料庫；收傳真不受影響，恢復後執行 `./sip-faxd db backfill` |
| 忘記管理員密碼 | `./sip-faxd user passwd admin` |
| `secret.key` 遺失 | 在 Web「設定」重新輸入被列出的密碼欄位 |

回報問題時，請附上 `./sip-faxd status`、`logs/sip-faxd.log` 相關時段，以及有問題的通話的 `logs/calls/<call-id>/` 目錄。
