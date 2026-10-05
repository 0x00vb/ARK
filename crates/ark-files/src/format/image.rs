//! Images: the person sees them; the agent reads what they are (type and size), since the
//! models ARK runs on read no images today. ARK does not change an image.

use crate::model::{Content, Edit, Kind, Media};
use crate::{Error, Format};

pub struct Image;

impl Format for Image {
    fn name(&self) -> &'static str {
        "image"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["png", "jpg", "jpeg", "gif", "webp", "svg"]
    }
    fn kind(&self) -> Kind {
        Kind::Image
    }
    fn read(&self, bytes: &[u8]) -> Result<Content, Error> {
        let (media_type, size) = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            ("image/png", png_size(bytes))
        } else if bytes.starts_with(b"\xff\xd8\xff") {
            ("image/jpeg", jpeg_size(bytes))
        } else if bytes.starts_with(b"GIF8") {
            ("image/gif", gif_size(bytes))
        } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            ("image/webp", None)
        } else if is_svg(bytes) {
            ("image/svg+xml", None)
        } else {
            return Err(Error::Format("not an image ARK shows".into()));
        };
        Ok(Content::Media(Media {
            media_type: media_type.into(),
            width: size.map(|s| s.0),
            height: size.map(|s| s.1),
        }))
    }
    fn edit(&self, _: &[u8], _: &[Edit]) -> Result<Vec<u8>, Error> {
        Err(Error::Input(
            "ARK shows images but does not change them; make a new one (code.run can draw one)"
                .into(),
        ))
    }
    fn create(&self, _: &Content) -> Result<Vec<u8>, Error> {
        self.edit(&[], &[])
    }
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn png_size(b: &[u8]) -> Option<(u32, u32)> {
    // The IHDR chunk comes first: width and height after its length and name.
    (b.get(12..16)? == b"IHDR").then_some(())?;
    Some((be32(b, 16)?, be32(b, 20)?))
}

fn gif_size(b: &[u8]) -> Option<(u32, u32)> {
    let w = u16::from_le_bytes(b.get(6..8)?.try_into().ok()?);
    let h = u16::from_le_bytes(b.get(8..10)?.try_into().ok()?);
    Some((w as u32, h as u32))
}

fn jpeg_size(b: &[u8]) -> Option<(u32, u32)> {
    // Walk the segments to the first start-of-frame marker, which holds the size.
    let mut i = 2;
    while i + 9 < b.len() {
        if b[i] != 0xff {
            return None;
        }
        let marker = b[i + 1];
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            let h = u16::from_be_bytes([b[i + 5], b[i + 6]]);
            let w = u16::from_be_bytes([b[i + 7], b[i + 8]]);
            return Some((w as u32, h as u32));
        }
        i += 2 + len;
    }
    None
}

fn is_svg(b: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&b[..b.len().min(4096)]);
    head.contains("<svg")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_image_reads_as_what_it_is_and_is_never_changed() {
        // A 3x2 PNG header (IHDR only is enough to read the size).
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&3u32.to_be_bytes());
        png.extend_from_slice(&2u32.to_be_bytes());
        let Content::Media(m) = Image.read(&png).unwrap() else {
            panic!()
        };
        assert_eq!(
            (m.media_type.as_str(), m.width, m.height),
            ("image/png", Some(3), Some(2))
        );
        let svg = Image
            .read(b"<?xml version=\"1.0\"?><svg width=\"4\"></svg>")
            .unwrap();
        assert!(
            matches!(svg, Content::Media(Media { ref media_type, .. }) if media_type == "image/svg+xml")
        );
        assert!(Image.read(b"hello").is_err());
        assert!(matches!(Image.edit(&png, &[]), Err(Error::Input(_))));
    }
}
