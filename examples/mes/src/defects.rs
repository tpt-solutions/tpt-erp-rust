//! Structured defect-code taxonomy for shop-floor inspection.
//!
//! Inspection defects are recorded against a WIP unit using a controlled vocabulary:
//! every [`DefectCode`] belongs to a [`DefectCategory`], carries a default [`Severity`]
//! and a default [`Disposition`]. The recorded [`Severity`] (the per-finding value, which
//! may override the code default) drives the recommended [`Disposition`] and therefore the
//! rework-vs-scrap decision, as well as downstream reporting.

use serde::Serialize;
use std::fmt;
use tpt_erp_primitives::{Entity, Id};

/// A defect category groups related defect codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum DefectCategory {
    /// Geometry / tolerance problems (bore, flatness, position).
    Dimensional,
    /// Cosmetic / surface-integrity problems (scratch, pitting, coating).
    Surface,
    /// Electrical continuity / functional problems (open, short, intermittent).
    Electrical,
}

// NOTE: `DefectCode` carries `&'static str` fields, so it is `Serialize`-only; the
// catalog is a compile-time constant and findings are produced at runtime.

impl fmt::Display for DefectCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            DefectCategory::Dimensional => "Dimensional",
            DefectCategory::Surface => "Surface",
            DefectCategory::Electrical => "Electrical",
        };
        f.write_str(s)
    }
}

/// Defect severity. The worst severity among a unit's findings drives the
/// recommended disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum Severity {
    Minor,
    Major,
    Critical,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Severity::Minor => "Minor",
            Severity::Major => "Major",
            Severity::Critical => "Critical",
        };
        f.write_str(s)
    }
}

/// The engineering disposition for a defect (or set of defects).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Disposition {
    /// The unit can be brought back into spec via rework.
    Rework,
    /// The unit is beyond economical repair and must be scrapped.
    Scrap,
    /// The defect is acceptable; the unit may ship as-is.
    UseAsIs,
}

impl fmt::Display for Disposition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Disposition::Rework => "Rework",
            Disposition::Scrap => "Scrap",
            Disposition::UseAsIs => "Use-as-is",
        };
        f.write_str(s)
    }
}

/// A specific, catalogued defect code within the taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DefectCode {
    /// Stable catalog code, e.g. `DIM-001`.
    pub code: &'static str,
    /// The category this code belongs to.
    pub category: DefectCategory,
    /// Human-readable description of the defect.
    pub description: &'static str,
    /// Severity assumed when a finding does not override it.
    pub default_severity: Severity,
    /// Disposition assumed from the catalog entry (used for reporting).
    pub default_disposition: Disposition,
}

impl DefectCode {
    /// Build a recorded [`DefectFinding`] for this code.
    ///
    /// `severity` overrides the catalog default when supplied; `note` captures the
    /// inspector's free-text observation.
    pub fn finding(self, severity: Option<Severity>, note: impl Into<String>) -> DefectFinding {
        let severity = severity.unwrap_or(self.default_severity);
        let disposition = match severity {
            Severity::Critical => Disposition::Scrap,
            Severity::Major => Disposition::Rework,
            Severity::Minor => Disposition::UseAsIs,
        };
        DefectFinding {
            id: Id::new(),
            code: self,
            severity,
            disposition,
            note: note.into(),
        }
    }
}

/// The controlled defect taxonomy: every recognized shop-floor defect code.
pub const DEFECT_CODES: &[DefectCode] = &[
    DefectCode {
        code: "DIM-001",
        category: DefectCategory::Dimensional,
        description: "Out-of-tolerance bore diameter",
        default_severity: Severity::Major,
        default_disposition: Disposition::Rework,
    },
    DefectCode {
        code: "DIM-002",
        category: DefectCategory::Dimensional,
        description: "Excessive flatness deviation",
        default_severity: Severity::Minor,
        default_disposition: Disposition::UseAsIs,
    },
    DefectCode {
        code: "SUR-001",
        category: DefectCategory::Surface,
        description: "Surface scratch within limits",
        default_severity: Severity::Minor,
        default_disposition: Disposition::UseAsIs,
    },
    DefectCode {
        code: "SUR-002",
        category: DefectCategory::Surface,
        description: "Corrosion pitting",
        default_severity: Severity::Major,
        default_disposition: Disposition::Rework,
    },
    DefectCode {
        code: "ELE-001",
        category: DefectCategory::Electrical,
        description: "Open circuit on trace",
        default_severity: Severity::Critical,
        default_disposition: Disposition::Scrap,
    },
    DefectCode {
        code: "ELE-002",
        category: DefectCategory::Electrical,
        description: "Intermittent contact",
        default_severity: Severity::Major,
        default_disposition: Disposition::Rework,
    },
];

/// Look up a [`DefectCode`] by its stable catalog code.
pub fn lookup_code(code: &str) -> Option<DefectCode> {
    DEFECT_CODES.iter().find(|c| c.code == code).copied()
}

/// A recorded defect against a specific WIP unit during inspection.
#[derive(Debug, Clone, Serialize)]
pub struct DefectFinding {
    /// Strong identifier for this finding.
    pub id: Id<DefectFinding>,
    /// The catalog code that was raised.
    pub code: DefectCode,
    /// The (possibly overridden) severity of this finding.
    pub severity: Severity,
    /// The disposition implied by this finding's severity.
    pub disposition: Disposition,
    /// Inspector free-text note.
    pub note: String,
}

impl Entity for DefectFinding {}

impl DefectFinding {
    /// Convenience constructor from a catalog code string.
    pub fn from_code(
        code: &str,
        severity: Option<Severity>,
        note: impl Into<String>,
    ) -> Option<Self> {
        lookup_code(code).map(|c| c.finding(severity, note))
    }
}

/// The recommended disposition for a set of findings: the worst severity wins
/// (`Critical` → scrap, `Major` → rework, otherwise use-as-is).
pub fn recommended_disposition(findings: &[DefectFinding]) -> Disposition {
    match findings.iter().map(|f| f.severity).max() {
        Some(Severity::Critical) => Disposition::Scrap,
        Some(Severity::Major) => Disposition::Rework,
        Some(Severity::Minor) | None => Disposition::UseAsIs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taxonomy_lookup_works() {
        let code = lookup_code("ELE-001").expect("known code");
        assert_eq!(code.category, DefectCategory::Electrical);
        assert_eq!(code.default_severity, Severity::Critical);
        let finding = code.finding(None, "trace open at pin 3");
        assert_eq!(finding.severity, Severity::Critical);
        assert_eq!(finding.disposition, Disposition::Scrap);
        assert!(!finding.note.is_empty());
    }

    #[test]
    fn unknown_code_is_none() {
        assert!(lookup_code("NOPE-999").is_none());
    }

    #[test]
    fn severity_drives_disposition() {
        let minor = lookup_code("DIM-002")
            .unwrap()
            .finding(None, "slightly off");
        let major = lookup_code("SUR-002").unwrap().finding(None, "pitting");
        let crit = lookup_code("ELE-001").unwrap().finding(None, "dead");
        assert_eq!(
            recommended_disposition(std::slice::from_ref(&minor)),
            Disposition::UseAsIs
        );
        assert_eq!(
            recommended_disposition(std::slice::from_ref(&major)),
            Disposition::Rework
        );
        assert_eq!(recommended_disposition(std::slice::from_ref(&crit)), Disposition::Scrap);
        // Worst severity wins across a mix.
        assert_eq!(
            recommended_disposition(&[minor, major, crit]),
            Disposition::Scrap
        );
    }

    #[test]
    fn severity_can_be_overridden() {
        // A typically-minor code can be escalated by the inspector.
        let f = lookup_code("SUR-001")
            .unwrap()
            .finding(Some(Severity::Critical), "deep gouge");
        assert_eq!(f.severity, Severity::Critical);
        assert_eq!(f.disposition, Disposition::Scrap);
    }
}
