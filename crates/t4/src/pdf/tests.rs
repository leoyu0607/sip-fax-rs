use super::*;

#[test]
fn point_conversion() {
    assert_eq!(points(1728, 204), "609.8823");
    assert_eq!(points(98, 98), "72.0000");
    assert_eq!(points(196, 196), "72.0000");
}
