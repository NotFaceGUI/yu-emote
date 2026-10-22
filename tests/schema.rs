//! Sample-backed schema/texture/animation tests.
//!
//! The official sample is not distributed. Set `YURIS_EMOTE_SAMPLE` and (for
//! the encrypted body) `YURIS_EMOTE_KEY`. Set `YURIS_REQUIRE_SAMPLES=1` to
//! turn a missing sample into a failure instead of an explicit skip.

use sha2::{Digest, Sha256};
use yu_emote::anim::Animator;
use yu_emote::psb;
use yu_emote::schema::Model;
use yu_emote::texture;

fn load() -> Option<(psb::PsbDocument, Model)> {
    let path = match std::env::var("YURIS_EMOTE_SAMPLE") {
        Ok(path) => path,
        Err(_) => {
            if std::env::var("YURIS_REQUIRE_SAMPLES").as_deref() == Ok("1") {
                panic!("YURIS_REQUIRE_SAMPLES=1 but YURIS_EMOTE_SAMPLE is not set");
            }
            eprintln!("SKIP: set YURIS_EMOTE_SAMPLE to emote_test.psb");
            return None;
        }
    };
    let key = std::env::var("YURIS_EMOTE_KEY").ok().map(|raw| {
        let raw = raw.trim().to_string();
        if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
            u32::from_str_radix(hex, 16).expect("hex key")
        } else {
            raw.parse().expect("decimal key")
        }
    });
    let document = psb::read_document_with_key(&path, key).expect("parse sample");
    let model = Model::from_document(&document);
    Some((document, model))
}

#[test]
fn schema_matches_official_sample() {
    let Some((document, model)) = load() else {
        return;
    };

    assert_eq!(model.id.as_deref(), Some("motion"));
    assert_eq!(model.label.as_deref(), Some("template"));
    assert_eq!(model.spec.as_deref(), Some("win"));
    assert!((model.version.unwrap_or_default() - 3.03).abs() < 0.001);

    assert_eq!(model.screen.width, 800.0);
    assert_eq!(model.screen.height, 1080.0);
    assert_eq!(model.metadata.format.as_deref(), Some("emote"));
    assert_eq!(model.metadata.character.as_deref(), Some("all_parts"));
    assert_eq!(model.metadata.base_motion.as_deref(), Some("全体構造"));

    assert_eq!(model.sources.len(), 2);
    let tex0 = model
        .sources
        .iter()
        .find(|page| page.name == "tex#0")
        .unwrap();
    assert_eq!((tex0.width, tex0.height), (2048, 2048));
    assert_eq!(tex0.format.as_deref(), Some("RGBA4444"));
    assert_eq!(tex0.resource, Some(1));
    assert_eq!(tex0.icons.len(), 97);
    assert_eq!(tex0.truncated_width, 2046);
    assert_eq!(tex0.truncated_height, 2047);

    let tex1 = model
        .sources
        .iter()
        .find(|page| page.name == "tex#1")
        .unwrap();
    assert_eq!((tex1.width, tex1.height), (512, 256));
    assert_eq!(tex1.resource, Some(0));
    assert_eq!(tex1.icons.len(), 13);

    assert_eq!(model.objects.len(), 6);
    assert_eq!(model.stats.motions, 21);
    assert_eq!(model.stats.layers, 234);
    assert_eq!(model.stats.frames, 888);
    assert_eq!(model.variables.len(), 24);
    assert_eq!(model.timelines.len(), 11);
    assert_eq!(model.controllers.hair.len(), 4);
    assert_eq!(model.controllers.transition.len(), 17);
    assert_eq!(
        model.controllers.mouth[0].label.as_deref(),
        Some("face_mouth")
    );
    assert_eq!(document.resources, 2);
    assert_eq!(document.resource_data.len(), 2);
}

#[test]
fn typed_motion_shape() {
    let Some((_, model)) = load() else {
        return;
    };
    let motion = model.motion("all_parts", "全体構造").expect("motion");
    assert_eq!(motion.last_time, 61);
    assert_eq!(motion.layers.len(), 1);
    let root = &motion.layers[0];
    assert_eq!(root.label, "■全体レイアウト");
    assert_eq!(root.kind, 2);
    assert_eq!(root.children.len(), 1);

    let body = model
        .motion("body_parts", "下半身変形基礎")
        .expect("motion");
    assert!(body.layer_count() > 0);
}

#[test]
fn texture_decoding_is_stable() {
    let Some((document, model)) = load() else {
        return;
    };
    for page in &model.sources {
        let decoded = texture::decode_page(&document, page).expect("decode page");
        assert_eq!(
            decoded.rgba8.len(),
            decoded.width as usize * decoded.height as usize * 4
        );
        let hash = Sha256::digest(&decoded.rgba8);
        let expected = match page.name.as_str() {
            "tex#0" => "55b0d64777f44350002ee05a5835beb58484b09970d82912b32e5340b1cdc998",
            "tex#1" => "89e9dd7791072c4381d1fafd8ae26f2d955cab984e01f41246cdc728513f9c3c",
            other => panic!("unexpected page {other}"),
        };
        assert_eq!(hex(&hash), expected, "page {}", page.name);
    }
}

#[test]
fn dump_is_deterministic() {
    let Some((_, model)) = load() else {
        return;
    };
    let first = serde_json::to_string(&model).expect("json");
    let second = serde_json::to_string(&Model::from_document(
        &psb::read_document_with_key(
            std::env::var("YURIS_EMOTE_SAMPLE").unwrap(),
            Some(parse_key()),
        )
        .expect("reparse"),
    ));
    assert_eq!(second.expect("json"), first);
}

#[test]
fn timeline_playback_moves_variables() {
    let Some((_, model)) = load() else {
        return;
    };
    let mut animator = Animator::new(&model, 60.0);
    animator.play("sample_00").expect("timeline exists");
    assert!(animator.variable("head_UD").is_some());
    let start = animator.variable("head_UD").unwrap();
    assert!(animator.tick(0.5));
    let after = animator.variable("head_UD").unwrap();
    assert!(after != start, "variable should advance over 30 frames");
    assert_eq!(animator.playbacks().len(), 1);
    animator.stop("sample_00");
    assert!(animator.playbacks().is_empty());
}

fn parse_key() -> u32 {
    let raw = std::env::var("YURIS_EMOTE_KEY").expect("YURIS_EMOTE_KEY");
    let raw = raw.trim();
    if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).expect("hex key")
    } else {
        raw.parse().expect("decimal key")
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}
