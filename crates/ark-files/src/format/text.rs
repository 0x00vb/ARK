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

/// How a text file is stored, kept when it is written back: Windows line endings (CRLF) and
/// UTF-16 (what Notepad and PowerShell 5 often save). The agent always sees `\n` lines.
struct Stored {
    crlf: bool,
    /// UTF-16 with a byte order mark: little-endian or not.
    utf16_le: Option<bool>,
}

impl Stored {
    fn encode(&self, text: &str) -> Vec<u8> {
        let text = if self.crlf {
            text.replace("\r\n", "\n").replace('\n', "\r\n")
        } else {
            text.to_string()
        };
        match self.utf16_le {
            None => text.into_bytes(),
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

/// `text` in place of the text file `old`, stored the way `old` was (CRLF, UTF-16).
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
        None => String::from_utf8_lossy(bytes).into_owned(),
    };
    let crlf = text.contains("\r\n");
    let text = if crlf {
        text.replace("\r\n", "\n")
    } else {
        text
    };
    Ok((text, Stored { crlf, utf16_le }))
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
