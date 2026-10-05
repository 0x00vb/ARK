//! The formats ARK reads and writes. One file each; `FORMATS` in the crate root lists them.

mod csv;
mod docx;
mod image;
mod pdf;
mod text;
mod xlsx;

pub use csv::Csv;
pub use docx::Docx;
pub use image::Image;
pub use pdf::Pdf;
pub use text::{Markdown, Text};
pub use xlsx::Xlsx;

/// One sheet from CSV text.
pub fn csv_sheet(text: &str) -> crate::model::Sheet {
    csv::parse(text, csv::delimiter(text))
}
