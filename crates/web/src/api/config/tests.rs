use super::*;
use runtime_config::FieldError;

fn fe(field: &str, code: &str) -> FieldError {
    FieldError {
        field: field.into(),
        code: code.into(),
        message: format!("{field} {code}"),
    }
}

#[test]
fn invalid_keeps_the_first_problem_of_each_field() {
    let e = update_error(UpdateError::Invalid(vec![
        fe("port_range", "reversed"),
        fe("port_range", "out_of_range"),
        fe("", "empty"),
    ]));
    assert_eq!(e.status, StatusCode::UNPROCESSABLE_ENTITY);
    let f = e.fields.unwrap();
    assert_eq!(f["port_range"]["code"], "reversed");
    assert_eq!(f[""]["code"], "empty");
    assert_eq!(f.len(), 2);
}

#[test]
fn conflict_names_the_current_version() {
    let e = update_error(UpdateError::Conflict { current: 7 });
    assert_eq!(e.status, StatusCode::CONFLICT);
    assert_eq!(e.fields.unwrap()["current_version"], 7);
}

#[test]
fn internal_details_stay_in_the_log() {
    let e = update_error(UpdateError::Internal("SELECT * FROM x failed".into()));
    assert_eq!(e.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!e.message.contains("SELECT"));
    let e = update_error(UpdateError::Unavailable("tcp 10.0.0.1:5432".into()));
    assert_eq!(e.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(!e.message.contains("10.0.0.1"));
}
