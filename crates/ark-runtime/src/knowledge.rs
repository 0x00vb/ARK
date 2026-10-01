//! The external-knowledge seam. ARK ships no knowledge base. A consumer that wants
//! the agent to consult external references (docs, a corpus, a vector store) implements
//! [`KnowledgeProvider`] and registers it on the agent profile. When one is present, the
//! runtime exposes a `search_knowledge` meta-tool and instructs the agent to cite results
//! separately from observations about the target.

/// One retrieved knowledge snippet, already cited.
#[derive(Clone, Debug)]
pub struct KnowledgeHit {
    /// A citation label the agent should reproduce, e.g. `[kb:rfc9110]`.
    pub citation: String,
    /// The snippet text.
    pub snippet: String,
}

/// A source of external, public knowledge — separate from the project's own evidence. Kept
/// deliberately synchronous and simple; an implementation may wrap any backend.
pub trait KnowledgeProvider: Send + Sync {
    /// Return up to `limit` snippets relevant to `query`, most relevant first.
    fn search(&self, query: &str, limit: usize) -> Vec<KnowledgeHit>;
}
