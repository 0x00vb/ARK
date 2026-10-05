//! An Office file is a zip of XML parts. A `Package` holds the parts in their order; a format
//! changes the parts it edits and writes the rest back byte for byte.

use std::io::{Cursor, Read, Write};

use crate::Error;

pub struct Package {
    parts: Vec<(String, Vec<u8>)>,
}

impl Package {
    pub fn read(bytes: &[u8]) -> Result<Package, Error> {
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|e| Error::Format(format!("not an Office file ({e})")))?;
        let mut parts = Vec::with_capacity(zip.len());
        for i in 0..zip.len() {
            let mut f = zip
                .by_index(i)
                .map_err(|e| Error::Format(format!("a damaged part ({e})")))?;
            if f.is_dir() {
                continue;
            }
            let mut data = Vec::with_capacity(f.size() as usize);
            f.read_to_end(&mut data)
                .map_err(|e| Error::Format(format!("a damaged part ({e})")))?;
            parts.push((f.name().to_string(), data));
        }
        Ok(Package { parts })
    }

    pub fn new() -> Package {
        Package { parts: Vec::new() }
    }

    pub fn has(&self, name: &str) -> bool {
        self.parts.iter().any(|(n, _)| n == name)
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.parts
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, d)| d.as_slice())
    }

    /// A part as text (Office XML is UTF-8).
    pub fn text(&self, name: &str) -> Result<String, Error> {
        let data = self
            .get(name)
            .ok_or_else(|| Error::Format(format!("the file has no {name}")))?;
        String::from_utf8(data.to_vec()).map_err(|_| Error::Format(format!("{name} is not UTF-8")))
    }

    pub fn text_opt(&self, name: &str) -> Option<String> {
        self.get(name)
            .and_then(|d| String::from_utf8(d.to_vec()).ok())
    }

    /// Replace a part, or add it at the end.
    pub fn set(&mut self, name: &str, data: impl Into<Vec<u8>>) {
        let data = data.into();
        match self.parts.iter_mut().find(|(n, _)| n == name) {
            Some(p) => p.1 = data,
            None => self.parts.push((name.to_string(), data)),
        }
    }

    pub fn remove(&mut self, name: &str) {
        self.parts.retain(|(n, _)| n != name);
    }

    pub fn write(&self) -> Result<Vec<u8>, Error> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        // Readers look for the content types first.
        let mut order: Vec<&(String, Vec<u8>)> = self.parts.iter().collect();
        order.sort_by_key(|(n, _)| n != "[Content_Types].xml");
        for (name, data) in order {
            zip.start_file(name.as_str(), opts)
                .map_err(|e| Error::Format(e.to_string()))?;
            zip.write_all(data)
                .map_err(|e| Error::Format(e.to_string()))?;
        }
        Ok(zip
            .finish()
            .map_err(|e| Error::Format(e.to_string()))?
            .into_inner())
    }
}

/// `target` of a relationship from part `from` (`xl/workbook.xml`), as a part name.
pub fn resolve(from: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop();
    for seg in target.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// The relationships part of `part` (`xl/_rels/workbook.xml.rels`).
pub fn rels_of(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

/// `(id, type, target)` of each relationship in a `.rels` part.
pub fn relationships(xml: &str) -> Vec<(String, String, String)> {
    use crate::xml::{attr, local, Lexer, Tok};
    Lexer::new(xml)
        .filter_map(|t| match t {
            Tok::Open { name, attrs, .. } if local(name) == "Relationship" => Some((
                attr(attrs, "Id")?.into_owned(),
                attr(attrs, "Type").unwrap_or_default().into_owned(),
                attr(attrs, "Target")?.into_owned(),
            )),
            _ => None,
        })
        .collect()
}

/// An unused `rIdN` in a `.rels` part.
pub fn next_rel_id(xml: &str) -> String {
    let used: Vec<String> = relationships(xml).into_iter().map(|r| r.0).collect();
    (1..)
        .map(|n| format!("rId{n}"))
        .find(|id| !used.contains(id))
        .unwrap_or_else(|| "rId1".into())
}

/// `xml` with `insert` placed just before its last `</tag>`.
pub fn insert_before_close(xml: &str, tag: &str, insert: &str) -> Option<String> {
    let close = format!("</{tag}>");
    let at = xml.rfind(&close)?;
    Some(format!("{}{insert}{}", &xml[..at], &xml[at..]))
}

/// Add a content-type override for a new part, unless there is one.
pub fn add_override(types: &str, part: &str, content_type: &str) -> String {
    let name = format!("/{part}");
    if types.contains(&format!("PartName=\"{name}\"")) {
        return types.to_string();
    }
    insert_before_close(
        types,
        "Types",
        &format!("<Override PartName=\"{name}\" ContentType=\"{content_type}\"/>"),
    )
    .unwrap_or_else(|| types.to_string())
}

/// Remove the content-type override of a part.
pub fn remove_override(types: &str, part: &str) -> String {
    let needle = format!("PartName=\"/{part}\"");
    let Some(at) = types.find(&needle) else {
        return types.to_string();
    };
    let start = types[..at].rfind('<').unwrap_or(at);
    let end = types[at..].find("/>").map_or(types.len(), |i| at + i + 2);
    format!("{}{}", &types[..start], &types[end..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_parts() {
        assert_eq!(
            resolve("xl/workbook.xml", "worksheets/sheet1.xml"),
            "xl/worksheets/sheet1.xml"
        );
        assert_eq!(
            resolve("xl/workbook.xml", "/xl/styles.xml"),
            "xl/styles.xml"
        );
        assert_eq!(
            resolve("xl/worksheets/sheet1.xml", "../drawings/d1.xml"),
            "xl/drawings/d1.xml"
        );
        assert_eq!(rels_of("word/document.xml"), "word/_rels/document.xml.rels");
        let mut p = Package::new();
        p.set("a.xml", "<a/>");
        p.set("[Content_Types].xml", "<Types></Types>");
        let back = Package::read(&p.write().unwrap()).unwrap();
        assert!(back.has("[Content_Types].xml") && back.has("a.xml"));
        assert_eq!(back.text("a.xml").unwrap(), "<a/>");
        let t = add_override("<Types></Types>", "x/y.xml", "t");
        assert!(t.contains("PartName=\"/x/y.xml\""));
        assert_eq!(remove_override(&t, "x/y.xml"), "<Types></Types>");
    }
}
