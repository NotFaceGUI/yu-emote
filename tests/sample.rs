//! Optional sample test against the real `emote_test.psb`.
//!
//! The sample is not distributed with this repository. Set
//! `YURIS_EMOTE_SAMPLE=/path/to/emote_test.psb` to run it. Set
//! `YURIS_EMOTE_KEY=0x...` for a body-encrypted file; without the key the test
//! verifies the explicit `EncryptedPsbRequiresKey` error instead of passing
//! silently.

use yu_emote::EmoteError;
use yu_emote::model::ModelSummary;
use yu_emote::psb;

fn sample_key() -> Option<u32> {
    let raw = std::env::var("YURIS_EMOTE_KEY").ok()?;
    let trimmed = raw.trim();
    let parsed = if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16)
    } else {
        trimmed.parse()
    };
    Some(parsed.expect("YURIS_EMOTE_KEY must be a decimal or 0x-prefixed u32"))
}

#[test]
fn emote_test_sample_parses() {
    let path = match std::env::var("YURIS_EMOTE_SAMPLE") {
        Ok(path) => path,
        Err(_) => {
            eprintln!("SKIP: set YURIS_EMOTE_SAMPLE to emote_test.psb");
            return;
        }
    };

    let key = sample_key();
    let result = psb::read_document_with_key(&path, key);
    let document = match (result, key) {
        (Ok(document), _) => document,
        (Err(EmoteError::EncryptedPsbRequiresKey), None) => {
            eprintln!("SKIP: sample body is encrypted; set YURIS_EMOTE_KEY");
            return;
        }
        (Err(error), _) => panic!("parse sample: {error}"),
    };

    let summary = ModelSummary::from_document(&document);
    eprintln!("{}", serde_json::to_string_pretty(&summary).expect("json"));

    assert_eq!(summary.version, 2, "sample is PSB v2");
    assert_eq!(document.resources, 2, "sample embeds two texture resources");
    assert!(!summary.top_level.is_empty(), "root object has keys");
    assert_eq!(summary.root_kind, "object");
    assert!(
        summary.known_sections["metadata"],
        "E-mote metadata present"
    );
}
