//! Render E-mote frames with the wgpu backend and compare against the CPU
//! reference.
//!
//! ```text
//! psb-gpu <model.psb> [--key HEX] [--motion NAME] [--timeline NAME]
//!         [--time TICKS] [--canvas WxH] [--view SCALE,X,Y] [--var NAME=VALUE]...
//!         [--out FILE.png] [--compare] [--bench FRAMES]
//! ```
//!
//! `--compare` renders the same plan with `soft` and prints per-channel mean /
//! max differences. `--bench` reports both submit-only and submit+readback
//! timings; the kernel path only submits and composites the texture.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufWriter;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use serde_json::json;
use yu_emote::gpu::TARGET_FORMAT;
use yu_emote::{Emote, GpuRenderer, TimelinePlayMode, soft};

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
            "usage: psb-gpu <model.psb> [--key HEX] [--motion NAME] [--timeline NAME] \
             [--time TICKS] [--canvas WxH] [--view SCALE,X,Y] [--var NAME=VALUE]... \
             [--out FILE.png] [--compare] [--bench FRAMES]"
        );
        return Ok(());
    };
    let mut key = None;
    let mut motion = None;
    let mut timeline = None;
    let mut time_ticks = 0.0f32;
    let mut canvas = [800u32, 600u32];
    let mut view: Option<soft::ViewTransform> = None;
    let mut variables = BTreeMap::new();
    let mut output = None;
    let mut compare = false;
    let mut bench = 0usize;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--key" => key = Some(parse_key(&need(&mut args, "--key")?)?),
            "--motion" => motion = Some(need(&mut args, "--motion")?),
            "--timeline" => timeline = Some(need(&mut args, "--timeline")?),
            "--time" => time_ticks = parse_num(&need(&mut args, "--time")?)?,
            "--canvas" => canvas = parse_canvas(&need(&mut args, "--canvas")?)?,
            "--out" => output = Some(need(&mut args, "--out")?),
            "--compare" => compare = true,
            "--bench" => bench = parse_num::<usize>(&need(&mut args, "--bench")?)?,
            "--var" => {
                let spec = need(&mut args, "--var")?;
                let (name, value) = spec
                    .split_once('=')
                    .ok_or_else(|| format!("invalid --var `{spec}`"))?;
                variables.insert(name.to_owned(), parse_num::<f32>(value)? as f64);
            }
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

    let mut emote = Emote::load(&path, key, motion.as_deref())?;
    emote.set_variables(&variables)?;
    if let Some(name) = timeline.as_deref() {
        emote.play_timeline(name, TimelinePlayMode::LOOP)?;
        if let Some(begin) = emote.runtime.timeline_loop_begin(name) {
            emote.runtime.set_timeline_time(name, begin)?;
        }
    }
    advance(&mut emote, time_ticks)?;
    let textures = emote.textures()?;
    let plan = emote.plan(canvas);
    let view = view.unwrap_or_else(|| soft::ViewTransform::fit(&plan, 20.0));

    let (mut renderer, adapter) = GpuRenderer::headless()?;
    let render_start = Instant::now();
    let frame = renderer.render(&plan, &textures, view)?;
    let submit_ms = render_start.elapsed().as_secs_f64() * 1000.0;
    let read_start = Instant::now();
    let gpu_rgba = renderer.read_frame(&frame)?;
    let read_ms = read_start.elapsed().as_secs_f64() * 1000.0;

    let mut comparison = None;
    if compare {
        soft::set_thread_count(0);
        let (cpu, _) = soft::render_with_textures(&plan, &textures, view)?;
        let mut sum = [0.0f64; 4];
        let mut max = [0u8; 4];
        for (a, b) in cpu.rgba8.chunks_exact(4).zip(gpu_rgba.chunks_exact(4)) {
            for channel in 0..4 {
                let diff = (a[channel] as i32 - b[channel] as i32).unsigned_abs() as u8;
                sum[channel] += f64::from(diff);
                max[channel] = max[channel].max(diff);
            }
        }
        let pixels = cpu.rgba8.len() / 4;
        comparison = Some(json!({
            "mean": sum.map(|value| round(value / pixels as f64)),
            "max": max,
            "pixels": pixels,
        }));
    }

    if let Some(output) = output {
        write_png(&output, canvas[0], canvas[1], &gpu_rgba)?;
    }

    let mut benchmark = None;
    if bench > 0 {
        let mut submit_times = Vec::with_capacity(bench);
        let mut sync_times = Vec::with_capacity(bench);
        for _ in 0..bench {
            emote.advance_ticks(1.0)?;
            let plan = emote.plan(canvas);
            let started = Instant::now();
            let frame = renderer.render(&plan, &textures, view)?;
            submit_times.push(started.elapsed());
            let frame_read = renderer.read_frame(&frame)?;
            std::hint::black_box(frame_read);
            sync_times.push(started.elapsed());
        }
        benchmark = Some(json!({
            "frames": bench,
            "submit_ms": summary(&mut submit_times),
            "submit_and_readback_ms": summary(&mut sync_times),
        }));
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "model": path,
            "adapter": {
                "name": adapter.name,
                "backend": format!("{:?}", adapter.backend),
                "device_type": format!("{:?}", adapter.device_type),
                "driver": adapter.driver,
            },
            "format": format!("{TARGET_FORMAT:?}"),
            "canvas": canvas,
            "view": { "scale": view.scale, "offset": view.offset },
            "plan": plan.summary_json(),
            "stats": frame.stats,
            "submit_ms": round(submit_ms),
            "readback_ms": round(read_ms),
            "comparison": comparison,
            "benchmark": benchmark,
        }))?
    );
    Ok(())
}

fn advance(emote: &mut Emote, mut ticks: f32) -> Result<(), Box<dyn std::error::Error>> {
    while ticks > 0.0 {
        let step = ticks.min(1.0);
        emote.advance_ticks(step)?;
        ticks -= step;
    }
    Ok(())
}

fn summary(times: &mut [Duration]) -> serde_json::Value {
    times.sort_unstable();
    let mean = times
        .iter()
        .map(|time| time.as_secs_f64() * 1000.0)
        .sum::<f64>()
        / times.len() as f64;
    let at = |p: f64| times[((times.len() - 1) as f64 * p).round() as usize].as_secs_f64() * 1000.0;
    json!({
        "mean": round(mean),
        "p50": round(at(0.50)),
        "p95": round(at(0.95)),
        "min": round(times[0].as_secs_f64() * 1000.0),
        "max": round(times[times.len() - 1].as_secs_f64() * 1000.0),
        "fps": round(1000.0 / mean),
    })
}

fn write_png(
    path: &str,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::create(path)?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgba)?;
    Ok(())
}

fn need(args: &mut impl Iterator<Item = String>, option: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{option} needs a value"))
}

fn parse_canvas(raw: &str) -> Result<[u32; 2], String> {
    let (w, h) = raw
        .split_once(['x', 'X', ','])
        .ok_or_else(|| format!("--canvas needs WxH, got `{raw}`"))?;
    Ok([parse_num::<u32>(w)?.max(1), parse_num::<u32>(h)?.max(1)])
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

fn round(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}
