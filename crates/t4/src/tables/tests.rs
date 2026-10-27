use super::*;

fn all_codes(term: &[&str], makeup: &[&str]) -> Vec<String> {
    term.iter()
        .chain(makeup)
        .chain(EXT_MAKEUP.iter())
        .map(|s| (*s).to_string())
        .collect()
}

#[test]
fn tables_are_prefix_free_and_complete() {
    for (term, makeup) in [
        (&WHITE_TERM[..], &WHITE_MAKEUP[..]),
        (&BLACK_TERM[..], &BLACK_MAKEUP[..]),
    ] {
        let codes = all_codes(term, makeup);
        assert_eq!(codes.len(), 64 + 27 + 13);
        for (i, a) in codes.iter().enumerate() {
            for (j, b) in codes.iter().enumerate() {
                if i != j {
                    assert!(!b.starts_with(a.as_str()), "{a} is a prefix of {b}");
                }
            }
        }
        // Kraft sum must not exceed 1 (and the EOL prefix of 11 zeros must be unused).
        let kraft: f64 = codes.iter().map(|c| 0.5f64.powi(c.len() as i32)).sum();
        assert!(kraft <= 1.0, "kraft {kraft}");
        assert!(codes.iter().all(|c| !c.contains("00000000000")));
    }
}

#[test]
fn well_known_codes() {
    let t = &*TABLES;
    assert_eq!(
        t.white.terminating(0),
        Code {
            value: 0b00110101,
            bits: 8
        }
    );
    assert_eq!(
        t.black.terminating(0),
        Code {
            value: 0b0000110111,
            bits: 10
        }
    );
    assert_eq!(
        t.white.makeup(1728),
        Code {
            value: 0b010011011,
            bits: 9
        }
    );
    assert_eq!(
        t.black.makeup(2560),
        Code {
            value: 0b000000011111,
            bits: 12
        }
    );
    // Lookup round trip for every code.
    for black in [false, true] {
        let ct = t.color(black);
        for run in 0..64u32 {
            let c = ct.terminating(run);
            let idx = usize::from(c.value) << (MAX_CODE_BITS - u32::from(c.bits));
            assert_eq!(
                (ct.lookup[idx].run, ct.lookup[idx].bits),
                (run as u16, c.bits)
            );
        }
        for run in (64..=2560).step_by(64) {
            let c = ct.makeup(run);
            let idx = usize::from(c.value) << (MAX_CODE_BITS - u32::from(c.bits));
            assert_eq!(
                (ct.lookup[idx].run, ct.lookup[idx].bits),
                (run as u16, c.bits)
            );
        }
    }
}
