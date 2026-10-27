use super::*;

#[test]
fn call_id_extraction() {
    let m = b"INVITE sip:a@b SIP/2.0\r\nVia: x\r\ni: abc@host\r\n\r\nCall-ID: not-this\r\n";
    assert_eq!(call_id_of(m).as_deref(), Some("abc@host"));
    let m = b"SIP/2.0 200 OK\r\ncall-id :  X-1@h \r\nCSeq: 1 BYE\r\n\r\n";
    assert_eq!(call_id_of(m).as_deref(), Some("X-1@h"));
    assert_eq!(call_id_of(b"garbage"), None);
    assert_eq!(call_id_of(b"\r\n\r\n"), None);
}

#[test]
fn directory_names_are_safe() {
    assert_eq!(dir_name("abc@10.0.0.1"), "abc@10.0.0.1");
    assert_eq!(dir_name("../../etc/passwd"), ".._.._etc_passwd");
    assert_eq!(dir_name(".."), "_");
    assert_eq!(dir_name(""), "_");
    assert_eq!(dir_name(&"x".repeat(300)).len(), 100);
    assert!(!dir_name("a/b\\c").contains(['/', '\\']));
}
