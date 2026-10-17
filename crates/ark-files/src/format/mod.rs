//! The formats ARK reads and writes. One file each; `FORMATS` in the crate root lists them.

mod csv;
mod docx;
mod geojson;
mod image;
mod pdf;
mod text;
mod xlsx;

pub use csv::Csv;
pub use docx::Docx;
pub use geojson::GeoJson;
pub use image::Image;
pub use pdf::Pdf;
pub(crate) use text::{rewrite, Charset};
pub use text::{Markdown, Text};
pub use xlsx::Xlsx;

/// `new` (UTF-8 text) stored the way the file `old` was: its Windows-1252 or byte order mark.
pub(crate) fn keep_charset(old: &[u8], new: Vec<u8>) -> Vec<u8> {
    let (_, charset) = Charset::decode(old);
    match String::from_utf8(new) {
        Ok(text) => charset.encode(&text),
        Err(e) => e.into_bytes(),
    }
}

/// One sheet from CSV text.
pub fn csv_sheet(text: &str) -> crate::model::Sheet {
    csv::parse(text, csv::delimiter(text))
}
