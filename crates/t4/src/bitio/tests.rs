use super::*;

#[test]
fn writer_and_reader_agree() {
    let mut w = BitWriter::default();
    w.put(0b101, 3);
    w.put(0b0000_0001, 8);
    w.put(1, 1);
    assert_eq!(w.bit_len(), 12);
    let bytes = w.finish();
    assert_eq!(bytes, vec![0b1010_0000, 0b0011_0000]);
    let mut b = Bits::default();
    b.extend(&bytes);
    assert_eq!(b.peek(3), 0b101);
    b.pos = 3;
    assert_eq!(b.peek(8), 1);
    assert_eq!(b.avail(), 13);
    assert!(!b.rest_is_zero());
    b.pos = 12;
    assert!(b.rest_is_zero());
    b.pos = 7;
    b.compact();
    assert_eq!((b.pos, b.avail()), (7, 9));
    b.pos = 9;
    b.compact();
    assert_eq!((b.pos, b.total()), (1, 8));
}
