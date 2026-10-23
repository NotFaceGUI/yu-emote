//! Render a timeline to a deterministic PNG sequence.
//!
//! ```text
//! psb-anim <model.psb> --timeline NAME --out-dir DIR [--fps 15] [--seconds 7]
//!          [--key HEX] [--object NAME] [--motion NAME] [--view SCALE,X,Y]
//!          [--var NAME=VALUE]... [--settle TICKS] [--margin 20]
//! ```
//!
//! One player is advanced across the whole clip, so this is O(frames) rather
//! than re-simulating from zero for every image. The first frame also fixes
//! the camera unless `--view` is supplied.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::json;
use yu_emote::plan::RenderPlan;
use yu_emote::player::Player;
use yu_emote::psb;
use yu_emote::schema::Model;
use yu_emote::soft;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!(
            "usage: psb-anim <model.psb> --timeline NAME --out-dir DIR [--key HEX] \
             [--object NAME] [--motion NAME] [--fps N] [--seconds T] [--view SCALE,X,Y] \
             [--var NAME=VALUE]... [--settle TICKS]"
        );
        return ExitCode::FAILURE;
    };
    let mut key = None;
    let mut object = None;
    let mut motion = None;
    let mut timeline = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut fps = 15.0f64;
    let mut seconds = 7.0f64;
    let mut variables = BTreeMap::new();
    let mut fixed_view: Option<soft::ViewTransform> = None;
    let mut settle = 0usize;
    let mut margin = 20.0f32;
    let mut loop_timeline = true;
    let mut controls_enabled = true;
    let mut physics_enabled = true;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--key" => match args.next().as_deref().map(parse_key) {
                Some(Ok(value)) => key = Some(value),
                _ => {
                    eprintln!("--key needs a decimal or 0x-prefixed u32");
                    return ExitCode::FAILURE;
                }
            },
            "--object" => object = args.next(),
            "--motion" => motion = args.next(),
            "--timeline" => timeline = args.next(),
            "--out-dir" => out_dir = args.next().map(PathBuf::from),
            "--fps" => match args.next().map(|value| value.parse::<f64>()) {
                Some(Ok(value)) if value > 0.0 => fps = value,
                _ => {
                    eprintln!("--fps needs a positive number");
                    return ExitCode::FAILURE;
                }
            },
            "--seconds" => match args.next().map(|value| value.parse::<f64>()) {
                Some(Ok(value)) if value >= 0.0 => seconds = value,
                _ => {
                    eprintln!("--seconds needs a non-negative number");
                    return ExitCode::FAILURE;
                }
            },
            "--margin" => match args.next().map(|value| value.parse::<f32>()) {
                Some(Ok(value)) => margin = value,
                _ => {
                    eprintln!("--margin needs a number");
                    return ExitCode::FAILURE;
                }
            },
            "--no-loop" => loop_timeline = false,
            "--no-controls" => controls_enabled = false,
            "--no-physics" => physics_enabled = false,
            "--settle" => match args.next().map(|value| value.parse::<usize>()) {
                Some(Ok(value)) => settle = value,
                _ => {
                    eprintln!("--settle needs a non-negative integer");
                    return ExitCode::FAILURE;
                }
            },
            "--var" => {
                let Some(spec) = args.next() else {
                    eprintln!("--var needs NAME=VALUE");
                    return ExitCode::FAILURE;
                };
                let Some((name, value)) = spec.split_once('=') else {
                    eprintln!("invalid --var `{spec}`");
                    return ExitCode::FAILURE;
                };
                match value.parse::<f64>() {
                    Ok(value) => {
                        variables.insert(name.to_string(), value);
                    }
                    Err(error) => {
                        eprintln!("invalid --var value `{value}`: {error}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "--view" => {
                let Some(value) = args.next() else {
                    eprintln!("--view needs SCALE,X,Y");
                    return ExitCode::FAILURE;
                };
                let parts: Vec<_> = value.split(',').collect();
                if parts.len() != 3 {
                    eprintln!("--view needs SCALE,X,Y");
                    return ExitCode::FAILURE;
                }
                match (
                    parts[0].parse::<f32>(),
                    parts[1].parse::<f32>(),
                    parts[2].parse::<f32>(),
                ) {
                    (Ok(scale), Ok(x), Ok(y)) => {
                        fixed_view = Some(soft::ViewTransform {
                            scale,
                            offset: [x, y],
                        })
                    }
                    _ => {
                        eprintln!("--view needs numeric SCALE,X,Y");
                        return ExitCode::FAILURE;
                    }
                }
            }
            other => {
                eprintln!("unknown argument `{other}`");
                return ExitCode::FAILURE;
            }
        }
    }
    let Some(timeline) = timeline else {
        eprintln!("--timeline is required");
        return ExitCode::FAILURE;
    };
    let Some(out_dir) = out_dir else {
        eprintln!("--out-dir is required");
        return ExitCode::FAILURE;
    };

    let document = match psb::read_document_with_key(&path, key) {
        Ok(document) => document,
        Err(error) => {
            eprintln!("{path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let model = Model::from_document(&document);
    let object = object
        .or_else(|| model.metadata.character.clone())
        .or_else(|| model.objects.keys().next().cloned())
        .unwrap_or_default();
    let motion = motion
        .or_else(|| model.metadata.base_motion.clone())
        .or_else(|| {
            model
                .objects
                .get(&object)
                .and_then(|object| object.motions.keys().next().cloned())
        })
        .unwrap_or_default();

    let mut player = Player::new(&model, &object, &motion, 60.0);
    player.controls_enabled = controls_enabled;
    player.physics_enabled = physics_enabled;
    for (name, value) in &variables {
        player.set_variable(name.clone(), *value);
    }
    if settle > 0 {
        player.settle(&model, settle);
    }
    if let Err(error) = player.play(&timeline, loop_timeline) {
        eprintln!("timeline `{timeline}`: {error}");
        return ExitCode::FAILURE;
    }

    let frame_count = (seconds * fps).round().max(1.0) as usize;
    if let Err(error) = fs::create_dir_all(&out_dir) {
        eprintln!("{}: {error}", out_dir.display());
        return ExitCode::FAILURE;
    }

    let mut textures: Option<BTreeMap<String, yu_emote::texture::DecodedTexture>> = None;
    let mut view = fixed_view;
    let mut written = Vec::with_capacity(frame_count);
    let dt = 1.0 / fps;
    for index in 0..frame_count {
        if index > 0 {
            player.tick(&model, dt);
        }
        player.overlay_variables(&variables);
        let Some(pose) = player.pose(&model) else {
            eprintln!("no pose for `{object}/{motion}`");
            return ExitCode::FAILURE;
        };
        let plan = RenderPlan::build(&model, &pose);
        if textures.is_none() {
            textures = match soft::decode_textures(&document, &model, &plan) {
                Ok(textures) => Some(textures),
                Err(error) => {
                    eprintln!("textures: {error}");
                    return ExitCode::FAILURE;
                }
            };
        }
        if view.is_none() {
            view = Some(soft::ViewTransform::fit(&plan, margin));
        }
        let Some(view_transform) = view else {
            unreachable!();
        };
        let Some(textures) = textures.as_ref() else {
            unreachable!();
        };
        let (image, _stats) = match soft::render_with_textures(&plan, textures, view_transform) {
            Ok(rendered) => rendered,
            Err(error) => {
                eprintln!("render: {error}");
                return ExitCode::FAILURE;
            }
        };
        let frame_path = out_dir.join(format!("frame-{index:04}.png"));
        if let Err(error) = write_png(&frame_path, &image) {
            eprintln!("{}: {error}", frame_path.display());
            return ExitCode::FAILURE;
        }
        written.push(frame_path.to_string_lossy().to_string());
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "object": object,
            "motion": motion,
            "timeline": timeline,
            "fps": fps,
            "seconds": seconds,
            "frames": written.len(),
            "view": view,
            "first": written.first(),
            "last": written.last(),
        }))
        .unwrap_or_default()
    );
    ExitCode::SUCCESS
}

fn write_png(path: &Path, image: &soft::RenderImage) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::create(path)?;
    let mut encoder = png::Encoder::new(file, image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&image.rgba8)?;
    Ok(())
}

fn parse_key(value: &str) -> Result<u32, ()> {
    let value = value.trim();
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16).map_err(|_| ())
    } else {
        value.parse().map_err(|_| ())
    }
}
