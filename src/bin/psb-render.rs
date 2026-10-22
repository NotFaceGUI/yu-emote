//! Render one E-mote motion with the deterministic software rasterizer.
//!
//! ```text
//! psb-render <model.psb> [--key HEX] [--object NAME] [--motion NAME]
//!            [--time FRAMES] [--var NAME=VALUE]... [--out FILE.png]
//! ```
//!
//! Without `--out`, prints statistics (coverage, bbox, triangles). With
//! `--out`, writes an RGBA PNG and still prints statistics.

use std::collections::BTreeMap;
use std::fs::File;
use std::process::ExitCode;

use serde_json::json;
use yu_emote::anim::Animator;
use yu_emote::pose::Pose;
use yu_emote::psb;
use yu_emote::schema::Model;
use yu_emote::soft;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!(
            "usage: psb-render <model.psb> [--key HEX] [--object NAME] [--motion NAME] \
             [--time FRAMES] [--var NAME=VALUE]... [--out FILE.png] \
             [--only LABEL] [--skip LABEL]"
        );
        return ExitCode::FAILURE;
    };
    let mut key = None;
    let mut object = None;
    let mut motion = None;
    let mut time = 0.0f64;
    let mut output = None;
    let mut variables = BTreeMap::new();
    let mut fit = true;
    let mut margin = 20.0f32;
    let mut scale_override: Option<f32> = None;
    let mut only: Vec<String> = Vec::new();
    let mut skip: Vec<String> = Vec::new();
    let mut limit: Option<usize> = None;
    let mut timeline: Option<String> = None;
    let mut seconds = 0.0f64;
    let mut fps = 60.0f64;
    let mut loop_timeline = false;
    let mut fixed_view: Option<(f32, f32, f32)> = None;
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
            "--time" => match args.next().map(|value| value.parse::<f64>()) {
                Some(Ok(value)) => time = value,
                Some(Err(error)) => {
                    eprintln!("invalid --time: {error}");
                    return ExitCode::FAILURE;
                }
                None => {
                    eprintln!("--time needs a number");
                    return ExitCode::FAILURE;
                }
            },
            "--var" => {
                let Some(spec) = args.next() else {
                    eprintln!("--var needs NAME=VALUE");
                    return ExitCode::FAILURE;
                };
                let Some((name, value)) = spec.split_once('=') else {
                    eprintln!("invalid --var `{spec}` (expected NAME=VALUE)");
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
            "--out" => output = args.next(),
            "--no-fit" => fit = false,
            "--margin" => match args.next().map(|value| value.parse::<f32>()) {
                Some(Ok(value)) => margin = value,
                _ => {
                    eprintln!("--margin needs a number");
                    return ExitCode::FAILURE;
                }
            },
            "--scale" => match args.next().map(|value| value.parse::<f32>()) {
                Some(Ok(value)) => scale_override = Some(value),
                _ => {
                    eprintln!("--scale needs a number");
                    return ExitCode::FAILURE;
                }
            },
            "--only" => {
                if let Some(value) = args.next() {
                    only.extend(value.split(',').map(str::to_owned));
                }
            }
            "--skip" => {
                if let Some(value) = args.next() {
                    skip.extend(value.split(',').map(str::to_owned));
                }
            }
            "--limit" => match args.next().map(|value| value.parse::<usize>()) {
                Some(Ok(value)) => limit = Some(value),
                _ => {
                    eprintln!("--limit needs a number");
                    return ExitCode::FAILURE;
                }
            },
            "--timeline" => timeline = args.next(),
            "--loop" => loop_timeline = true,
            "--seconds" => match args.next().map(|value| value.parse::<f64>()) {
                Some(Ok(value)) => seconds = value,
                _ => {
                    eprintln!("--seconds needs a number");
                    return ExitCode::FAILURE;
                }
            },
            "--fps" => match args.next().map(|value| value.parse::<f64>()) {
                Some(Ok(value)) if value > 0.0 => fps = value,
                _ => {
                    eprintln!("--fps needs a positive number");
                    return ExitCode::FAILURE;
                }
            },
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
                    (Ok(scale), Ok(x), Ok(y)) => fixed_view = Some((scale, x, y)),
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

    let document = match psb::read_document_with_key(&path, key) {
        Ok(document) => document,
        Err(error) => {
            eprintln!("{path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let model = Model::from_document(&document);
    let mut timeline_info = None;
    if let Some(name) = &timeline {
        let mut animator = Animator::new(&model, fps);
        if let Err(error) = animator.play_with(name, 1.0, loop_timeline) {
            eprintln!("timeline `{name}`: {error}");
            return ExitCode::FAILURE;
        }
        let mut ticks = 0usize;
        let count = (seconds.max(0.0) * fps).round() as usize;
        for _ in 0..count {
            animator.tick(1.0 / fps);
            ticks += 1;
        }
        let evaluated = animator.evaluated_variables();
        // Explicit --var values win over the timeline.
        for (name, value) in evaluated {
            variables.entry(name).or_insert(value);
        }
        timeline_info = Some(json!({
            "name": name,
            "seconds": seconds,
            "fps": fps,
            "ticks": ticks,
            "unsupported_easing": animator.unsupported_easing(),
        }));
    }
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
    let Some(pose) = Pose::evaluate(&model, &object, &motion, time, &variables) else {
        eprintln!("no motion `{object}/{motion}`");
        return ExitCode::FAILURE;
    };
    let mut plan = yu_emote::plan::RenderPlan::build(&model, &pose);
    let view = if let Some((scale, x, y)) = fixed_view {
        soft::ViewTransform {
            scale,
            offset: [x, y],
        }
    } else if let Some(scale) = scale_override {
        let (min, max) = plan.bounds().unwrap_or(([0.0; 2], [1.0; 2]));
        let center = [(min[0] + max[0]) * 0.5, (min[1] + max[1]) * 0.5];
        soft::ViewTransform {
            scale,
            offset: [
                plan.width as f32 * 0.5 - center[0] * scale,
                plan.height as f32 * 0.5 - center[1] * scale,
            ],
        }
    } else if fit {
        soft::ViewTransform::fit(&plan, margin)
    } else {
        soft::ViewTransform::identity()
    };
    if !only.is_empty() {
        plan.items.retain(|item| {
            only.iter()
                .any(|needle| item.label.contains(needle.as_str()))
        });
    }
    if !skip.is_empty() {
        plan.items.retain(|item| {
            !skip
                .iter()
                .any(|needle| item.label.contains(needle.as_str()))
        });
    }
    if let Some(limit) = limit {
        plan.items.truncate(limit);
    }
    let (image, stats) = match soft::render_plan_with_document(&document, &model, &plan, view) {
        Ok(rendered) => rendered,
        Err(error) => {
            eprintln!("render: {error}");
            return ExitCode::FAILURE;
        }
    };
    let bbox = image.coverage_bbox();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "object": object,
            "motion": motion,
            "time": time,
            "width": image.width,
            "height": image.height,
            "covered_pixels": image.covered_pixels(),
            "bbox": bbox,
            "view": view,
            "stats": stats,
            "timeline": timeline_info,
        }))
        .unwrap_or_default()
    );

    if let Some(path) = output {
        if let Err(error) = write_png(&path, &image) {
            eprintln!("{path}: {error}");
            return ExitCode::FAILURE;
        }
        eprintln!("wrote {path}");
    }
    ExitCode::SUCCESS
}

fn write_png(path: &str, image: &soft::RenderImage) -> Result<(), Box<dyn std::error::Error>> {
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
