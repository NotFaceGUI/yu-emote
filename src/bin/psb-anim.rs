//! Render a timeline to a deterministic PNG sequence.
//!
//! ```text
//! psb-anim <model.psb> --timeline NAME --out-dir DIR [--fps 15] [--seconds 7]
//!          [--key HEX] [--motion NAME] [--view SCALE,X,Y] [--canvas WxH]
//!          [--var NAME=VALUE]... [--settle TICKS] [--settle-loops N]
//!          [--margin PX] [--no-loop] [--no-physics]
//! ```
//!
//! For an authored loop the clip starts at `loopBegin`; `--settle-loops N`
//! advances N complete loops before recording so physics/controller transients
//! do not appear as a seam when the video repeats.
//!
//! The player advances in native 60 Hz ticks and renders one output frame per
//! `60 / fps` ticks, so physics and timeline easing are evaluated at engine
//! rate regardless of the output frame rate. The first frame fixes the view
//! unless `--view` is supplied.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::json;
use yu_emote::{EMOTE_TICKS_PER_SECOND, Emote, TimelinePlayMode, soft};

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
            "usage: psb-anim <model.psb> --timeline NAME --out-dir DIR [--fps N] \
             [--seconds T] [--key HEX] [--motion NAME] [--view SCALE,X,Y] \
             [--canvas WxH] [--var NAME=VALUE]... [--settle TICKS] \
             [--settle-loops N] [--margin PX] [--no-loop] [--no-physics]"
        );
        return Ok(());
    };

    let mut key = None;
    let mut motion = None;
    let mut timeline = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut fps = 15.0f32;
    let mut seconds = 7.0f32;
    let mut settle_ticks = 0u32;
    let mut settle_loops = 0u32;
    let mut looping = true;
    let mut physics = true;
    let mut variables = BTreeMap::new();
    let mut fixed_view: Option<soft::ViewTransform> = None;
    let mut canvas = [900u32, 1400u32];
    let mut margin = 20.0f32;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--key" => key = Some(parse_key(&need(&mut args, "--key")?)?),
            "--motion" => motion = Some(need(&mut args, "--motion")?),
            "--timeline" => timeline = Some(need(&mut args, "--timeline")?),
            "--out-dir" => out_dir = Some(PathBuf::from(need(&mut args, "--out-dir")?)),
            "--fps" => {
                let value = parse_num(&need(&mut args, "--fps")?)?;
                if value <= 0.0 {
                    return Err("--fps needs a positive number".into());
                }
                fps = value;
            }
            "--seconds" => seconds = parse_num(&need(&mut args, "--seconds")?)?,
            "--settle" => settle_ticks = parse_num::<u32>(&need(&mut args, "--settle")?)?,
            "--settle-loops" => {
                settle_loops = parse_num::<u32>(&need(&mut args, "--settle-loops")?)?
            }
            "--no-loop" => looping = false,
            "--no-physics" => physics = false,
            "--no-controls" => {
                // Controllers are part of the runtime; accepted for CLI
                // compatibility with the previous tool.
            }
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
            other => return Err(format!("unknown argument `{other}`").into()),
        }
    }

    let timeline = timeline.ok_or("--timeline is required")?;
    let out_dir = out_dir.ok_or("--out-dir is required")?;
    fs::create_dir_all(&out_dir)?;

    let mut emote = Emote::load(&path, key, motion.as_deref())?;
    emote.set_variables(&variables)?;
    if !physics {
        emote.runtime.set_physics_enabled(false);
    }
    let mode = if looping {
        TimelinePlayMode::LOOP
    } else {
        TimelinePlayMode::ONCE
    };
    emote.play_timeline(&timeline, mode)?;
    // An authored loop may have an intro before `loopBegin`. Start the clip at
    // the steady-state boundary so the encoded video loops without a seam.
    let loop_bounds = if looping {
        let begin = emote.runtime.timeline_loop_begin(&timeline);
        let end = emote.runtime.timeline_loop_end(&timeline);
        if let Some(begin) = begin {
            emote.runtime.set_timeline_time(&timeline, begin)?;
        }
        begin.zip(end)
    } else {
        None
    };

    // Let one-shot physics/controller transients settle before recording. A
    // cold-start first loop does not have the same state as the steady-state
    // loop, which would show up as a jump when the video repeats.
    if let Some((begin, end)) = loop_bounds {
        let loop_ticks = (end - begin).max(0.0);
        for _ in 0..settle_loops {
            let mut remaining = loop_ticks;
            while remaining > 0.0 {
                let step = remaining.min(1.0);
                emote.advance_ticks(step)?;
                remaining -= step;
            }
        }
    }
    for _ in 0..settle_ticks {
        emote.advance_ticks(1.0)?;
    }

    let frames = (seconds * fps).round().max(1.0) as usize;
    let ticks_per_frame = EMOTE_TICKS_PER_SECOND / fps;
    let mut tick_carry = 0.0f32;
    let mut textures = None;
    let mut view = fixed_view;
    let mut written = 0usize;

    for frame in 0..frames {
        tick_carry += ticks_per_frame;
        while tick_carry >= 1.0 {
            emote.advance_ticks(1.0)?;
            tick_carry -= 1.0;
        }

        let plan = emote.plan(canvas);
        let frame_view = match view {
            Some(view) => view,
            None => {
                let fitted = soft::ViewTransform::fit(&plan, margin);
                view = Some(fitted);
                fitted
            }
        };
        if textures.is_none() {
            textures = Some(emote.textures()?);
        }
        let textures = textures.as_ref().expect("textures were just decoded");
        let (image, _stats) = soft::render_with_textures(&plan, textures, frame_view)?;
        let file = out_dir.join(format!("frame-{frame:04}.png"));
        write_png(&file, &image)?;
        written += 1;
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "timeline": timeline,
            "frames": written,
            "fps": fps,
            "canvas": canvas,
            "view": view.map(|view| json!({
                "scale": view.scale,
                "offset": view.offset,
            })),
            "playing": emote.runtime.is_timeline_playing(&timeline),
            "loop": emote.runtime.is_loop_timeline(&timeline),
            "loop_begin": emote.runtime.timeline_loop_begin(&timeline),
            "loop_end": emote.runtime.timeline_loop_end(&timeline),
            "out_dir": out_dir,
        }))?
    );
    Ok(())
}

fn write_png(path: &Path, image: &soft::RenderImage) -> Result<(), Box<dyn std::error::Error>> {
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
