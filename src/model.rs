//! Renderer-independent model summary.
//!
//! This layer reports what a PSB tree contains without assigning semantics to
//! keys it has not verified. It is deliberately generic: E-mote schema names
//! are listed as *known sections* (presence only), and every node is counted.

use std::collections::BTreeMap;

use emote_psb::value::{PsbValue, number::PsbNumber};
use serde::Serialize;

use crate::psb::PsbDocument;

/// Top-level section names observed in E-mote PSB data across public tooling.
///
/// Presence is reported as a boolean; this is **not** a schema claim.
pub const KNOWN_SECTIONS: [&str; 19] = [
    // Observed top-level keys in the YU-RIS `emote_test.psb` (PSB v2).
    "metadata",
    "object",
    "source",
    "screenSize",
    "easing",
    "label",
    "id",
    "spec",
    "stereovisionProfile",
    // Generic E-mote/tooling section names (presence only).
    "animation",
    "textures",
    "parts",
    "motion",
    "timeline",
    "controller",
    "mesh",
    "bones",
    "variable",
    "eye",
];

/// Counts of PSB value variants in a tree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PsbNodeCounts {
    /// `null` values.
    pub nulls: usize,
    /// Boolean values.
    pub booleans: usize,
    /// Integer values.
    pub integers: usize,
    /// Float/double values.
    pub floats: usize,
    /// String values.
    pub strings: usize,
    /// Embedded binary resource references.
    pub resources: usize,
    /// Extra resource references.
    pub extra_resources: usize,
    /// Lists.
    pub lists: usize,
    /// Objects/maps.
    pub objects: usize,
    /// PSB compiler intrinsics.
    pub compiler_intrinsics: usize,
}

/// Deterministic summary of one E-mote PSB document.
#[derive(Debug, Clone, Serialize)]
pub struct ModelSummary {
    /// PSB format version.
    pub version: u16,
    /// Header encryption flag.
    pub encrypted: bool,
    /// Embedded binary resource count.
    pub resources: usize,
    /// Extra resource count.
    pub extra_resources: usize,
    /// Kind of the root value (`object`, `list`, ...).
    pub root_kind: &'static str,
    /// Top-level keys and their value kinds, sorted by key.
    pub top_level: BTreeMap<String, &'static str>,
    /// Presence of [`KNOWN_SECTIONS`] at the root (case-insensitive).
    pub known_sections: BTreeMap<&'static str, bool>,
    /// Recursive node counts.
    pub counts: PsbNodeCounts,
    /// Maximum nesting depth (root = 0).
    pub max_depth: usize,
}

impl ModelSummary {
    /// Build a summary from a parsed document.
    pub fn from_document(document: &PsbDocument) -> Self {
        let mut counts = PsbNodeCounts::default();
        let mut max_depth = 0;
        walk(&document.root, 0, &mut counts, &mut max_depth);

        let mut top_level = BTreeMap::new();
        let mut known_sections: BTreeMap<&'static str, bool> =
            KNOWN_SECTIONS.iter().map(|name| (*name, false)).collect();
        if let PsbValue::Object(object) = &document.root {
            for (key, value) in object {
                top_level.insert(key.to_string(), kind(value));
                let lower = key.to_ascii_lowercase();
                if let Some(entry) = known_sections.get_mut(lower.as_str()) {
                    *entry = true;
                }
            }
        }

        Self {
            version: document.version,
            encrypted: document.encrypted,
            resources: document.resources,
            extra_resources: document.extra_resources,
            root_kind: kind(&document.root),
            top_level,
            known_sections,
            counts,
            max_depth,
        }
    }
}

/// Kind label used in summaries.
pub fn kind(value: &PsbValue) -> &'static str {
    match value {
        PsbValue::Null => "null",
        PsbValue::Bool(_) => "bool",
        PsbValue::Number(PsbNumber::Integer(_)) => "integer",
        PsbValue::Number(_) => "float",
        PsbValue::String(_) => "string",
        PsbValue::Resource(_) => "resource",
        PsbValue::ExtraResource(_) => "extra_resource",
        PsbValue::List(_) => "list",
        PsbValue::Object(_) => "object",
        _ => "compiler_intrinsic",
    }
}

fn walk(value: &PsbValue, depth: usize, counts: &mut PsbNodeCounts, max_depth: &mut usize) {
    *max_depth = (*max_depth).max(depth);
    match value {
        PsbValue::Null => counts.nulls += 1,
        PsbValue::Bool(_) => counts.booleans += 1,
        PsbValue::Number(PsbNumber::Integer(_)) => counts.integers += 1,
        PsbValue::Number(_) => counts.floats += 1,
        PsbValue::String(_) => counts.strings += 1,
        PsbValue::Resource(_) => counts.resources += 1,
        PsbValue::ExtraResource(_) => counts.extra_resources += 1,
        PsbValue::List(items) => {
            counts.lists += 1;
            for item in items {
                walk(item, depth + 1, counts, max_depth);
            }
        }
        PsbValue::Object(map) => {
            counts.objects += 1;
            for item in map.values() {
                walk(item, depth + 1, counts, max_depth);
            }
        }
        _ => counts.compiler_intrinsics += 1,
    }
}
