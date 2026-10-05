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
        if bytes.contains(&0) {
            return Err(Error::Format("not text".into()));
        }
        Ok(Content::Text(String::from_utf8_lossy(bytes).into_owned()))
    }
    fn edit(&self, bytes: &[u8], edits: &[Edit]) -> Result<Vec<u8>, Error> {
        let Content::Text(mut t) = self.read(bytes)? else {
            unreachable!()
        };
        apply_text(&mut t, edits)?;
        Ok(t.into_bytes())
    }
    fn create(&self, content: &Content) -> Result<Vec<u8>, Error> {
        match content {
            Content::Text(t) => Ok(t.clone().into_bytes()),
            _ => Err(Error::Input("a text file takes `text`".into())),
        }
    }
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
