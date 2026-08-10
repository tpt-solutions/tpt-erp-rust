//! Audit fields (created/updated bookkeeping) and the `Auditable` accessor.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The four audit columns every tenant-scoped entity carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditFields {
    /// When the row was first written.
    pub created_at: DateTime<Utc>,
    /// When the row was last mutated.
    pub updated_at: DateTime<Utc>,
    /// Who created the row (principal id / name).
    pub created_by: Option<String>,
    /// Who last mutated the row.
    pub updated_by: Option<String>,
}

impl AuditFields {
    /// Fresh audit fields stamped with `now` and `by` for both create and update.
    pub fn new(now: DateTime<Utc>, by: &str) -> Self {
        Self {
            created_at: now,
            updated_at: now,
            created_by: Some(by.to_string()),
            updated_by: Some(by.to_string()),
        }
    }
}

/// Implemented by the `TptEntity` derive when the struct carries audit columns
/// (`created_at`, `updated_at`, `created_by`, `updated_by`).
pub trait Auditable {
    /// Snapshot the current audit fields, if the entity tracks them.
    fn audit_fields(&self) -> Option<AuditFields>;
}
