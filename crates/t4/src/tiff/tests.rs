use super::*;

#[test]
fn datetime_formatting() {
    assert_eq!(format_tiff_datetime(0), "1970:01:01 00:00:00");
    assert_eq!(format_tiff_datetime(951_782_400), "2000:02:29 00:00:00");
    assert_eq!(format_tiff_datetime(1_792_003_661), "2026:10:14 18:47:41");
    assert_eq!(format_tiff_datetime(4_102_444_799), "2099:12:31 23:59:59");
}
