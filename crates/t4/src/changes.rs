//! Row representation as changing elements: positions (< width) where the colour flips,
//! starting from white. Even indexes are white-to-black changes, odd indexes black-to-white.

/// Changing elements of a packed row (1 = black).
pub(crate) fn from_packed(row: &[u8], width: u32, out: &mut Vec<u32>) {
    out.clear();
    let mut black = false;
    let mut x = 0u32;
    let mut i = 0usize;
    while x < width {
        let byte = row.get(i).copied().unwrap_or(0);
        if (byte == 0 && !black || byte == 0xFF && black) && x + 8 <= width {
            x += 8;
            i += 1;
            continue;
        }
        let end = (x + 8).min(width);
        let mut bit = 0u32;
        while x < end {
            let px = (byte >> (7 - bit)) & 1 == 1;
            if px != black {
                out.push(x);
                black = px;
            }
            x += 1;
            bit += 1;
        }
        i += 1;
    }
}

/// Remove pairs of equal adjacent positions (zero-length runs), keeping colour parity.
pub(crate) fn normalize(ch: &mut Vec<u32>) {
    let mut w = 0usize;
    for r in 0..ch.len() {
        let v = ch[r];
        if w > 0 && ch[w - 1] == v {
            w -= 1;
        } else {
            ch[w] = v;
            w += 1;
        }
    }
    ch.truncate(w);
}

/// Render changing elements into a packed row (`out` must be `ceil(width/8)` bytes).
pub(crate) fn to_packed(ch: &[u32], width: u32, out: &mut [u8]) {
    out.fill(0);
    let mut i = 0usize;
    while i < ch.len() {
        let start = ch[i];
        let end = ch.get(i + 1).copied().unwrap_or(width).min(width);
        set_bits(out, start, end);
        i += 2;
    }
}

fn set_bits(out: &mut [u8], start: u32, end: u32) {
    let mut x = start;
    while x < end {
        let byte = (x >> 3) as usize;
        let bit = x & 7;
        let n = (8 - bit).min(end - x);
        let mask = (0xFFu16 >> (8 - n)) as u8;
        if let Some(b) = out.get_mut(byte) {
            *b |= mask << (8 - bit - n);
        }
        x += n;
    }
}

/// Index of the first changing element right of `a0` that changes to the colour opposite to
/// the current colour `a0_black` (b1 when applied to the reference line, a1 on the coding
/// line). `hint` is a starting guess and is updated.
pub(crate) fn first_after(ch: &[u32], a0: i64, a0_black: bool, hint: &mut usize) -> usize {
    let mut j = (*hint).min(ch.len());
    while j > 0 && i64::from(ch[j - 1]) > a0 {
        j -= 1;
    }
    while j < ch.len() && i64::from(ch[j]) <= a0 {
        j += 1;
    }
    if (j & 1) != usize::from(a0_black) {
        j += 1;
    }
    *hint = j.saturating_sub(1);
    j
}

/// Changing element `j`, or `width` past the end.
pub(crate) fn at(ch: &[u32], width: u32, j: usize) -> u32 {
    ch.get(j).copied().unwrap_or(width)
}

#[cfg(test)]
mod tests {
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
}
