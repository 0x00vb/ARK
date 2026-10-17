//! Any file that is text: notes, code, JSON. Markdown is text the window shows as a page.

use crate::model::{apply_text, Content, Edit, Kind};
use crate::{Error, Format};

pub struct Text;

impl Format for Text {
    fn name(&self) -> &'static str {
        "text"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &[]
    }
    fn kind(&self) -> Kind {
        Kind::Text
    }
    fn read(&self, bytes: &[u8]) -> Result<Content, Error> {
        Ok(Content::Text(decode(bytes)?.0))
    }
    fn edit(&self, bytes: &[u8], edits: &[Edit]) -> Result<Vec<u8>, Error> {
        let (mut t, stored) = decode(bytes)?;
        apply_text(&mut t, edits)?;
        Ok(stored.encode(&t))
    }
    fn create(&self, content: &Content) -> Result<Vec<u8>, Error> {
        match content {
            Content::Text(t) => Ok(t.clone().into_bytes()),
            _ => Err(Error::Input("a text file takes `text`".into())),
        }
    }
}

/// The bytes of a text file that is not UTF-16: kept when it is written back, so a file saved
/// by Excel or Notepad on Windows does not lose its accents or its byte order mark.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Charset {
    Utf8,
    /// UTF-8 with a byte order mark (what Excel needs to read accents in a CSV).
    Utf8Bom,
    /// Windows-1252, the legacy code page of Western Windows: bytes that are not valid UTF-8.
    Windows1252,
}

const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// Windows-1252 bytes 0x80 to 0x9F as characters. The five bytes it leaves undefined map to the
/// control character of the same number, as Windows does.
const CP1252_HIGH: [char; 32] = [
    '\u{20AC}', '\u{81}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{8D}', '\u{017D}', '\u{8F}',
    '\u{90}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{9D}', '\u{017E}', '\u{0178}',
];

impl Charset {
    /// The text of `bytes` and how they were stored.
    pub(crate) fn decode(bytes: &[u8]) -> (String, Charset) {
        if let Some(rest) = bytes.strip_prefix(BOM) {
            return (String::from_utf8_lossy(rest).into_owned(), Charset::Utf8Bom);
        }
        match std::str::from_utf8(bytes) {
            Ok(t) => (t.to_string(), Charset::Utf8),
            Err(_) => {
                let text = bytes
                    .iter()
                    .map(|&b| match b {
                        0x80..=0x9F => CP1252_HIGH[(b - 0x80) as usize],
                        _ => b as char,
                    })
                    .collect();
                (text, Charset::Windows1252)
            }
        }
    }

    /// `text` stored this way. Text that Windows-1252 cannot hold (an emoji the agent added)
    /// is stored as UTF-8 with a byte order mark: the whole file converts, none of it is lost.
    pub(crate) fn encode(self, text: &str) -> Vec<u8> {
        match self {
            Charset::Utf8 => text.as_bytes().to_vec(),
            Charset::Utf8Bom => [BOM, text.as_bytes()].concat(),
            Charset::Windows1252 => text
                .chars()
                .map(|c| match c as u32 {
                    0..=0x7F | 0xA0..=0xFF => Some(c as u32 as u8),
                    _ => CP1252_HIGH
                        .iter()
                        .position(|h| *h == c)
                        .map(|i| 0x80 + i as u8),
                })
                .collect::<Option<Vec<u8>>>()
                .unwrap_or_else(|| Charset::Utf8Bom.encode(text)),
        }
    }
}

/// How a text file is stored, kept when it is written back: Windows line endings (CRLF),
/// UTF-16 (what Notepad and PowerShell 5 often save), a UTF-8 byte order mark and Windows-1252.
/// The agent always sees `\n` lines.
struct Stored {
    crlf: bool,
    /// UTF-16 with a byte order mark: little-endian or not.
    utf16_le: Option<bool>,
    charset: Charset,
}

impl Stored {
    fn encode(&self, text: &str) -> Vec<u8> {
        let text = if self.crlf {
            text.replace("\r\n", "\n").replace('\n', "\r\n")
        } else {
            text.to_string()
        };
        match self.utf16_le {
            None => self.charset.encode(&text),
            Some(le) => {
                let mut out = if le {
                    vec![0xFF, 0xFE]
                } else {
                    vec![0xFE, 0xFF]
                };
                for u in text.encode_utf16() {
                    out.extend(if le { u.to_le_bytes() } else { u.to_be_bytes() });
                }
                out
            }
        }
    }
}

/// `text` in place of the text file `old`, stored the way `old` was (CRLF, UTF-16, encoding).
pub(crate) fn rewrite(old: &[u8], text: &str) -> Vec<u8> {
    match decode(old) {
        Ok((_, stored)) => stored.encode(text),
        Err(_) => text.as_bytes().to_vec(),
    }
}

fn decode(bytes: &[u8]) -> Result<(String, Stored), Error> {
    let utf16_le = match bytes {
        [0xFF, 0xFE, ..] => Some(true),
        [0xFE, 0xFF, ..] => Some(false),
        _ => None,
    };
    let mut charset = Charset::Utf8;
    let text = match utf16_le {
        Some(le) => {
            let units: Vec<u16> = bytes[2..]
                .chunks_exact(2)
                .map(|c| {
                    if le {
                        u16::from_le_bytes([c[0], c[1]])
                    } else {
                        u16::from_be_bytes([c[0], c[1]])
                    }
                })
                .collect();
            String::from_utf16_lossy(&units)
        }
        None if bytes.contains(&0) => return Err(Error::Format("not text".into())),
        None => {
            let (text, set) = Charset::decode(bytes);
            charset = set;
            text
        }
    };
    let crlf = text.contains("\r\n");
    let text = if crlf {
        text.replace("\r\n", "\n")
    } else {
        text
    };
    Ok((
        text,
        Stored {
            crlf,
            utf16_le,
            charset,
        },
    ))
}

/// Markdown: text for the agent, a rendered page for the person.
pub struct Markdown;

impl Format for Markdown {
    fn name(&self) -> &'static str {
        "markdown"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["md", "markdown"]
    }
    fn kind(&self) -> Kind {
        Kind::Markdown
    }
    fn read(&self, bytes: &[u8]) -> Result<Content, Error> {
        Text.read(bytes)
    }
    fn edit(&self, bytes: &[u8], edits: &[Edit]) -> Result<Vec<u8>, Error> {
        Text.edit(bytes, edits)
    }
    fn create(&self, content: &Content) -> Result<Vec<u8>, Error> {
        Text.create(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replace(find: &str, replace: &str) -> Edit {
        Edit::FindReplace {
            find: find.into(),
            replace: replace.into(),
        }
    }

    /// A file saved on Windows keeps its CRLF lines, and a multi-line edit written with `\n`
    /// still finds its text.
    #[test]
    fn crlf_files_stay_crlf() {
        let out = Text
            .edit(
                b"one\r\ntwo\r\nthree\r\n",
                &[replace("one\ntwo", "uno\ndos")],
            )
            .unwrap();
        assert_eq!(out, b"uno\r\ndos\r\nthree\r\n");
        let Content::Text(t) = Text.read(b"a\r\nb\r\n").unwrap() else {
            panic!()
        };
        assert_eq!(t, "a\nb\n");
    }

    #[test]
    fn a_windows_1252_file_keeps_its_accents_and_its_encoding() {
        // "caf\xE9 \x80 \x96 se\xF1or\r\n" in Windows-1252.
        let bytes = b"caf\xE9 \x80 \x96 se\xF1or\r\n";
        let Content::Text(t) = Text.read(bytes).unwrap() else {
            panic!()
        };
        assert_eq!(t, "caf\u{e9} \u{20ac} \u{2013} se\u{f1}or\n");
        let out = Text.edit(bytes, &[replace("caf", "bar")]).unwrap();
        assert_eq!(out, b"bar\xE9 \x80 \x96 se\xF1or\r\n");
    }

    #[test]
    fn a_character_windows_1252_cannot_hold_converts_the_file_to_utf8() {
        let out = Text
            .edit(b"caf\xE9", &[replace("caf", "\u{1F600} caf")])
            .unwrap();
        assert_eq!(out, "\u{FEFF}\u{1F600} caf\u{e9}".as_bytes());
    }

    #[test]
    fn a_utf8_byte_order_mark_is_kept_and_not_shown() {
        let bytes = b"\xEF\xBB\xBFone\n";
        let Content::Text(t) = Text.read(bytes).unwrap() else {
            panic!()
        };
        assert_eq!(t, "one\n");
        assert_eq!(
            Text.edit(bytes, &[replace("one", "two")]).unwrap(),
            b"\xEF\xBB\xBFtwo\n"
        );
    }

    #[test]
    fn utf16_files_are_text_and_stay_utf16() {
        let mut bytes = vec![0xFF, 0xFE];
        for u in "hola\r\n".encode_utf16() {
            bytes.extend(u.to_le_bytes());
        }
        let Content::Text(t) = Text.read(&bytes).unwrap() else {
            panic!()
        };
        assert_eq!(t, "hola\n");
        let out = Text.edit(&bytes, &[replace("hola", "chau")]).unwrap();
        let mut want = vec![0xFF, 0xFE];
        for u in "chau\r\n".encode_utf16() {
            want.extend(u.to_le_bytes());
        }
        assert_eq!(out, want);
    }
}
