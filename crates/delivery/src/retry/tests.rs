use super::*;

#[test]
fn d05_backoff_table() {
    let got: Vec<i64> = (1..=7).map(|n| backoff(n).num_minutes()).collect();
    assert_eq!(got, vec![1, 5, 15, 60, 240, 240, 240]);
    assert_eq!(backoff(0).num_minutes(), 1);
}
