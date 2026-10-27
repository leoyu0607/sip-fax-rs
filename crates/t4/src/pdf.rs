//! Minimal PDF writer: one page per fax page, the G4 data embedded as a `CCITTFaxDecode` image.

use crate::{Error, tiff::Page};

/// Points (1/72 inch) for `pixels` at `dpi`, floored to 1/10000 point, as text.
fn points(pixels: u32, dpi: u32) -> String {
    let scaled = u64::from(pixels) * 72 * 10_000 / u64::from(dpi);
    format!("{}.{:04}", scaled / 10_000, scaled % 10_000)
}

/// Write pages as a PDF. Page size is `width / x_dpi` by `height / y_dpi` inches.
pub fn write_pdf(pages: &[Page<'_>]) -> Result<Vec<u8>, Error> {
    if pages.is_empty() {
        return Err(Error::NoPages);
    }
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    // offsets[i] is the byte offset of object i + 1.
    let mut offsets: Vec<usize> = Vec::new();
    let n_objects = 2 + 3 * pages.len();

    let begin = |out: &mut Vec<u8>, offsets: &mut Vec<usize>, id: usize| {
        debug_assert_eq!(offsets.len() + 1, id);
        offsets.push(out.len());
        out.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
    };

    begin(&mut out, &mut offsets, 1);
    out.extend_from_slice(b"<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    begin(&mut out, &mut offsets, 2);
    let kids: Vec<String> = (0..pages.len())
        .map(|i| format!("{} 0 R", 3 + 3 * i))
        .collect();
    out.extend_from_slice(
        format!(
            "<< /Type /Pages /Count {} /Kids [{}] >>\nendobj\n",
            pages.len(),
            kids.join(" ")
        )
        .as_bytes(),
    );

    for (i, p) in pages.iter().enumerate() {
        p.validate()?;
        let (page_id, content_id, image_id) = (3 + 3 * i, 4 + 3 * i, 5 + 3 * i);
        let (w_pt, h_pt) = (points(p.width, p.x_dpi), points(p.height, p.y_dpi));

        begin(&mut out, &mut offsets, page_id);
        out.extend_from_slice(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w_pt} {h_pt}] \
                 /Resources << /ProcSet [/PDF /ImageB] /XObject << /Im0 {image_id} 0 R >> >> \
                 /Contents {content_id} 0 R >>\nendobj\n"
            )
            .as_bytes(),
        );

        let content = format!("q {w_pt} 0 0 {h_pt} 0 0 cm /Im0 Do Q\n");
        begin(&mut out, &mut offsets, content_id);
        out.extend_from_slice(
            format!(
                "<< /Length {} >>\nstream\n{content}endstream\nendobj\n",
                content.len()
            )
            .as_bytes(),
        );

        begin(&mut out, &mut offsets, image_id);
        out.extend_from_slice(
            format!(
                "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} \
                 /ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /CCITTFaxDecode \
                 /DecodeParms << /K -1 /Columns {w} /Rows {h} /BlackIs1 false >> \
                 /Length {len} >>\nstream\n",
                w = p.width,
                h = p.height,
                len = p.g4.len()
            )
            .as_bytes(),
        );
        out.extend_from_slice(p.g4);
        out.extend_from_slice(b"\nendstream\nendobj\n");
    }

    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", n_objects + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        if *off > 9_999_999_999 {
            return Err(Error::TooLarge);
        }
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            n_objects + 1
        )
        .as_bytes(),
    );
    Ok(out)
}

#[cfg(test)]
mod tests;
