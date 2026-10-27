use super::*;

#[test]
fn parses_urls() {
    let c = parse_config("mssql://sa:p%40ss%21@db.example:1444/fax?encrypt=on&trust_server_certificate=true&application_name=x").unwrap();
    assert_eq!(c.get_addr(), "db.example:1444");
    assert_eq!(
        parse_config("mssql://sa:p@db/fax").unwrap().get_addr(),
        "db:1433"
    );
    for ok in ["off", "false", "on", "true", "required", "not_supported"] {
        parse_config(&format!("mssql://sa:p@db/fax?encrypt={ok}")).unwrap();
    }
    parse_config("mssql://sa:p@db").unwrap(); // default database of the login
    for bad in [
        "mssql://sa:p@db/fax?encrypt=sometimes",
        "mssql://sa:p@db/fax?nope=1",
        "mssql:///fax",
        "not a url",
    ] {
        assert!(matches!(parse_config(bad), Err(Error::BadUrl(_))), "{bad}");
    }
    assert_eq!(percent_decode("a%20b%2Fc"), "a b/c");
}

#[test]
fn insert_rewrite_places_output_clause() {
    let sql = "INSERT INTO faxes (a, b) VALUES (?, ?)";
    assert_eq!(
        sql.replacen(") VALUES", ") OUTPUT INSERTED.id VALUES", 1),
        "INSERT INTO faxes (a, b) OUTPUT INSERTED.id VALUES (?, ?)"
    );
}
