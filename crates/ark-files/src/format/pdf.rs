//! PDF, read only. The agent reads its text as a document, a heading per page; the person sees
//! the pages themselves (the window's `pdf` view). ARK does not change a PDF.

use crate::model::{Block, BlockKind, Content, Document, Edit, Kind};
use crate::{Error, Format};

pub struct Pdf;

impl Format for Pdf {
    fn name(&self) -> &'static str {
        "pdf"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["pdf"]
    }
    fn kind(&self) -> Kind {
        Kind::Pdf
    }
    fn read(&self, bytes: &[u8]) -> Result<Content, Error> {
        if !bytes.starts_with(b"%PDF") {
            return Err(Error::Format("not a PDF".into()));
        }
        // The extractor can panic on a damaged file; a damaged file is a format error here.
        let pages = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem_by_pages(bytes))
            .map_err(|_| Error::Format("the PDF could not be read".into()))?
            .map_err(|e| Error::Format(e.to_string()))?;
        let mut blocks = Vec::new();
        for (i, page) in pages.iter().enumerate() {
            blocks.push(Block::new(BlockKind::Heading(6), format!("Page {}", i + 1)));
            for para in page.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
                blocks.push(Block::new(BlockKind::Paragraph, para));
            }
        }
        if pages.iter().all(|p| p.trim().is_empty()) {
            blocks.push(Block::new(
                BlockKind::Other("note".into()),
                "this PDF has no text layer (a scan or images only)",
            ));
        }
        Ok(Content::Document(Document { blocks }))
    }
    fn edit(&self, _: &[u8], _: &[Edit]) -> Result<Vec<u8>, Error> {
        Err(Error::Input(
            "ARK reads PDFs but does not change them; write a new file (a .docx or .md) instead"
                .into(),
        ))
    }
    fn create(&self, _: &Content) -> Result<Vec<u8>, Error> {
        self.edit(&[], &[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PDF with one line of text on each page, built by hand with correct offsets.
    pub(crate) fn sample(pages: &[&str]) -> Vec<u8> {
        let n = pages.len();
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
        let mut objs = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        for (i, text) in pages.iter().enumerate() {
            let stream = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
            objs.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
                5 + 2 * i
            ));
            objs.push(format!(
                "<< /Length {} >>\nstream\n{stream}\nendstream",
                stream.len()
            ));
        }
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes(),
        );
        for o in offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objs.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    #[test]
    fn a_pdf_reads_as_its_text_page_by_page_and_is_never_changed() {
        let Content::Document(doc) = Pdf.read(&sample(&["Revenue grew 12%", "Risks"])).unwrap()
        else {
            panic!()
        };
        let text: Vec<String> = doc.blocks.iter().map(|b| b.text.clone()).collect();
        assert_eq!(text, ["Page 1", "Revenue grew 12%", "Page 2", "Risks"]);
        assert!(Pdf.read(b"not a pdf").is_err());
        // A damaged file is an error, not a crash.
        let mut broken = sample(&["x"]);
        broken.truncate(60);
        assert!(Pdf.read(&broken).is_err());
        assert!(matches!(Pdf.edit(b"%PDF", &[]), Err(Error::Input(_))));
    }
}
