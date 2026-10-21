#![no_main]
//! Feeds arbitrary bytes to the TIFF reader and, for every page that decodes, to the PNG
//! encoders. Nothing may panic; a decoded bitmap must have exactly the size its header says.

use libfuzzer_sys::fuzz_target;
use t4::{TiffReader, png, row_bytes};

fuzz_target!(|data: &[u8]| {
    let Ok(reader) = TiffReader::new(data) else { return };
    for i in 0..reader.page_count().min(4) {
        let info = reader.info(i).unwrap();
        // keep the per-input cost bounded: the reader allows pages up to 8192 x 16384
        if u64::from(info.width) * u64::from(info.height) > 4_000_000 {
            continue;
        }
        if let Ok(bmp) = reader.decode_page(i) {
            assert_eq!(bmp.data.len(), row_bytes(bmp.width) * bmp.height as usize);
            let _ = png::encode_full(&bmp).unwrap();
            let _ = png::encode_thumb(&bmp).unwrap();
        }
    }
});
