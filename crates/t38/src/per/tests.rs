use super::*;

#[test]
fn bits_roundtrip_and_align() {
    let mut w = BitWriter::new();
    w.write_bit(true);
    w.write_bits(0b101, 3);
    w.write_bits(0xABC, 12);
    w.write_bytes(&[9, 8]);
    let bytes = w.into_bytes();
    assert_eq!(bytes, [0b1101_1010, 0b1011_1100, 9, 8]);
    let mut r = BitReader::new(&bytes);
    assert!(r.read_bit().unwrap());
    assert_eq!(r.read_bits(3).unwrap(), 0b101);
    assert_eq!(r.read_bits(12).unwrap(), 0xABC);
    assert_eq!(r.read_bytes(2).unwrap(), [9, 8]);
    assert!(r.at_end());
}

#[test]
fn length_chunks() {
    for (n, enc) in [
        (0usize, vec![0x00u8]),
        (127, vec![0x7F]),
        (128, vec![0x80, 0x80]),
        (16383, vec![0xBF, 0xFF]),
        (16384, vec![0xC1]),
        (65536, vec![0xC4]),
    ] {
        let mut w = BitWriter::new();
        w.write_length_chunk(n);
        assert_eq!(w.into_bytes(), enc, "chunk {n}");
        let mut r = BitReader::new(&enc);
        let (got, more) = r.read_length_chunk().unwrap();
        assert_eq!((got, more), (n, n >= FRAGMENT_UNIT));
    }
}

#[test]
fn fragment_plan() {
    assert_eq!(plan_fragments(0), [0]);
    assert_eq!(plan_fragments(16383), [16383]);
    assert_eq!(plan_fragments(16384), [16384, 0]);
    assert_eq!(plan_fragments(16385), [16384, 1]);
    assert_eq!(plan_fragments(65536), [65536, 0]);
    assert_eq!(plan_fragments(65536 + 16384 + 5), [65536, 16384, 5]);
}

#[test]
fn invalid_fragment_header_and_truncation() {
    assert_eq!(
        BitReader::new(&[0xC0]).read_length_chunk(),
        Err(PerError::InvalidFragment(0xC0))
    );
    assert_eq!(
        BitReader::new(&[0xC5]).read_length_chunk(),
        Err(PerError::InvalidFragment(0xC5))
    );
    assert_eq!(
        BitReader::new(&[0x80]).read_length_chunk(),
        Err(PerError::Truncated)
    );
    assert_eq!(BitReader::new(&[]).read_bit(), Err(PerError::Truncated));
    assert_eq!(BitReader::new(&[1]).read_bytes(2), Err(PerError::Truncated));
    assert_eq!(BitReader::new(&[1]).read_bits(9), Err(PerError::Truncated));
}

#[test]
fn octet_string_fragmented_roundtrip() {
    for len in [0usize, 1, 127, 128, 16383, 16384, 16385, 70000] {
        let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        let mut w = BitWriter::new();
        write_octet_string(&mut w, &data);
        let enc = w.into_bytes();
        let mut r = BitReader::new(&enc);
        assert_eq!(read_octet_string(&mut r).unwrap(), data, "len {len}");
        assert!(r.at_end());
    }
}

#[test]
fn zero_padding_detection() {
    let r = BitReader::new(&[1, 0, 0]);
    assert!(!r.rest_is_zero_padding());
    let mut r = BitReader::new(&[1, 0, 0]);
    r.read_bytes(1).unwrap();
    assert!(r.rest_is_zero_padding());
    assert_eq!(r.consumed_bytes(), 1);
    assert_eq!(r.remaining_bytes(), 2);
}
