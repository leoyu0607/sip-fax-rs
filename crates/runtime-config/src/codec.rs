//! The three forms of a section body and the conversions between them:
//!
//! * **stored** (database): secrets are `enc:v1:...` strings;
//! * **plain** (memory, export with `--include-secrets`): secrets are plain strings;
//! * **api** (what a client sees and sends): a secret is `{"set": true, "ref": "<path>"}` or
//!   `{"set": false}` when read; when written it is a plain string (new value),
//!   `{"set": true, "ref": ...}` (keep the stored one) or `null` / `{"set": false}` (clear).
//!
//! Secret fields: `trunk:<n>` `register.password`; `routes` the `password` of every `email` and
//! the `secret` of every `webhook` delivery.

use serde_json::{Map, Value, json};

use crate::model::Section;
use crate::secret::{PREFIX, SecretBox, SecretError};
use crate::validate::FieldError;

/// What stands in for a secret that cannot be decrypted. It is not empty, so the configuration
/// stays valid and everything else works; the registration, e-mail or webhook that uses it is
/// refused by its peer (wrong password / signature) until the secret is entered again.
pub const UNDECRYPTABLE: &str = "***undecryptable-secret***";

/// A secret that could not be decrypted when the configuration was loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretIssue {
    /// The section.
    pub section: String,
    /// The field, for example `register.password`.
    pub field: String,
    /// Why.
    pub error: SecretError,
}

type Visitor<'a> = dyn FnMut(&str, &mut Map<String, Value>, &str) + 'a;

/// Calls `f(path, object, key)` for every place of the section where a secret may be.
fn visit(section: &Section, body: &mut Value, f: &mut Visitor<'_>) {
    match section {
        Section::Trunk(_) => {
            if let Some(Value::Object(reg)) = body.get_mut("register") {
                f("register.password", reg, "password");
            }
        }
        Section::Routes => {
            let Some(Value::Array(routes)) = body.get_mut("routes") else {
                return;
            };
            for (i, r) in routes.iter_mut().enumerate() {
                let Some(Value::Array(ds)) = r.get_mut("deliveries") else {
                    continue;
                };
                for (j, d) in ds.iter_mut().enumerate() {
                    let key = match d.get("kind").and_then(Value::as_str) {
                        Some("email") => "password",
                        Some("webhook") => "secret",
                        _ => continue,
                    };
                    if let Value::Object(o) = d {
                        f(&format!("routes[{i}].deliveries[{j}].{key}"), o, key);
                    }
                }
            }
        }
        _ => {}
    }
}

fn context(section: &Section, path: &str) -> String {
    format!("{}/{path}", section.name())
}

/// The stored secret at `path` (None when the field is not set).
fn stored_at(section: &Section, stored: &Value, path: &str) -> Option<String> {
    let mut copy = stored.clone();
    let mut out = None;
    visit(section, &mut copy, &mut |p, obj, key| {
        if p == path
            && let Some(Value::String(s)) = obj.get(key)
        {
            out = Some(s.clone());
        }
    });
    out
}

/// Turns an API body into the stored form.
///
/// Returns `(stored, validation)`: `validation` is the same body with secrets as plain strings
/// (placeholders for kept ones), for the typed validation.
///
/// # Errors
/// A `{"set": true}` that refers to a secret the section does not have, or no key to encrypt.
pub fn from_api(
    section: &Section,
    body: &Value,
    old_stored: Option<&Value>,
    secrets: &SecretBox,
) -> Result<(Value, Value), Vec<FieldError>> {
    let mut stored = body.clone();
    let mut validation = body.clone();
    let mut errs = Vec::new();
    let mut plan: Vec<(String, Action)> = Vec::new();
    visit(section, &mut stored, &mut |path, obj, key| {
        let action = match obj.get(key) {
            None | Some(Value::Null) => Action::Clear,
            Some(Value::String(s)) if s.is_empty() => Action::Clear,
            Some(Value::String(s)) => Action::New(s.clone()),
            Some(Value::Object(o)) => {
                if o.get("set").and_then(Value::as_bool) == Some(true) {
                    let r = o
                        .get("ref")
                        .and_then(Value::as_str)
                        .map_or_else(|| path.to_string(), str::to_string);
                    Action::Keep(r)
                } else {
                    Action::Clear
                }
            }
            Some(_) => Action::Bad,
        };
        plan.push((path.to_string(), action));
    });
    // apply the plan to `stored` and `validation` in the same visiting order
    let mut applied: Vec<(String, Option<String>, Option<String>)> = Vec::new();
    for (path, action) in plan {
        match action {
            Action::Clear => applied.push((path, None, None)),
            Action::Bad => {
                errs.push(FieldError::new(
                    &path,
                    "wrong_type",
                    "a secret is a string, null or {\"set\": bool}",
                ));
                applied.push((path, None, None));
            }
            Action::New(s) => match secrets.encrypt(&context(section, &path), &s) {
                Ok(enc) => applied.push((path, Some(enc), Some(s))),
                Err(e) => {
                    errs.push(FieldError::new(&path, "secret_key", e.to_string()));
                    applied.push((path, None, None));
                }
            },
            Action::Keep(r) => match old_stored.and_then(|o| stored_at(section, o, &r)) {
                // re-encrypt under the context of the new place: the context is bound to the path
                Some(old) => match secrets
                    .decrypt(&context(section, &r), &old)
                    .and_then(|plain| secrets.encrypt(&context(section, &path), &plain))
                {
                    Ok(enc) => applied.push((path, Some(enc), Some("***".into()))),
                    Err(e) => {
                        errs.push(FieldError::new(
                            &path,
                            "secret_key",
                            format!("the stored secret cannot be kept: {e}; enter it again"),
                        ));
                        applied.push((path, None, None));
                    }
                },
                None => {
                    errs.push(FieldError::new(
                        &path,
                        "required",
                        "there is no stored secret to keep; enter it",
                    ));
                    applied.push((path, None, None));
                }
            },
        }
    }
    let mut it_s = applied.iter();
    visit(section, &mut stored, &mut |_, obj, key| {
        if let Some((_, enc, _)) = it_s.next() {
            match enc {
                Some(e) => {
                    obj.insert(key.into(), Value::String(e.clone()));
                }
                None => {
                    obj.remove(key);
                }
            }
        }
    });
    let mut it_v = applied.iter();
    visit(section, &mut validation, &mut |_, obj, key| {
        if let Some((_, _, plain)) = it_v.next() {
            match plain {
                Some(p) => {
                    obj.insert(key.into(), Value::String(p.clone()));
                }
                None => {
                    obj.remove(key);
                }
            }
        }
    });
    if errs.is_empty() {
        Ok((stored, validation))
    } else {
        Err(errs)
    }
}

enum Action {
    Clear,
    Bad,
    New(String),
    Keep(String),
}

/// The stored body for a client: secrets become `{"set": true, "ref": path}` / `{"set": false}`.
pub fn redact(section: &Section, stored: &Value) -> Value {
    let mut v = stored.clone();
    visit(section, &mut v, &mut |path, obj, key| {
        let set = matches!(obj.get(key), Some(Value::String(s)) if !s.is_empty());
        obj.insert(
            key.into(),
            if set {
                json!({"set": true, "ref": path})
            } else {
                json!({"set": false})
            },
        );
    });
    v
}

/// The body for the audit trail: every secret that is set becomes `"***"`.
pub fn mask_for_audit(section: &Section, stored: &Value) -> Value {
    let mut v = stored.clone();
    visit(section, &mut v, &mut |_, obj, key| {
        if matches!(obj.get(key), Some(Value::String(s)) if !s.is_empty()) {
            obj.insert(key.into(), Value::String("***".into()));
        }
    });
    v
}

/// Removes the placeholders of secrets that could not be decrypted (they are not values).
pub fn strip_placeholders(section: &Section, body: &Value) -> Value {
    let mut v = body.clone();
    visit(section, &mut v, &mut |_, obj, key| {
        if matches!(obj.get(key), Some(Value::String(s)) if s == UNDECRYPTABLE) {
            obj.remove(key);
        }
    });
    v
}

/// Removes every secret (the default export).
pub fn strip(section: &Section, body: &Value) -> Value {
    let mut v = body.clone();
    visit(section, &mut v, &mut |_, obj, key| {
        obj.remove(key);
    });
    v
}

/// Decrypts the stored body. A secret that cannot be decrypted becomes an empty string (the
/// affected registration, e-mail or webhook then fails with an authentication error instead of
/// the whole configuration being refused) and is reported.
pub fn decrypt(
    section: &Section,
    stored: &Value,
    secrets: &SecretBox,
) -> (Value, Vec<SecretIssue>) {
    let mut v = stored.clone();
    let mut issues = Vec::new();
    visit(section, &mut v, &mut |path, obj, key| {
        let Some(Value::String(s)) = obj.get(key) else {
            return;
        };
        if !s.starts_with(PREFIX) {
            return; // written by hand in plain text: accepted, encrypted at the next change
        }
        match secrets.decrypt(&context(section, path), s) {
            Ok(plain) => {
                obj.insert(key.into(), Value::String(plain));
            }
            Err(error) => {
                obj.insert(key.into(), Value::String(UNDECRYPTABLE.into()));
                issues.push(SecretIssue {
                    section: section.name(),
                    field: path.to_string(),
                    error,
                });
            }
        }
    });
    (v, issues)
}

/// Encrypts the plain body (import, first start): every non-empty secret becomes `enc:v1:...`.
///
/// # Errors
/// There is no key.
pub fn encrypt_plain(
    section: &Section,
    plain: &Value,
    secrets: &SecretBox,
) -> Result<Value, SecretError> {
    let mut v = plain.clone();
    let mut err = None;
    visit(section, &mut v, &mut |path, obj, key| {
        if let Some(Value::String(s)) = obj.get(key) {
            if s.is_empty() {
                obj.remove(key);
                return;
            }
            match secrets.encrypt(&context(section, path), s) {
                Ok(e) => {
                    obj.insert(key.into(), Value::String(e));
                }
                Err(e) => err = Some(e),
            }
        }
    });
    match err {
        Some(e) => Err(e),
        None => Ok(v),
    }
}

/// Number of stored values that are `enc:v1:` strings (to tell whether a lost key matters).
pub fn count_encrypted(section: &Section, stored: &Value) -> usize {
    let mut n = 0;
    let mut v = stored.clone();
    visit(section, &mut v, &mut |_, obj, key| {
        if matches!(obj.get(key), Some(Value::String(s)) if s.starts_with(PREFIX)) {
            n += 1;
        }
    });
    n
}
