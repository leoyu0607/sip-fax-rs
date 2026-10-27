use super::*;

#[test]
fn m13_language_follows_the_lang_variable() {
    assert_eq!(Lang::from_lang_var(Some("zh_TW.UTF-8")), Lang::ZhTw);
    assert_eq!(Lang::from_lang_var(Some("zh_TW")), Lang::ZhTw);
    // reverse: every other value, and no value, is English
    assert_eq!(Lang::from_lang_var(Some("en_US.UTF-8")), Lang::En);
    assert_eq!(Lang::from_lang_var(Some("zh_CN.UTF-8")), Lang::En);
    assert_eq!(Lang::from_lang_var(Some("")), Lang::En);
    assert_eq!(Lang::from_lang_var(None), Lang::En);
}

#[test]
fn m13_both_languages_have_a_text_for_every_key_and_they_differ() {
    for key in [
        Key::TabFaxes,
        Key::ServiceNotRunning,
        Key::FooterList,
        Key::MsgNoteConflict,
        Key::NoDeliveries,
    ] {
        let (en, zh) = (t(Lang::En, key), t(Lang::ZhTw, key));
        assert!(!en.is_empty() && !zh.is_empty());
        assert_ne!(en, zh, "{key:?}");
    }
}
