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
mod tests;
