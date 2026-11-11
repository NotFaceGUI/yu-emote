//! Official-shaped player facade tests.
//!
//! Model-backed cases skip explicitly when `YURIS_EMOTE_SAMPLE` is not set;
//! `YURIS_REQUIRE_SAMPLES=1` turns a skip into a failure. Transform math and
//! the support ledger run without a model.

use std::path::PathBuf;

use yu_emote::player::official_api_support;
use yu_emote::{Capability, EmotePlayer, EmotePlayerOptions, OFFICIAL_INTERFACE_VERSION, soft};

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

fn player() -> Option<EmotePlayer> {
    let path = sample()?;
    Some(
        EmotePlayer::load(
            &path,
            key(),
            None,
            EmotePlayerOptions {
                canvas: [400, 600],
                ..EmotePlayerOptions::default()
            },
        )
        .expect("load player"),
    )
}

#[test]
fn transform_composition_matches_direct_application() {
    let center = [13.0, -7.0];
    let coord = [4.0, 9.0];
    let player_view = soft::ViewTransform::player(center, coord, 1.4, 0.63);
    let outer = soft::ViewTransform {
        scale: 0.75,
        rotation: 0.21,
        offset: [120.0, 45.0],
    };
    let composed = player_view.compose(outer);
    for point in [[0.0, 0.0], [20.0, -30.0], [-5.5, 12.25]] {
        let direct = outer.apply(player_view.apply(point));
        let got = composed.apply(point);
        for axis in 0..2 {
            assert!(
                (direct[axis] - got[axis]).abs() < 1e-3,
                "axis {axis}: direct={} composed={}",
                direct[axis],
                got[axis]
            );
        }
    }
}

#[test]
fn player_transform_rotates_around_center_then_translates() {
    let center = [10.0, 20.0];
    let coord = [-3.0, 7.0];
    let view = soft::ViewTransform::player(center, coord, 2.0, std::f32::consts::FRAC_PI_2);
    let got = view.apply(center);
    assert!((got[0] - coord[0]).abs() < 1e-5);
    assert!((got[1] - coord[1]).abs() < 1e-5);

    // A point one unit right of center rotates to one unit below center.
    let got = view.apply([center[0] + 1.0, center[1]]);
    assert!((got[0] - coord[0]).abs() < 1e-5);
    assert!((got[1] - (coord[1] + 2.0)).abs() < 1e-5);
}

#[test]
fn official_support_ledger_is_consistent() {
    let support = official_api_support();
    assert_eq!(support.version, OFFICIAL_INTERFACE_VERSION);
    assert!(!support.supported.is_empty());
    let mut seen = std::collections::BTreeSet::new();
    for method in support
        .supported
        .iter()
        .chain(support.state_only)
        .chain(support.unsupported)
        .chain(support.not_applicable)
    {
        assert!(seen.insert(*method), "duplicate ledger entry: {method}");
    }
}

#[test]
fn player_visibility_controls_the_draw_plan() {
    let Some(mut player) = player() else {
        return;
    };
    assert_eq!(player.capability(), Capability::DataOnly);
    assert!(!player.has_renderer());
    assert!(!player.is_hidden());
    assert!(!player.plan().items.is_empty(), "visible plan is empty");
    let (image, stats) = player.render_cpu().expect("cpu render");
    assert!(image.covered_pixels() > 1000, "visible frame is empty");
    assert!(stats.triangles > 0);

    player.hide();
    assert!(player.is_hidden());
    assert!(player.is_modified(), "hide did not request a redraw");
    player.clear_modified();
    assert!(!player.is_modified());
    assert!(player.plan().items.is_empty(), "hidden plan has items");
    let (image, stats) = player.render_cpu().expect("cpu render");
    assert_eq!(image.covered_pixels(), 0);
    assert_eq!(stats.items, 0);

    player.show();
    assert!(!player.is_hidden());
    assert!(player.is_modified(), "show did not request a redraw");
}

#[test]
fn default_player_camera_centers_the_model() {
    let Some(player) = player() else {
        return;
    };
    let plan = player.plan();
    let Some((min, max)) = plan.bounds() else {
        eprintln!("SKIP: player has no measurable bounds");
        return;
    };
    let center = [(min[0] + max[0]) * 0.5, (min[1] + max[1]) * 0.5];
    let got = player.effective_view().apply(center);
    assert!((got[0] - 200.0).abs() < 0.5, "x={}", got[0]);
    assert!((got[1] - 300.0).abs() < 0.5, "y={}", got[1]);
}

#[test]
fn player_coord_honours_transform_order_mask() {
    let Some(mut player) = player() else {
        return;
    };
    player.set_canvas(400, 600);
    player.set_view(Some(soft::ViewTransform::identity()));
    let Some((min, max)) = player.plan().bounds() else {
        eprintln!("SKIP: player has no measurable bounds");
        return;
    };
    let center = [(min[0] + max[0]) * 0.5, (min[1] + max[1]) * 0.5];

    player.set_scale(2.0);
    player.set_coord(10.0, 20.0);
    // The SDK default includes POSITION_TRANSLATE_TO_SCALE: coord is folded
    // into the player scale before the shader adds it.
    assert_ne!(
        player.transform_order_mask()
            & yu_emote::eluna::transform_order_mask::POSITION_TRANSLATE_TO_SCALE,
        0
    );
    let got = player.effective_view().apply(center);
    assert!((got[0] - 20.0).abs() < 1e-3, "x={}", got[0]);
    assert!((got[1] - 40.0).abs() < 1e-3, "y={}", got[1]);

    player
        .set_transform_order_mask(
            yu_emote::eluna::transform_order_mask::POSITION_SCALE_TO_TRANSLATE,
        )
        .expect("set mask");
    let got = player.effective_view().apply(center);
    assert!((got[0] - 10.0).abs() < 1e-3, "x={}", got[0]);
    assert!((got[1] - 20.0).abs() < 1e-3, "y={}", got[1]);
}

#[test]
fn player_variables_round_trip_by_official_index() {
    let Some(mut player) = player() else {
        return;
    };
    let count = player.variable_count();
    assert!(count > 0, "model exposes no variables");
    let label = player
        .variable_label_at(count - 1)
        .expect("last variable label")
        .to_owned();
    assert!(player.variable_frame_count_at(count - 1).is_some());

    player.set_variable(&label, 7.0).expect("set variable");
    let value = player.get_variable(&label).expect("get variable");
    assert_eq!(value, 7.0);
    player
        .set_variable_timed(&label, 9.0, 0.0, 0.0)
        .expect("zero-time set is immediate");
    assert_eq!(player.get_variable(&label), Some(9.0));
    assert_eq!(player.get_variable("__missing__"), None);
}

#[test]
fn batch_variable_writes_rebuild_once() {
    let Some(mut player) = player() else {
        return;
    };
    let labels: Vec<String> = (0..2)
        .filter_map(|index| player.variable_label_at(index).map(str::to_owned))
        .collect();
    assert_eq!(labels.len(), 2);
    player
        .set_variables(labels.iter().map(|label| (label.as_str(), 5.0)))
        .expect("batch set");
    for label in &labels {
        assert_eq!(player.get_variable(label), Some(5.0), "label {label}");
    }
}

#[test]
fn timed_player_transform_reaches_target_over_ticks() {
    let Some(mut player) = player() else {
        return;
    };
    player.set_coord(0.0, 0.0);
    player.set_coord_timed(40.0, -20.0, 30.0, 0.0);
    assert!(player.is_animating());
    player.progress(15.0).expect("half progress");
    let mid = player.get_coord();
    assert!(mid[0] > 0.0 && mid[0] < 40.0);
    assert!(mid[1] < 0.0 && mid[1] > -20.0);
    player.progress(15.0).expect("finish progress");
    let end = player.get_coord();
    assert!((end[0] - 40.0).abs() < 1e-4);
    assert!((end[1] + 20.0).abs() < 1e-4);

    player.set_scale(2.0);
    assert_eq!(player.get_scale(), 2.0);
    player.set_rot(0.5);
    assert_eq!(player.get_rot(), 0.5);
}

#[test]
fn timeline_calls_follow_official_names() {
    let Some(mut player) = player() else {
        return;
    };
    assert!(player.main_timeline_count() > 0);
    let label = player
        .main_timeline_label_at(0)
        .expect("first main timeline")
        .to_owned();
    assert!(player.timeline_total_frame_count(&label).is_some());

    player.play_timeline(&label, 0).expect("play timeline");
    assert!(player.is_timeline_playing(&label));
    assert!(player.is_timeline_playing(""));
    assert!(player.playing_timeline_count() > 0);
    assert_eq!(player.playing_timeline_label_at(0), Some(label.clone()));
    assert!(player.playing_timeline_flags_at(0).is_some());

    player.progress(1.0).expect("progress");
    player.stop_timeline(&label).expect("stop timeline");
    assert!(!player.is_timeline_playing(&label));
}

#[test]
fn unsupported_official_device_calls_are_explicit() {
    let stencil = yu_emote::EmoteDeviceOptions {
        mask_mode: yu_emote::EmoteMaskMode::Stencil,
        ..Default::default()
    };
    assert!(matches!(
        yu_emote::EmoteDevice::headless(stencil),
        Err(yu_emote::EmoteError::UnsupportedFeature(_))
    ));

    let Ok((mut device, _info)) = yu_emote::EmoteDevice::headless(Default::default()) else {
        eprintln!("SKIP: no wgpu adapter");
        return;
    };
    assert!(matches!(
        device.set_mask_mode(yu_emote::EmoteMaskMode::Stencil),
        Err(yu_emote::EmoteError::UnsupportedFeature(_))
    ));
    assert!(matches!(
        device.set_shader_model(yu_emote::ShaderModel::Model3),
        Err(yu_emote::EmoteError::UnsupportedFeature(_))
    ));
    assert!(matches!(
        device.set_mip_map_enabled(true),
        Err(yu_emote::EmoteError::UnsupportedFeature(_))
    ));
    assert_eq!(device.shader_model(), yu_emote::ShaderModel::Wgpu);
    device.set_screen_rect(640, 480);
    assert_eq!(device.screen_rect(), [640, 480]);
}

#[test]
fn device_creates_ready_player_on_wgpu() {
    let Some(path) = sample() else {
        return;
    };
    let bytes = std::fs::read(path).expect("read sample");
    let Ok((device, _info)) = yu_emote::EmoteDevice::headless(Default::default()) else {
        eprintln!("SKIP: no wgpu adapter");
        return;
    };
    let mut player = device
        .create_player(&bytes, key(), None)
        .expect("create player");
    assert_eq!(player.capability(), Capability::Ready);
    assert!(player.has_renderer());
    player.set_canvas(200, 300);
    player.progress(1.0).expect("progress");
    let frame = player.render().expect("gpu render");
    let rgba = player.read_frame(&frame).expect("readback");
    let covered = rgba.chunks_exact(4).filter(|pixel| pixel[3] != 0).count();
    assert!(covered > 1000, "GPU frame is nearly empty: {covered}");

    player.release_decoded_textures();
    assert!(player.textures().is_empty());
    let frame = player.render().expect("gpu render after texture release");
    let rgba = player.read_frame(&frame).expect("readback");
    let covered = rgba.chunks_exact(4).filter(|pixel| pixel[3] != 0).count();
    assert!(
        covered > 1000,
        "GPU frame after release is nearly empty: {covered}"
    );
    assert!(matches!(
        player.render_cpu(),
        Err(yu_emote::EmoteError::UnsupportedFeature(_))
    ));
}
