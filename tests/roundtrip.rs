//! Synthetic PSB round-trip and summary determinism.

use std::collections::HashMap;
use std::io::Cursor;

use emote_psb::psb::write::PsbWriter;
use emote_psb::value::{PsbValue, number::PsbNumber};
use smol_str::SmolStr;
use yu_emote::model::ModelSummary;
use yu_emote::psb::read_psb;

fn object(pairs: Vec<(&str, PsbValue)>) -> PsbValue {
    let mut map = HashMap::new();
    for (key, value) in pairs {
        map.insert(SmolStr::new(key), value);
    }
    PsbValue::Object(map)
}

fn write_and_read(root: &PsbValue) -> yu_emote::psb::PsbDocument {
    let mut buffer = Cursor::new(Vec::new());
    PsbWriter::new(2, false, root, &mut buffer)
        .expect("create writer")
        .finish()
        .expect("finish writer");
    buffer.set_position(0);
    read_psb(buffer).expect("read back")
}

#[test]
fn summary_counts_and_sections_are_deterministic() {
    let root = object(vec![
        (
            "metadata",
            object(vec![
                ("name", PsbValue::String(SmolStr::new("sample"))),
                ("version", PsbValue::Number(PsbNumber::Integer(3))),
            ]),
        ),
        (
            "animation",
            PsbValue::List(vec![
                PsbValue::Number(PsbNumber::Integer(7)),
                PsbValue::Number(PsbNumber::Float(1.5)),
                PsbValue::Bool(true),
            ]),
        ),
        ("textures", PsbValue::List(Vec::new())),
    ]);

    let document = write_and_read(&root);
    assert_eq!(document.version, 2);
    assert!(!document.encrypted);
    assert_eq!(document.resources, 0);

    let summary = ModelSummary::from_document(&document);
    assert_eq!(summary.root_kind, "object");
    assert_eq!(summary.top_level.get("metadata"), Some(&"object"));
    assert_eq!(summary.top_level.get("animation"), Some(&"list"));
    assert!(summary.known_sections["metadata"]);
    assert!(summary.known_sections["animation"]);
    assert!(summary.known_sections["textures"]);
    assert!(!summary.known_sections["mesh"]);
    assert_eq!(summary.counts.objects, 2);
    assert_eq!(summary.counts.lists, 2);
    assert_eq!(summary.counts.integers, 2);
    assert_eq!(summary.counts.floats, 1);
    assert_eq!(summary.counts.booleans, 1);
    assert_eq!(summary.counts.strings, 1);
    assert!(summary.max_depth >= 2);

    // The summary must be byte-stable for the same document.
    let a = serde_json::to_string(&summary).expect("json");
    let b = serde_json::to_string(&ModelSummary::from_document(&write_and_read(&root)))
        .expect("json again");
    assert_eq!(a, b);
}

#[test]
fn root_list_and_scalar_are_reported() {
    let document = write_and_read(&PsbValue::List(vec![PsbValue::Null]));
    let summary = ModelSummary::from_document(&document);
    assert_eq!(summary.root_kind, "list");
    assert!(summary.top_level.is_empty());
    assert_eq!(summary.counts.lists, 1);
    assert_eq!(summary.counts.nulls, 1);
}
