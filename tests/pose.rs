//! Pose/plan tests. The official sample is optional (see `tests/schema.rs`).

use std::collections::BTreeMap;

use yu_emote::plan::RenderPlan;
use yu_emote::pose::{Pose, evaluate_frames, parameter_time};
use yu_emote::psb;
use yu_emote::schema::{Content, Frame, Model, ParameterDef, ParameterRef};

fn load_document() -> Option<(psb::PsbDocument, Model)> {
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

fn load() -> Option<Model> {
    load_document().map(|(_, model)| model)
}

#[test]
fn nested_motions_produce_drawable_character() {
    let Some(model) = load() else {
        return;
    };
    let variables = BTreeMap::new();
    let pose = Pose::evaluate(&model, "all_parts", "全体構造", 0.0, &variables).unwrap();
    let drawables = pose.drawables();
    assert!(
        drawables.len() > 20,
        "expected the nested character, got {}",
        drawables.len()
    );
    assert!(drawables.iter().all(|layer| layer.state.opacity > 0.0));
    assert!(drawables.iter().any(|layer| layer.state.mesh_bp.is_some()));
    assert!(
        drawables
            .iter()
            .any(|layer| layer.state.src.as_deref() == Some("tex#0"))
    );
    assert!(drawables.iter().all(|layer| layer.world.m11.is_finite()));
}

#[test]
fn pose_is_deterministic() {
    let Some(model) = load() else {
        return;
    };
    let variables = BTreeMap::new();
    let first = Pose::evaluate(&model, "all_parts", "全体構造", 12.0, &variables).unwrap();
    let second = Pose::evaluate(&model, "all_parts", "全体構造", 12.0, &variables).unwrap();
    assert_eq!(
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&second).unwrap()
    );
}

#[test]
fn plan_covers_visible_meshes() {
    let Some(model) = load() else {
        return;
    };
    let variables = BTreeMap::new();
    let pose = Pose::evaluate(&model, "all_parts", "全体構造", 20.0, &variables).unwrap();
    let plan = RenderPlan::build(&model, &pose);
    assert!(!plan.items.is_empty(), "{:?}", plan.skipped);
    assert!(plan.vertex_count() > 100);
    assert!(plan.triangle_count() > 0);
    assert!(!plan.skipped.contains_key("missing-page"));
    assert!(!plan.skipped.contains_key("missing-icon"));
    for item in &plan.items {
        assert!(item.page == "blank" || model.sources.iter().any(|page| page.name == item.page));
        for vertex in &item.vertices {
            assert!((0.0..=1.0).contains(&vertex.uv[0]), "uv {:?}", vertex.uv);
            assert!((0.0..=1.0).contains(&vertex.uv[1]), "uv {:?}", vertex.uv);
            assert!(vertex.position[0].is_finite());
            assert!(vertex.position[1].is_finite());
        }
        assert!(
            item.indices
                .iter()
                .all(|index| (*index as usize) < item.vertices.len())
        );
    }
    assert!(plan.items.iter().any(|item| item.mesh));
    assert!(plan.summary_json()["items"].is_array());
}

#[test]
fn frame_types_hold_and_interpolate() {
    let frame = |time: i64, kind: i64, coord: [f64; 3], ti: Option<i64>| Frame {
        time,
        kind,
        time_quantization: ti,
        content: Some(Content {
            coord: Some(coord),
            ..Content::default()
        }),
    };
    let frames = [
        frame(0, 3, [0.0, 0.0, 0.0], None),
        frame(10, 3, [10.0, 20.0, 0.0], None),
    ];
    let state = evaluate_frames(&frames, 5.0, None);
    assert_eq!(state.coord, [5.0, 10.0, 0.0]);
    assert!((state.interpolation_t - 0.5).abs() < 1e-9);

    // A type-0 frame holds the previous decoded state.
    let held = [
        frame(0, 3, [1.0, 2.0, 0.0], None),
        Frame {
            time: 10,
            kind: 0,
            time_quantization: None,
            content: None,
        },
    ];
    let previous = evaluate_frames(&held[..1], 0.0, None);
    let state = evaluate_frames(&held, 10.0, Some(&previous));
    assert_eq!(state.coord, previous.coord);
    assert_eq!(state.kind, 0);

    // `ti` quantizes elapsed time.
    let quantized = [
        frame(0, 3, [0.0, 0.0, 0.0], Some(4)),
        frame(10, 3, [10.0, 0.0, 0.0], None),
    ];
    let state = evaluate_frames(&quantized, 7.0, None);
    assert!((state.coord[0] - 4.0).abs() < 1e-9);
}

#[test]
fn software_render_is_deterministic() {
    let Some((document, model)) = load_document() else {
        return;
    };
    let variables = BTreeMap::new();
    let pose = Pose::evaluate(&model, "all_parts", "全体構造", 0.0, &variables).unwrap();
    let (first, stats) =
        yu_emote::soft::render_with_document(&document, &model, &pose, Default::default())
            .expect("render");
    let (second, _) =
        yu_emote::soft::render_with_document(&document, &model, &pose, Default::default())
            .expect("render");
    assert!(first.covered_pixels() > 10_000);
    assert!(stats.triangles > 0);
    assert_eq!(first.rgba8, second.rgba8);
}

#[test]
fn parameter_time_clamps_and_scales() {
    let mut model = Model::default();
    model.parameters.push(ParameterDef {
        id: Some("mouth".into()),
        range_begin: 0.0,
        range_end: 10.0,
        division: 5.0,
        discretization: false,
        raw: serde_json::Value::Null,
    });
    let mut variables = BTreeMap::new();
    variables.insert("mouth".to_string(), 4.0);
    let reference = ParameterRef::Index(0);
    assert_eq!(
        parameter_time(&model, Some(&reference), &variables),
        Some(2.0)
    );
    variables.insert("mouth".to_string(), 100.0);
    assert_eq!(
        parameter_time(&model, Some(&reference), &variables),
        Some(5.0)
    );
    assert_eq!(
        parameter_time(&model, Some(&ParameterRef::Index(99)), &variables),
        Some(0.0)
    );
}
