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
    let mut only: Option<String> = None;
    let mut skip: Option<String> = None;
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
            "--only" => only = args.next(),
            "--skip" => skip = args.next(),
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
    let view = if let Some(scale) = scale_override {
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
    if let Some(needle) = &only {
        plan.items
            .retain(|item| item.label.contains(needle.as_str()));
    }
    if let Some(needle) = &skip {
        plan.items
            .retain(|item| !item.label.contains(needle.as_str()));
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
