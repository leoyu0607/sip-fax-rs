use super::*;

#[test]
fn pack_unpack_round_trip() {
    for width in [1u32, 7, 8, 9, 16, 17, 100] {
        let rb = (width as usize).div_ceil(8);
        for seed in 0..50u32 {
            let mut row = vec![0u8; rb];
            for x in 0..width {
                if (x.wrapping_mul(seed.wrapping_mul(2654435761) | 1) >> 3) & 1 == 1 {
                    row[(x >> 3) as usize] |= 0x80 >> (x & 7);
                }
            }
            let mut ch = Vec::new();
            from_packed(&row, width, &mut ch);
            let mut back = vec![0u8; rb];
            to_packed(&ch, width, &mut back);
            assert_eq!(row, back, "width {width} seed {seed}");
        }
    }
}

#[test]
fn normalize_removes_zero_length_runs() {
    let mut v = vec![3, 3, 5, 9, 9, 9, 12];
    normalize(&mut v);
    assert_eq!(v, vec![5, 9, 12]);
}

#[test]
fn first_after_respects_colour_parity() {
    let ch = [2u32, 5, 8, 11];
    let mut h = 0;
    // white a0 = -1: first change to black is index 0
    assert_eq!(first_after(&ch, -1, false, &mut h), 0);
    // black at a0 = 2: next change to white is index 1 (position 5)
    assert_eq!(first_after(&ch, 2, true, &mut h), 1);
    // white at a0 = 5: next change to black is index 2
    assert_eq!(first_after(&ch, 5, false, &mut h), 2);
    // beyond the end
    assert_eq!(at(&ch, 20, first_after(&ch, 11, true, &mut h)), 20);
    assert_eq!(at(&ch, 20, 99), 20);
}
