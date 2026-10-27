use super::*;

#[test]
fn png_names_follow_the_tiff() {
    let t = "faxes/2026/10/22/abc@gw.tif";
    assert_eq!(
        png_rel_path(t, 1, PngKind::Full),
        "faxes/2026/10/22/abc@gw-p1.png"
    );
    assert_eq!(
        png_rel_path(t, 12, PngKind::Thumb),
        "faxes/2026/10/22/abc@gw-p12-thumb.png"
    );
    assert_eq!(png_rel_path("x.tiff", 2, PngKind::Full), "x-p2.png");
    assert_eq!(png_rel_path("odd", 1, PngKind::Full), "odd-p1.png");
}

#[test]
fn only_the_pngs_of_this_tiff_match() {
    for f in ["c-p1.png", "c-p10.png", "c-p3-thumb.png"] {
        assert!(is_png_of("c", f), "{f}");
    }
    // another call of the same stem with a suffix, its own files, odd names
    for f in [
        "c-2-p1.png",
        "c.tif",
        "c.pdf",
        "c-p.png",
        "c-p1.png.part",
        "c-p1-full.png",
        "c-px.png",
        "cc-p1.png",
        "c-p1-thumb.png.part",
    ] {
        assert!(!is_png_of("c", f), "{f}");
    }
    assert!(is_png_of("c-2", "c-2-p1.png"));
}
