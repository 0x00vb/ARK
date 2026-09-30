//! Bridge the adapter `BlobResolver` interface to the `wb-store` blob store. This
//! wrapper lives here (not in wb-adapters or wb-store) so neither of those crates
//! depends on the other — the engine is where they meet.

use std::path::PathBuf;
use ark_tools::{AdapterError, BlobResolver};
use ark_core::ArtifactId;
use ark_store::Store;

pub struct StoreBlobs<'a>(pub &'a Store);

impl<'a> BlobResolver for StoreBlobs<'a> {
    fn path(&self, id: &ArtifactId) -> Option<PathBuf> {
        let p = self.0.blob_path(id);
        p.exists().then_some(p)
    }

    fn read(&self, id: &ArtifactId) -> Result<Vec<u8>, AdapterError> {
        self.0
            .read_blob(id)
            .map_err(|e| AdapterError::ArtifactNotFound(format!("{id}: {e}")))
    }
}
