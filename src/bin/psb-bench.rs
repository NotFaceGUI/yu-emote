//! Benchmark the runtime + draw plan + software rasterizer.
//!
//! ```text
//! psb-bench <model.psb> [--key HEX] [--motion NAME] [--timeline NAME]
//!           [--canvas WxH] [--frames N] [--warmup N] [--loop] [--no-loop]
//!           [--threads N] [--view SCALE,X,Y]
//! ```
//!
//! Reports mean/p50/p95/min/max milliseconds for tick, plan and raster, plus
//! the implied frames per second. No PNG encoding is included.

use std::time::{Duration, Instant};

use serde_json::json;
use yu_emote::{Emote, TimelinePlayMode, soft};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!(
            "usage: psb-bench <model.psb> [--key HEX] [--motion NAME] [--timeline NAME] \
             [--canvas WxH] [--frames N] [--warmup N] [--no-loop] \
             [--threads N] [--view SCALE,X,Y]"
        );
        return Ok(());
    };
    let mut key = None;
    let mut motion = None;
    let mut timeline = None;
    let mut canvas = [800u32, 600u32];
    let mut frames = 120usize;
    let mut warmup = 5usize;
    let mut looping = true;
    let mut threads = 0usize;
    let mut view: Option<soft::ViewTransform> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--key" => key = Some(parse_key(&need(&mut args, "--key")?)?),
            "--motion" => motion = Some(need(&mut args, "--motion")?),
            "--timeline" => timeline = Some(need(&mut args, "--timeline")?),
            "--frames" => frames = parse_num::<usize>(&need(&mut args, "--frames")?)?,
            "--warmup" => warmup = parse_num::<usize>(&need(&mut args, "--warmup")?)?,
            "--threads" => threads = parse_num::<usize>(&need(&mut args, "--threads")?)?,
            "--no-loop" => looping = false,
            "--canvas" => canvas = parse_canvas(&need(&mut args, "--canvas")?)?,
            "--view" => {
                let raw = need(&mut args, "--view")?;
                let parts: Vec<_> = raw.split(',').collect();
                if parts.len() != 3 {
                    return Err(format!("--view needs SCALE,X,Y, got `{raw}`").into());
                }
                view = Some(soft::ViewTransform {
                    scale: parse_num(parts[0])?,
                    offset: [parse_num(parts[1])?, parse_num(parts[2])?],
                });
            }
            other => return Err(format!("unknown argument `{other}`").into()),
        }
    }

    let load_start = Instant::now();
    let mut emote = Emote::load(&path, key, motion.as_deref())?;
    let load_ms = load_start.elapsed().as_secs_f64() * 1000.0;
    let texture_start = Instant::now();
    let textures = emote.textures()?;
    let texture_ms = texture_start.elapsed().as_secs_f64() * 1000.0;
    soft::set_thread_count(threads);
    let texture_pixels: usize = textures.values().map(|t| t.pixel_count()).sum();

    if let Some(name) = timeline.as_deref() {
        let mode = if looping {
            TimelinePlayMode::LOOP
        } else {
            TimelinePlayMode::ONCE
        };
        emote.play_timeline(name, mode)?;
        if looping {
            if let Some(begin) = emote.runtime.timeline_loop_begin(name) {
                emote.runtime.set_timeline_time(name, begin)?;
            }
            if let (Some(begin), Some(end)) = (
                emote.runtime.timeline_loop_begin(name),
                emote.runtime.timeline_loop_end(name),
            ) {
                let ticks = (end - begin).max(0.0);
                let mut remaining = ticks;
                while remaining > 0.0 {
                    let step = remaining.min(1.0);
                    emote.advance_ticks(step)?;
                    remaining -= step;
                }
            }
        }
    }

    let view = view.unwrap_or_else(|| soft::ViewTransform::fit(&emote.plan(canvas), 20.0));

    let mut tick_times = Vec::with_capacity(frames);
    let mut plan_times = Vec::with_capacity(frames);
    let mut raster_times = Vec::with_capacity(frames);
    let mut total_times = Vec::with_capacity(frames);
    let mut covered = 0usize;
    let mut triangles = 0usize;

    for index in 0..warmup + frames {
        let total_start = Instant::now();

        let tick_start = Instant::now();
        emote.advance_ticks(1.0)?;
        let tick = tick_start.elapsed();

        let plan_start = Instant::now();
        let plan = emote.plan(canvas);
        let plan_elapsed = plan_start.elapsed();

        let raster_start = Instant::now();
        let (image, stats) = soft::render_with_textures(&plan, &textures, view)?;
        let raster = raster_start.elapsed();

        let total = total_start.elapsed();
        if index >= warmup {
            tick_times.push(tick);
            plan_times.push(plan_elapsed);
            raster_times.push(raster);
            total_times.push(total);
            covered = image.covered_pixels();
            triangles = stats.triangles;
        }
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "model": path,
            "canvas": canvas,
            "frames": frames,
            "warmup": warmup,
            "threads": soft::effective_threads(canvas[0], canvas[1]),
            "threads_setting": soft::thread_count(),
            "load_ms": round(load_ms),
            "texture_decode_ms": round(texture_ms),
            "texture_pixels": texture_pixels,
            "view": { "scale": view.scale, "offset": view.offset },
            "covered_pixels": covered,
            "triangles": triangles,
            "tick_ms": summary(&mut tick_times),
            "plan_ms": summary(&mut plan_times),
            "raster_ms": summary(&mut raster_times),
            "total_ms": summary(&mut total_times),
            "fps": {
                "mean": round(1000.0 / mean(&total_times)),
                "p95_frame": round(1000.0 / percentile(&total_times, 0.95)),
            },
            "budget_60fps": {
                "mean_ok": mean(&total_times) <= 16.667,
                "p95_ok": percentile(&total_times, 0.95) <= 16.667,
            },
        }))?
    );
    Ok(())
}

fn summary(times: &mut [Duration]) -> serde_json::Value {
    times.sort_unstable();
    json!({
        "mean": round(mean(times)),
        "p50": round(percentile(times, 0.50)),
        "p95": round(percentile(times, 0.95)),
        "min": round(times.first().map_or(0.0, |t| t.as_secs_f64() * 1000.0)),
        "max": round(times.last().map_or(0.0, |t| t.as_secs_f64() * 1000.0)),
    })
}

fn mean(times: &[Duration]) -> f64 {
    if times.is_empty() {
        return 0.0;
    }
    times.iter().map(|t| t.as_secs_f64() * 1000.0).sum::<f64>() / times.len() as f64
}

fn percentile(times: &[Duration], p: f64) -> f64 {
    if times.is_empty() {
        return 0.0;
    }
    let index = ((times.len() as f64 - 1.0) * p).round() as usize;
    times[index].as_secs_f64() * 1000.0
}

fn round(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn parse_canvas(raw: &str) -> Result<[u32; 2], String> {
    let (w, h) = raw
        .split_once(['x', 'X', ','])
        .ok_or_else(|| format!("--canvas needs WxH, got `{raw}`"))?;
    Ok([parse_num::<u32>(w)?.max(1), parse_num::<u32>(h)?.max(1)])
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
