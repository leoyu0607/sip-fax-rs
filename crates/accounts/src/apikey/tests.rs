use super::*;

#[test]
fn names_and_key_shapes() {
    assert!(valid_key_name("erp-sync.2"));
    assert!(!valid_key_name(""));
    assert!(!valid_key_name("ERP"));
    assert!(!valid_key_name(&"a".repeat(49)));
    assert!(valid_key_name(&"a".repeat(48)));
    let key = format!("{KEY_PREFIX}{}", new_token());
    assert!(looks_like_key(&key), "{key}");
    assert!(!looks_like_key(&key[1..]));
    assert!(!looks_like_key(&format!("{key}x")));
    assert!(!looks_like_key(&key.replace('_', "+")));
}

#[test]
fn a_key_may_read_and_with_write_edit_but_nothing_else() {
    let r = ApiKeyAuth {
        id: 1,
        name: "erp".into(),
        write: false,
    };
    let w = ApiKeyAuth {
        write: true,
        ..r.clone()
    };
    for cap in Capability::ALL {
        assert_eq!(r.allows(cap), cap == Capability::ViewFaxes, "{cap:?}");
        assert_eq!(
            w.allows(cap),
            matches!(cap, Capability::ViewFaxes | Capability::EditFaxMeta),
            "{cap:?}"
        );
    }
    assert_eq!(w.actor(), "api:erp");
}
