//! Full-text search over artifact contents (strings, symbols, decompilation,
//! notes) via Tantivy. This is what turns "which function references this string"
//! into a query instead of a re-run. Best-effort: if the index can't open, the
//! rest of the store keeps working and search is simply unavailable.

use std::path::Path;
use std::sync::Mutex;

use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, Schema, Value, STORED, STRING, TEXT};
use tantivy::{doc, Index, IndexWriter, TantivyDocument};

pub struct SearchIndex {
    index: Index,
    writer: Mutex<IndexWriter>,
    f: Fields,
}

struct Fields {
    project_id: Field,
    artifact_id: Field,
    name: Field,
    kind: Field,
    body: Field,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Hit {
    pub artifact_id: String,
    pub name: String,
    pub kind: String,
    pub score: f32,
}

impl SearchIndex {
    pub fn open(dir: &Path) -> tantivy::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let mut sb = Schema::builder();
        let project_id = sb.add_text_field("project_id", STRING | STORED);
        let artifact_id = sb.add_text_field("artifact_id", STRING | STORED);
        let name = sb.add_text_field("name", TEXT | STORED);
        let kind = sb.add_text_field("kind", STRING | STORED);
        let body = sb.add_text_field("body", TEXT);
        let schema = sb.build();

        let dir = tantivy::directory::MmapDirectory::open(dir)?;
        let index = Index::open_or_create(dir, schema)?;
        let writer = index.writer(15_000_000)?;
        Ok(SearchIndex {
            index,
            writer: Mutex::new(writer),
            f: Fields {
                project_id,
                artifact_id,
                name,
                kind,
                body,
            },
        })
    }

    /// Index one text document. Commits immediately (volumes are small).
    pub fn add(
        &self,
        project_id: &str,
        artifact_id: &str,
        name: &str,
        kind: &str,
        body: &str,
    ) -> tantivy::Result<()> {
        let mut w = self.writer.lock().unwrap();
        w.add_document(doc!(
            self.f.project_id => project_id,
            self.f.artifact_id => artifact_id,
            self.f.name => name,
            self.f.kind => kind,
            self.f.body => body,
        ))?;
        w.commit()?;
        Ok(())
    }

    /// Search within a project. Matches on body and name; results are filtered to
    /// the given project.
    pub fn search(&self, project_id: &str, query: &str, limit: usize) -> tantivy::Result<Vec<Hit>> {
        let reader = self.index.reader()?;
        let searcher = reader.searcher();
        let mut qp = QueryParser::for_index(&self.index, vec![self.f.body, self.f.name]);
        qp.set_conjunction_by_default();
        let parsed = match qp.parse_query(query) {
            Ok(q) => q,
            Err(_) => return Ok(Vec::new()),
        };

        let top = searcher.search(&parsed, &TopDocs::with_limit(limit * 4))?;
        let mut hits = Vec::new();
        for (score, addr) in top {
            let d: TantivyDocument = searcher.doc(addr)?;
            let get = |field: Field| {
                d.get_first(field)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            if get(self.f.project_id) != project_id {
                continue;
            }
            hits.push(Hit {
                artifact_id: get(self.f.artifact_id),
                name: get(self.f.name),
                kind: get(self.f.kind),
                score,
            });
            if hits.len() >= limit {
                break;
            }
        }
        Ok(hits)
    }
}
