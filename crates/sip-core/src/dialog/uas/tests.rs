use super::*;

#[test]
fn schedule_is_500_1000_2000_4000_4000() {
    let mut s = RetransmitSchedule::new(&TimerConfig::default());
    let v: Vec<u128> = (0..6).map(|_| s.next_delay().as_millis()).collect();
    assert_eq!(v, vec![500, 1000, 2000, 4000, 4000, 4000]);
}
