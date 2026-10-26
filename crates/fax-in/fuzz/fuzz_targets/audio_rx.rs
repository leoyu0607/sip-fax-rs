#![no_main]
//! Feeds arbitrary audio to the G.711 engine's receivers (docs/test-plans/M5.md section 1): the
//! first octet picks the block length, then each block is one selector octet (bit 0: the packet
//! was lost) and its samples, little-endian. The engine must not panic, loop forever or grow
//! without bound.

use fax_in::audioengine::{AudioConfig, AudioEngine};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&first, mut rest)) = data.split_first() else {
        return;
    };
    let block = [80usize, 160, 240, 1][usize::from(first & 3)];
    let mut engine = AudioEngine::new(AudioConfig::default());
    if first & 4 != 0 {
        engine.start();
    }
    let mut rx = vec![0i16; block];
    let mut tx = vec![1i16; block];
    while let Some((&sel, tail)) = rest.split_first() {
        let (bytes, next) = tail.split_at((2 * block).min(tail.len()));
        rest = next;
        if sel & 1 == 1 {
            engine.process_lost(block, &mut tx);
        } else {
            rx.fill(0);
            for (s, b) in rx.iter_mut().zip(bytes.chunks_exact(2)) {
                *s = i16::from_le_bytes([b[0], b[1]]);
            }
            engine.process(&rx, &mut tx);
        }
    }
    let _ = engine.result();
    let _ = engine.take_pages();
});
