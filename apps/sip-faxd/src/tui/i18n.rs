//! The strings of the TUI in one table (English, Traditional Chinese). The language follows
//! `LANG`: a value starting with `zh_TW` selects Chinese, anything else English. The table is
//! plain Rust so the web interface's server side can share it later.

/// The interface language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// English (the default).
    En,
    /// Traditional Chinese.
    ZhTw,
}

impl Lang {
    /// Picks the language for the value of `LANG` (`None` when it is unset).
    pub fn from_lang_var(value: Option<&str>) -> Lang {
        match value {
            Some(v) if v.starts_with("zh_TW") => Lang::ZhTw,
            _ => Lang::En,
        }
    }

    /// The language of the environment this process runs in.
    pub fn from_env() -> Lang {
        Lang::from_lang_var(std::env::var("LANG").ok().as_deref())
    }
}

/// Every string the screens show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    TabFaxes,
    TabCalls,
    TabStatus,
    ServiceRunning,
    ServiceStopping,
    ServiceNotRunning,
    DbUnavailable,
    ColId,
    ColStarted,
    ColTrunk,
    ColAni,
    ColDnis,
    ColStatus,
    ColPages,
    ColState,
    ColAnswered,
    ColT38,
    Read,
    Unread,
    UnreadOnly,
    NoFaxes,
    NoCalls,
    DetailTitle,
    FCallId,
    FEnded,
    FDuration,
    FHangup,
    FRemoteIdent,
    FFiles,
    FPurged,
    FRead,
    FNote,
    FDeliveries,
    NoDeliveries,
    NoteEditTitle,
    FooterList,
    FooterDetail,
    FooterCalls,
    FooterStatus,
    FooterNote,
    MsgMarkedRead,
    MsgMarkedUnread,
    MsgNoteSaved,
    MsgNoteConflict,
    MsgNoSelection,
    MsgError,
    SPid,
    SVersion,
    SStarted,
    SUptime,
    SListen,
    SCalls,
    STrunks,
    Nothing,
    Yes,
    No,
}

/// The text of `key` in `lang`.
pub fn t(lang: Lang, key: Key) -> &'static str {
    let (en, zh) = pair(key);
    match lang {
        Lang::En => en,
        Lang::ZhTw => zh,
    }
}

fn pair(key: Key) -> (&'static str, &'static str) {
    use Key::*;
    match key {
        TabFaxes => ("Faxes", "傳真"),
        TabCalls => ("Calls", "通話"),
        TabStatus => ("Status", "狀態"),
        ServiceRunning => ("service running", "服務執行中"),
        ServiceStopping => ("service stopping", "服務停止中"),
        ServiceNotRunning => ("service not running", "服務未執行"),
        DbUnavailable => ("database not reachable", "資料庫無法連線"),
        ColId => ("ID", "編號"),
        ColStarted => ("STARTED (UTC)", "開始時間 (UTC)"),
        ColTrunk => ("TRUNK", "中繼線"),
        ColAni => ("ANI", "來電號碼"),
        ColDnis => ("DNIS", "被叫號碼"),
        ColStatus => ("STATUS", "結果"),
        ColPages => ("PG", "頁"),
        ColState => ("STATE", "已讀"),
        ColAnswered => ("ANSWERED", "已接聽"),
        ColT38 => ("T.38", "T.38"),
        Read => ("read", "已讀"),
        Unread => ("UNREAD", "未讀"),
        UnreadOnly => ("unread only", "只看未讀"),
        NoFaxes => ("no faxes", "沒有傳真"),
        NoCalls => ("no calls in progress", "目前沒有通話"),
        DetailTitle => ("Fax", "傳真"),
        FCallId => ("call id", "Call-ID"),
        FEnded => ("ended", "結束時間"),
        FDuration => ("duration", "通話長度"),
        FHangup => ("hangup", "掛斷原因"),
        FRemoteIdent => ("remote ident", "對方識別"),
        FFiles => ("files", "檔案"),
        FPurged => ("purged", "已清除"),
        FRead => ("read", "已讀狀態"),
        FNote => ("note", "備註"),
        FDeliveries => ("deliveries", "投遞"),
        NoDeliveries => ("none", "無"),
        NoteEditTitle => ("Edit note", "編輯備註"),
        FooterList => (
            "Tab pane  j/k move  Enter details  u unread filter  r toggle read  n note  q quit",
            "Tab 切換  j/k 移動  Enter 詳細  u 只看未讀  r 切換已讀  n 備註  q 離開",
        ),
        FooterDetail => (
            "Esc back  r toggle read  n note  q quit",
            "Esc 返回  r 切換已讀  n 備註  q 離開",
        ),
        FooterCalls => ("Tab pane  j/k move  q quit", "Tab 切換  j/k 移動  q 離開"),
        FooterStatus => ("Tab pane  q quit", "Tab 切換  q 離開"),
        FooterNote => ("Enter save  Esc cancel", "Enter 儲存  Esc 取消"),
        MsgMarkedRead => ("marked read", "已標為已讀"),
        MsgMarkedUnread => ("marked unread", "已標為未讀"),
        MsgNoteSaved => ("note saved", "備註已儲存"),
        MsgNoteConflict => (
            "the note was changed by someone else; reloaded, not saved",
            "備註已被其他人修改，已重新載入，未儲存",
        ),
        MsgNoSelection => ("no fax selected", "尚未選取傳真"),
        MsgError => ("error", "錯誤"),
        SPid => ("PID", "程序編號"),
        SVersion => ("version", "版本"),
        SStarted => ("started", "啟動時間"),
        SUptime => ("uptime", "已執行"),
        SListen => ("listen", "監聽"),
        SCalls => ("calls", "通話數"),
        STrunks => ("trunks", "中繼線"),
        Nothing => ("none", "無"),
        Yes => ("yes", "是"),
        No => ("no", "否"),
    }
}

#[cfg(test)]
mod tests;
