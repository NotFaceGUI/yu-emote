//! Sample-backed runtime/renderer checks.
//!
//! The sample models are not distributed with this repository. Set
//! `YURIS_EMOTE_SAMPLE` to a PSB file and, for a body-encrypted model,
//! `YURIS_EMOTE_KEY` to the per-game key. Without a sample the tests skip
//! explicitly; `YURIS_REQUIRE_SAMPLES=1` turns a skip into a failure.

use std::path::PathBuf;

use yu_emote::{EMOTE_TICKS_PER_SECOND, Emote, TimelinePlayMode, soft};

fn sample() -> Option<PathBuf> {
    match std::env::var("YURIS_EMOTE_SAMPLE") {
        Ok(path) => Some(PathBuf::from(path)),
        Err(_) => {
            if std::env::var("YURIS_REQUIRE_SAMPLES").as_deref() == Ok("1") {
                panic!("YURIS_REQUIRE_SAMPLES=1 but YURIS_EMOTE_SAMPLE is not set");
            }
            eprintln!("SKIP: set YURIS_EMOTE_SAMPLE to a PSB file");
            None
        }
    }
}

fn key() -> Option<u32> {
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
fn runtime_builds_scene_and_plan() {
    let Some(path) = sample() else {
        return;
    };
    let emote = Emote::load(&path, key(), None).expect("load");
    assert!(!emote.scene().sprites.is_empty(), "scene has no sprites");
    let plan = emote.plan([900, 1400]);
    assert!(!plan.items.is_empty(), "plan has no draw items");
    assert!(plan.triangle_count() > 0, "plan has no triangles");
    let textures = emote.textures().expect("textures");
    assert!(!textures.is_empty(), "no decoded textures");
    let view = soft::ViewTransform::fit(&plan, 20.0);
    let (image, stats) = soft::render_with_textures(&plan, &textures, view).expect("render");
    assert!(image.covered_pixels() > 1000, "frame is nearly empty");
    assert!(stats.triangles > 0);
    assert_eq!(stats.missing_textures, 0);
}

#[test]
fn runtime_advances_and_scene_changes() {
    let Some(path) = sample() else {
        return;
    };
    let mut emote = Emote::load(&path, key(), None).expect("load");
    let before = emote.scene().sprites.clone();
    for _ in 0..30 {
        emote.advance_ticks(1.0).expect("tick");
    }
    let after = emote.scene().sprites.clone();
    assert!(
        before
            .iter()
            .zip(after.iter())
            .any(|(a, b)| a.world_transform != b.world_transform || a.mesh != b.mesh),
        "scene did not change after 30 ticks"
    );
}

#[test]
fn authored_loop_keeps_playing() {
    let Some(path) = sample() else {
        return;
    };
    let mut emote = Emote::load(&path, key(), None).expect("load");
    let Some(timeline) = emote
        .timeline_names()
        .into_iter()
        .find(|name| emote.runtime.is_loop_timeline(name))
    else {
        eprintln!("SKIP: sample has no authored loop timeline");
        return;
    };
    emote
        .play_timeline(&timeline, TimelinePlayMode::LOOP)
        .expect("play");
    let frames = emote
        .runtime
        .timeline_total_frame_count(&timeline)
        .unwrap_or(EMOTE_TICKS_PER_SECOND * 10.0);
    let ticks = (frames + 10.0).ceil() as usize;
    for _ in 0..ticks {
        emote.advance_ticks(1.0).expect("tick");
        assert!(
            emote.runtime.is_timeline_playing(&timeline),
            "loop timeline `{timeline}` stopped after {ticks} ticks"
        );
    }
}
