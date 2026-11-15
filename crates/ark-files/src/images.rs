//! The pictures a message carries, read from the project folder with the limits every provider
//! accepts. One reader for the chat's `send` and for a task's admission and start.

use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::{format, inside, kind_of, Content, Error, Format, Kind};

/// Pictures one message carries, the most bytes of each and of all. A provider counts a picture
/// as base64 (a third more): Anthropic refuses one over 5 MB and a request over 32 MB, so 3.5 MB
/// each and 20 MB in all stay under both. The page shrinks a larger picture before it sends it.
pub const MAX_IMAGES: usize = 8;
pub const MAX_IMAGE_BYTES: usize = 3_500_000;
pub const MAX_IMAGES_BYTES: usize = 20_000_000;

/// A picture read from the project folder.
#[derive(Clone, Debug)]
pub struct AttachedImage {
    pub path: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
    /// Lowercase hex SHA-256 of `bytes`.
    pub sha256: String,
    pub size: u64,
}

/// Read `paths` (project files). Only PNG, JPEG, GIF and WebP, which every provider takes. Each
/// file is read once, at most one byte past the limit: the length, the type and the hash are of
/// the bytes read.
pub fn images(root: &Path, paths: &[String]) -> Result<Vec<AttachedImage>, Error> {
    if paths.len() > MAX_IMAGES {
        return Err(Error::Input(format!(
            "at most {MAX_IMAGES} images go with one message"
        )));
    }
    let mut total = 0;
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let real = inside(root, path)?;
        let not_an_image = || Error::Input(format!("{path} is not a PNG, JPEG, GIF or WebP image"));
        if !matches!(kind_of(path), Kind::Image) {
            return Err(not_an_image());
        }
        let bytes = read_bounded(&real, path)?;
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(Error::Input(format!(
                "{path} is larger than {:.1} MB; make it smaller",
                MAX_IMAGE_BYTES as f64 / 1e6
            )));
        }
        total += bytes.len();
        if total > MAX_IMAGES_BYTES {
            return Err(Error::Input(format!(
                "the images are larger than {} MB together; send fewer",
                MAX_IMAGES_BYTES / 1_000_000
            )));
        }
        let media_type = match format::Image.read(&bytes)? {
            Content::Media(m) => m.media_type,
            _ => unreachable!("an image reads as media"),
        };
        if !matches!(
            media_type.as_str(),
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        ) {
            return Err(not_an_image());
        }
        out.push(AttachedImage {
            path: path.clone(),
            media_type,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            size: bytes.len() as u64,
            bytes,
        });
    }
    Ok(out)
}

fn read_bounded(real: &Path, rel: &str) -> Result<Vec<u8>, Error> {
    let file = std::fs::File::open(real).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => Error::Path(format!("there is no file {rel}")),
        _ => Error::Io(e),
    })?;
    let meta = file.metadata()?;
    if meta.is_dir() {
        return Err(Error::Path(format!("`{rel}` is a folder")));
    }
    if !meta.is_file() {
        return Err(Error::Path(format!("`{rel}` is not a file")));
    }
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1x1 PNG.
    const PIXEL: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64,
        0x60, 0xf8, 0x5f, 0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba, 0x92, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn message(result: Result<Vec<AttachedImage>, Error>) -> String {
        result.unwrap_err().to_string()
    }

    #[test]
    fn a_picture_is_read_with_its_type_size_and_hash() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("shot.png"), PIXEL).unwrap();
        let read = images(dir.path(), &strings(&["shot.png"])).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].media_type, "image/png");
        assert_eq!(read[0].size, PIXEL.len() as u64);
        assert_eq!(read[0].sha256.len(), 64);
        std::fs::write(dir.path().join("shot.png"), [PIXEL, &[0]].concat()).unwrap();
        let changed = images(dir.path(), &strings(&["shot.png"])).unwrap();
        assert_ne!(read[0].sha256, changed[0].sha256);
    }

    #[test]
    fn what_is_not_a_small_picture_inside_the_folder_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("notes.txt"), "hello").unwrap();
        std::fs::write(dir.path().join("outside.png"), PIXEL).unwrap();
        let mut big = PIXEL.to_vec();
        big.resize(MAX_IMAGE_BYTES + 1, 0);
        std::fs::write(root.join("big.png"), &big).unwrap();
        assert!(message(images(&root, &strings(&["../outside.png"])))
            .contains("outside the project folder"));
        assert!(message(images(&root, &strings(&["notes.txt"]))).contains("not a PNG"));
        assert!(message(images(&root, &strings(&["big.png"]))).contains("larger than 3.5 MB"));
        assert!(message(images(&root, &strings(&["missing.png"]))).contains("missing.png"));
        let many: Vec<String> = (0..=MAX_IMAGES).map(|i| format!("{i}.png")).collect();
        assert!(message(images(&root, &many)).contains("at most 8"));
    }

    #[test]
    fn pictures_that_fit_alone_are_refused_together() {
        let dir = tempfile::tempdir().unwrap();
        let mut part = PIXEL.to_vec();
        part.resize(3_400_000, 0);
        for i in 0..6 {
            std::fs::write(dir.path().join(format!("{i}.png")), &part).unwrap();
        }
        let names: Vec<String> = (0..6).map(|i| format!("{i}.png")).collect();
        assert!(message(images(dir.path(), &names)).contains("together"));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_pointing_outside_the_folder_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(dir.path().join("secret.png"), PIXEL).unwrap();
        std::os::unix::fs::symlink(dir.path().join("secret.png"), root.join("link.png")).unwrap();
        assert!(message(images(&root, &strings(&["link.png"]))).contains("outside the project"));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_another_file_inside_is_caught_by_the_hash() {
        let dir = tempfile::tempdir().unwrap();
        let mut other = PIXEL.to_vec();
        other.push(0);
        std::fs::write(dir.path().join("a.png"), PIXEL).unwrap();
        std::fs::write(dir.path().join("b.png"), &other).unwrap();
        std::os::unix::fs::symlink(dir.path().join("a.png"), dir.path().join("link.png")).unwrap();
        let before = images(dir.path(), &strings(&["link.png"])).unwrap();
        std::fs::remove_file(dir.path().join("link.png")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("b.png"), dir.path().join("link.png")).unwrap();
        let after = images(dir.path(), &strings(&["link.png"])).unwrap();
        assert_ne!(before[0].sha256, after[0].sha256);
        assert_ne!(before[0].size, after[0].size);
    }

    #[test]
    fn a_file_over_the_limit_is_read_only_one_byte_past_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut big = PIXEL.to_vec();
        big.resize(MAX_IMAGE_BYTES + 1_000_000, 0);
        std::fs::write(dir.path().join("big.png"), &big).unwrap();
        let bytes = read_bounded(&dir.path().join("big.png"), "big.png").unwrap();
        assert_eq!(bytes.len(), MAX_IMAGE_BYTES + 1);
    }
}
