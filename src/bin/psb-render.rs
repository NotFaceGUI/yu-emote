//! Render one E-mote frame with the deterministic software rasterizer.
//!
//! ```text
//! psb-render <model.psb> [--key HEX] [--motion NAME] [--time TICKS]
//!            [--timeline NAME] [--seconds S] [--loop] [--settle TICKS]
//!            [--var NAME=VALUE]... [--view SCALE,X,Y] [--canvas WxH]
//!            [--margin PX] [--no-fit] [--out FILE.png]
//! ```
//!
//! `--time` and `--settle` are native 1/60 s ticks. Without `--out`, only the
//! JSON summary is printed. With `--out`, an RGBA PNG is written and the same
//! summary is printed to stdout.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufWriter;
use std::process::ExitCode;

use serde_json::json;
use yu_emote::{Emote, TimelinePlayMode, soft};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!(
            "usage: psb-render <model.psb> [--key HEX] [--motion NAME] [--time TICKS] \
             [--timeline NAME] [--seconds S] [--loop] [--settle TICKS] \
             [--var NAME=VALUE]... [--view SCALE,X,Y] [--canvas WxH] \
             [--margin PX] [--no-fit] [--out FILE.png]"
        );
        return Ok(());
    };

    let mut key = None;
    let mut motion = None;
    let mut timeline = None;
    let mut time_ticks = 0.0f32;
    let mut seconds = 0.0f32;
    let mut settle_ticks = 0u32;
    let mut looping = false;
    let mut variables = BTreeMap::new();
    let mut fixed_view: Option<soft::ViewTransform> = None;
    let mut canvas = [900u32, 1400u32];
    let mut margin = 20.0f32;
    let mut fit = true;
    let mut output = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--key" => key = Some(parse_key(&need(&mut args, "--key")?)?),
            "--motion" => motion = Some(need(&mut args, "--motion")?),
            "--timeline" => timeline = Some(need(&mut args, "--timeline")?),
            "--time" => time_ticks = parse_num(&need(&mut args, "--time")?)?,
            "--seconds" => seconds = parse_num(&need(&mut args, "--seconds")?)?,
            "--settle" => settle_ticks = parse_num::<u32>(&need(&mut args, "--settle")?)?,
            "--loop" => looping = true,
            "--var" => {
                let spec = need(&mut args, "--var")?;
                let (name, value) = spec
                    .split_once('=')
                    .ok_or_else(|| format!("invalid --var `{spec}` (expected NAME=VALUE)"))?;
                variables.insert(name.to_owned(), parse_num::<f32>(value)? as f64);
            }
            "--view" => {
                let value = need(&mut args, "--view")?;
                let parts: Vec<_> = value.split(',').collect();
                if parts.len() != 3 {
                    return Err(format!("--view needs SCALE,X,Y, got `{value}`").into());
                }
                fixed_view = Some(soft::ViewTransform {
                    scale: parse_num(parts[0])?,
                    offset: [parse_num(parts[1])?, parse_num(parts[2])?],
                });
            }
            "--canvas" => {
                let value = need(&mut args, "--canvas")?;
                let (w, h) = value
                    .split_once(['x', 'X', ','])
                    .ok_or_else(|| format!("--canvas needs WxH, got `{value}`"))?;
                canvas = [parse_num::<u32>(w)?.max(1), parse_num::<u32>(h)?.max(1)];
            }
            "--margin" => margin = parse_num(&need(&mut args, "--margin")?)?,
            "--no-fit" => fit = false,
            "--out" => output = Some(need(&mut args, "--out")?),
            other => return Err(format!("unknown argument `{other}`").into()),
        }
    }

    let mut emote = Emote::load(&path, key, motion.as_deref())?;
    emote.set_variables(&variables)?;

    if let Some(name) = timeline.as_deref() {
        let mode = if looping {
            TimelinePlayMode::LOOP
        } else {
            TimelinePlayMode::ONCE
        };
        emote.play_timeline(name, mode)?;
    }

    let mut remaining = time_ticks + seconds * yu_emote::EMOTE_TICKS_PER_SECOND;
    remaining += settle_ticks as f32;
    advance_ticks(&mut emote, remaining)?;

    let plan = emote.plan(canvas);
    let view = if let Some(view) = fixed_view {
        view
    } else if fit {
        soft::ViewTransform::fit(&plan, margin)
    } else {
        soft::ViewTransform::identity()
    };
    let textures = emote.textures()?;
    let (image, stats) = soft::render_with_textures(&plan, &textures, view)?;

    if let Some(output) = output {
        write_png(&output, &image)?;
    }

    let bounds = plan.bounds();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "motion": motion,
            "timeline": timeline,
            "ticks": remaining,
            "canvas": canvas,
            "view": { "scale": view.scale, "offset": view.offset },
            "bounds": bounds.map(|(min, max)| json!([min, max])),
            "plan": plan.summary_json(),
            "stats": stats,
            "image": {
                "width": image.width,
                "height": image.height,
                "covered": image.covered_pixels(),
                "bbox": image.coverage_bbox(),
            },
            "spec": emote.spec(),
            "screen_size": emote.screen_size(),
            "variables": emote.variables(),
        }))?
    );
    Ok(())
}

fn advance_ticks(emote: &mut Emote, mut ticks: f32) -> Result<(), Box<dyn std::error::Error>> {
    // Advance in native 60 Hz steps so timeline easing, controllers and
    // physics see the same deltas as the engine.
    while ticks > 0.0 {
        let step = ticks.min(1.0);
        emote.advance_ticks(step)?;
        ticks -= step;
    }
    Ok(())
}

fn write_png(path: &str, image: &soft::RenderImage) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::create(path)?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&image.rgba8)?;
    Ok(())
}

fn need(args: &mut impl Iterator<Item = String>, option: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{option} needs a value"))
}

fn parse_key(raw: &str) -> Result<u32, String> {
    let trimmed = raw.trim();
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16).map_err(|_| format!("invalid key `{raw}`"))
    } else {
        trimmed
            .parse::<u32>()
            .map_err(|_| format!("invalid key `{raw}`"))
    }
}

fn parse_num<T: std::str::FromStr>(raw: &str) -> Result<T, String> {
    raw.trim()
        .parse::<T>()
        .map_err(|_| format!("invalid number `{raw}`"))
}
