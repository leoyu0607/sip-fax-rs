//! U02 (backend half): the list of every code the front end has to translate. The list is
//! written to `web-ui/src/lib/api-codes.json`; the vitest `codes.test.ts` checks that both
//! locales have a text for each entry. Here the list is compared with the file (set
//! `UPDATE_API_CODES=1` to rewrite it) and with the source: every literal code found in the
//! places that build an API error is in the list, so a new code cannot be forgotten.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use store::{DeliveryStatus, FaxStatus};

/// `error.<code>`: `{"error": {"code"}}` of the API (HTTP status in the comment).
const ERROR_CODES: &[&str] = &[
    "bad_request",               // 400
    "unauthenticated",           // 401
    "invalid_credentials",       // 401
    "session_expired",           // 401
    "account_disabled",          // 401
    "csrf",                      // 403
    "forbidden",                 // 403
    "origin",                    // 403 (WebSocket)
    "password_change_required",  // 403
    "wrong_password",            // 403
    "not_found",                 // 404
    "method_not_allowed",        // 405
    "timeout",                   // 408
    "conflict",                  // 409
    "username_taken",            // 409
    "last_admin",                // 409
    "purged",                    // 410
    "too_large",                 // 413
    "unsupported_media_type",    // 415
    "invalid",                   // 422 (settings; `fields` have their own codes)
    "unprocessable",             // 422 (the framework)
    "too_long",                  // 422
    "bad_username",              // 422
    "bad_role",                  // 422
    "bad_locale",                // 422
    "password_too_short",        // 422
    "password_too_long",         // 422
    "password_same_as_username", // 422
    "account_locked",            // 423
    "rate_limited",              // 429
    "internal",                  // 500
    "unavailable",               // 503
];

/// `validation.<code>`: the `code` of an entry of `fields` of a 422 `invalid`
/// (`runtime_config::validate::FieldError`).
const VALIDATION_CODES: &[&str] = &[
    "required",
    "invalid",
    "wrong_type",
    "unknown_field",
    "out_of_range",
    "reversed",
    "empty",
    "too_long",
    "duplicate",
    "not_last",
    "unknown_preset",
    "mismatch",
    "removed",
    "secret_key",
];

/// Adding a variant fails to compile here.
fn fax_statuses() -> Vec<&'static str> {
    let all = [
        FaxStatus::Receiving,
        FaxStatus::Received,
        FaxStatus::Partial,
        FaxStatus::Failed,
        FaxStatus::NoFax,
    ];
    for s in all {
        match s {
            FaxStatus::Receiving
            | FaxStatus::Received
            | FaxStatus::Partial
            | FaxStatus::Failed
            | FaxStatus::NoFax => {}
        }
    }
    all.iter().map(FaxStatus::as_str).collect()
}

fn delivery_statuses() -> Vec<&'static str> {
    let all = [
        DeliveryStatus::Pending,
        DeliveryStatus::Done,
        DeliveryStatus::Failed,
        DeliveryStatus::GivingUp,
    ];
    for s in all {
        match s {
            DeliveryStatus::Pending
            | DeliveryStatus::Done
            | DeliveryStatus::Failed
            | DeliveryStatus::GivingUp => {}
        }
    }
    all.iter().map(DeliveryStatus::as_str).collect()
}

fn sorted(v: impl IntoIterator<Item = impl Into<String>>) -> Vec<String> {
    let set: BTreeSet<String> = v.into_iter().map(Into::into).collect();
    set.into_iter().collect()
}

fn the_list() -> Value {
    json!({
        "error": sorted(ERROR_CODES.iter().copied()),
        "validation": sorted(VALIDATION_CODES.iter().copied()),
        "hangup_code": sorted(fax_in::HangupCode::ALL.iter().map(|c| c.as_str())),
        "fax_status": sorted(fax_statuses()),
        "delivery_status": sorted(delivery_statuses()),
    })
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn sources(crate_dir: &str) -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    rs_files(
        &root().join("crates").join(crate_dir).join("src"),
        &mut files,
    );
    files
        .into_iter()
        .map(|p| {
            let s = std::fs::read_to_string(&p).unwrap();
            (p, s)
        })
        .collect()
}

/// Index just after the string literal that starts at `i` (a `"`).
fn skip_string(b: &[u8], mut i: usize) -> usize {
    i += 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    i
}

/// The string literal that is the second argument of every `call(` in `src`.
fn second_literals(src: &str, call: &str) -> Vec<String> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = src[from..].find(call) {
        let mut i = from + at + call.len();
        from = i;
        let mut depth = 0i32;
        // the first argument: up to the comma at depth 0
        while i < b.len() {
            match b[i] {
                b'"' => {
                    i = skip_string(b, i);
                    continue;
                }
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                b',' if depth == 0 => break,
                _ => {}
            }
            i += 1;
        }
        if i >= b.len() || b[i] != b',' {
            continue;
        }
        i += 1;
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < b.len() && b[i] == b'"' {
            let end = skip_string(b, i);
            out.push(src[i + 1..end - 1].to_string());
        }
    }
    out
}

/// The literals that are the value of a block (`{ "code" }`) in the statements that start with
/// `keyword` (`let code = if ... { "a" } else { "b" };`): the codes a validator picks by `if`.
fn block_literals_after(src: &str, keyword: &str) -> Vec<String> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = src[from..].find(keyword) {
        let mut i = from + at + keyword.len();
        from = i;
        let mut depth = 0i32;
        let mut prev_sig = b' ';
        while i < b.len() {
            match b[i] {
                b'"' => {
                    let end = skip_string(b, i);
                    let lit = &src[i + 1..end - 1];
                    if prev_sig == b'{'
                        && !lit.is_empty()
                        && lit.bytes().all(|c| c.is_ascii_lowercase() || c == b'_')
                    {
                        out.push(lit.to_string());
                    }
                    prev_sig = b'"';
                    i = end;
                    continue;
                }
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b';' if depth <= 0 => break,
                _ => {}
            }
            if !b[i].is_ascii_whitespace() {
                prev_sig = b[i];
            }
            i += 1;
        }
    }
    out
}

/// The `=> "literal"` arms of every `fn <name>(` body in `src`.
fn arms_of_fn(src: &str, name: &str) -> Vec<String> {
    let pat = format!("fn {name}(");
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = src[from..].find(&pat) {
        let start = from + at;
        from = start + pat.len();
        let Some(open) = src[start..].find('{') else {
            continue;
        };
        let mut depth = 0;
        let mut end = start + open;
        for (k, ch) in src[start + open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = start + open + k;
                        break;
                    }
                }
                _ => {}
            }
        }
        for line in src[start + open..end].lines() {
            if let Some((_, rhs)) = line.split_once("=>") {
                let rhs = rhs.trim().trim_end_matches(',');
                if let Some(lit) = rhs.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
                    out.push(lit.to_string());
                }
            }
        }
    }
    out
}

#[test]
fn u02_the_list_of_codes_is_the_file_the_front_end_checks() {
    let list = the_list();
    let path = root().join("web-ui/src/lib/api-codes.json");
    let text = serde_json::to_string_pretty(&list).unwrap() + "\n";
    if std::env::var("UPDATE_API_CODES").is_ok_and(|v| v == "1") {
        std::fs::write(&path, &text).unwrap();
    }
    let have = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (run with UPDATE_API_CODES=1)", path.display()));
    assert_eq!(
        serde_json::from_str::<Value>(&have).unwrap(),
        list,
        "web-ui/src/lib/api-codes.json is stale: run this test with UPDATE_API_CODES=1"
    );
    // the lists are not empty and have no duplicates
    for (k, v) in list.as_object().unwrap() {
        assert!(!v.as_array().unwrap().is_empty(), "{k}");
    }
    assert_eq!(
        ERROR_CODES.len(),
        list["error"].as_array().unwrap().len(),
        "ERROR_CODES has a duplicate"
    );
    assert_eq!(
        VALIDATION_CODES.len(),
        list["validation"].as_array().unwrap().len()
    );
    // statuses and hangup codes: the values are what the database stores
    assert_eq!(list["fax_status"].as_array().unwrap().len(), 5);
    assert_eq!(list["hangup_code"].as_array().unwrap().len(), 13);
    assert_eq!(list["delivery_status"].as_array().unwrap().len(), 4);
}

#[test]
fn u02_every_code_in_the_source_is_in_the_list() {
    let mut found_errors = BTreeSet::new();
    let mut found_fields = BTreeSet::new();
    for (_, s) in sources("web") {
        found_errors.extend(second_literals(&s, "ApiError::new("));
        found_errors.extend(arms_of_fn(&s, "code_of"));
    }
    for (_, s) in sources("accounts") {
        // LoginError, SessionError, AccountError, PasswordError
        found_errors.extend(arms_of_fn(&s, "code"));
    }
    for (_, s) in sources("runtime-config") {
        found_fields.extend(second_literals(&s, "FieldError::new("));
        // codes chosen by an if-chain before the call (`let code = if .. { "reversed" } ..`)
        found_fields.extend(block_literals_after(&s, "let code ="));
    }
    // the scanner finds what it should: a few codes that must be there
    for c in [
        "csrf",
        "bad_locale",
        "unauthenticated",
        "last_admin",
        "password_too_short",
    ] {
        assert!(
            found_errors.contains(c),
            "the scan missed {c}: {found_errors:?}"
        );
    }
    for c in [
        "out_of_range",
        "required",
        "empty",
        "reversed",
        "not_last",
        "unknown_preset",
    ] {
        assert!(
            found_fields.contains(c),
            "the scan missed {c}: {found_fields:?}"
        );
    }
    let known_errors: BTreeSet<&str> = ERROR_CODES.iter().copied().collect();
    let known_fields: BTreeSet<&str> = VALIDATION_CODES.iter().copied().collect();
    let missing: Vec<_> = found_errors
        .iter()
        // `error` is the catch-all of `error::code_of` for a status nothing maps: the
        // front end shows `error.unknown` for it
        .filter(|c| c.as_str() != "error" && !known_errors.contains(c.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "error codes not in ERROR_CODES: {missing:?}"
    );
    let missing: Vec<_> = found_fields
        .iter()
        .filter(|c| !known_fields.contains(c.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "field codes not in VALIDATION_CODES: {missing:?}"
    );
}
