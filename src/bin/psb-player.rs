//! Drive one E-mote model through the official-shaped player facade.
//!
//! ```text
//! psb-player <model.psb> [--key HEX] [--motion NAME] [--timeline NAME]
//!            [--ticks N] [--canvas WxH] [--view SCALE,X,Y]
//!            [--margin PX] [--gpu] [--out FILE.png]
//! ```
//!
//! This is the reference host loop for the kernel: create a player, play a
//! timeline, advance native 60 Hz ticks, then render. Without `--gpu` the
//! deterministic CPU rasterizer is used; `--gpu` exercises
//! `EmoteDevice`/`EmotePlayer::render` on a headless wgpu device.

use std::fs::File;
use std::io::BufWriter;
use std::process::ExitCode;

use serde_json::json;
use yu_emote::{
    Emote, EmoteDevice, EmoteDeviceOptions, EmotePlayer, EmotePlayerOptions,
    OFFICIAL_INTERFACE_VERSION, soft,
};

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
            "usage: psb-player <model.psb> [--key HEX] [--motion NAME] \
             [--timeline NAME] [--ticks N] [--canvas WxH] [--view SCALE,X,Y] \
             [--margin PX] [--gpu] [--out FILE.png]"
        );
        return Ok(());
    };

    let mut key = None;
    let mut motion = None;
    let mut timeline = None;
    let mut ticks = 0u32;
    let mut canvas = [800u32, 600u32];
    let mut margin = 20.0f32;
    let mut view = None;
    let mut gpu = false;
    let mut output = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--key" => key = Some(parse_key(&need(&mut args, "--key")?)?),
            "--motion" => motion = Some(need(&mut args, "--motion")?),
            "--timeline" => timeline = Some(need(&mut args, "--timeline")?),
            "--ticks" => ticks = parse_num::<u32>(&need(&mut args, "--ticks")?)?,
            "--canvas" => {
                let value = need(&mut args, "--canvas")?;
                let (w, h) = value
                    .split_once(['x', 'X', ','])
                    .ok_or_else(|| format!("--canvas needs WxH, got `{value}`"))?;
                canvas = [parse_num::<u32>(w)?.max(1), parse_num::<u32>(h)?.max(1)];
            }
            "--view" => {
                let value = need(&mut args, "--view")?;
                let parts: Vec<_> = value.split(',').collect();
                if parts.len() != 3 {
                    return Err(format!("--view needs SCALE,X,Y, got `{value}`").into());
                }
                view = Some(soft::ViewTransform {
                    scale: parse_num(parts[0])?,
                    rotation: 0.0,
                    offset: [parse_num(parts[1])?, parse_num(parts[2])?],
                });
            }
            "--margin" => margin = parse_num(&need(&mut args, "--margin")?)?,
            "--gpu" => gpu = true,
            "--out" => output = Some(need(&mut args, "--out")?),
            other => return Err(format!("unknown argument `{other}`").into()),
        }
    }

    let emote = Emote::load(&path, key, motion.as_deref())?;
    let options = EmotePlayerOptions {
        canvas,
        view,
        margin,
    };
    let mut player = if gpu {
        let (device, _adapter) = EmoteDevice::headless(EmoteDeviceOptions {
            screen: canvas,
            ..EmoteDeviceOptions::default()
        })?;
        device.create_player_from_emote_with_options(emote, options)?
    } else {
        EmotePlayer::new(emote, options)?
    };
    player.set_canvas(canvas[0], canvas[1]);
    if let Some(view) = view {
        player.set_view(Some(view));
    }
    if let Some(name) = timeline.as_deref() {
        player.play_timeline(name, 0)?;
    }

    // Native 60 Hz stepping: the same deltas the engine uses for easing,
    // controllers and physics.
    for _ in 0..ticks {
        player.progress(1.0)?;
    }

    let plan = player.plan();
    let effective_view = player.effective_view();
    let mut summary = json!({
        "capability": player.capability().as_str(),
        "official_interface": OFFICIAL_INTERFACE_VERSION,
        "ticks": ticks,
        "canvas": canvas,
        "view": effective_view,
        "plan": plan.summary_json(),
    });
    let (image_width, image_height, covered, rgba8) = if gpu {
        let frame = player.render()?;
        let data = player.read_frame(&frame)?;
        let covered = data.chunks_exact(4).filter(|pixel| pixel[3] != 0).count();
        (frame.width, frame.height, covered, data)
    } else {
        let (image, stats) = player.render_cpu()?;
        summary["stats"] = serde_json::to_value(stats)?;
        (
            image.width,
            image.height,
            image.covered_pixels(),
            image.rgba8,
        )
    };
    summary["image"] = json!({
        "width": image_width,
        "height": image_height,
        "covered": covered,
    });
    println!("{}", serde_json::to_string_pretty(&summary)?);

    if let Some(output) = output {
        write_png(&output, image_width, image_height, &rgba8)?;
    }
    Ok(())
}

fn write_png(
    path: &str,
    width: u32,
    height: u32,
    rgba8: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::create(path)?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgba8)?;
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
