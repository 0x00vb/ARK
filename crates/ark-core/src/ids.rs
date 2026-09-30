//! Identifier types. Artifacts are content-addressed; everything else gets a UUID.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Content address of an artifact: the blake3 hash of its bytes, hex-encoded.
///
/// The same bytes always produce the same id, so ingesting an object twice stores
/// it once. This is the anchor for reproducibility — an invocation references the
/// exact bytes it ran against.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ArtifactId(pub String);

impl ArtifactId {
    /// Hash raw bytes into a content address.
    pub fn of_bytes(bytes: &[u8]) -> Self {
        ArtifactId(blake3::hash(bytes).to_hex().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Short form for display (first 12 hex chars), like a git short hash.
    pub fn short(&self) -> &str {
        &self.0[..self.0.len().min(12)]
    }
}

impl fmt::Display for ArtifactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Debug for ArtifactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "artifact:{}", self.short())
    }
}

macro_rules! uuid_id {
    ($name:ident, $prefix:literal) => {
        #[doc = concat!("Unique id for a ", $prefix, ".")]
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub struct $name(pub String);

        impl $name {
            pub fn new() -> Self {
                $name(uuid::Uuid::new_v4().to_string())
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
            pub fn short(&self) -> &str {
                &self.0[..self.0.len().min(8)]
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}:{}", $prefix, self.short())
            }
        }
    };
}

uuid_id!(ProjectId, "project");
uuid_id!(InvocationId, "invocation");
uuid_id!(SessionId, "session");
uuid_id!(NoteId, "note");
