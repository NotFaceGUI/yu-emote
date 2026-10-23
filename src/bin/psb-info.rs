//! `psb-info`: print a deterministic JSON summary of an E-mote PSB/MDF file.
//!
//! ```text
//! psb-info <model.psb> [--key HEX] [--motion NAME] [--no-scene]
//! ```

use std::process::ExitCode;

use serde_json::json;
use yu_emote::Emote;

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
        eprintln!("usage: psb-info <model.psb> [--key HEX] [--motion NAME] [--no-scene]");
        return Ok(());
    };
    let mut key = None;
    let mut motion = None;
    let mut scene = true;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--key" => key = Some(parse_key(&need(&mut args, "--key")?)?),
            "--motion" => motion = Some(need(&mut args, "--motion")?),
            "--no-scene" => scene = false,
            other => return Err(format!("unknown argument `{other}`").into()),
        }
    }

    let emote = Emote::load(&path, key, motion.as_deref())?;
    let document = &emote.document;
    let root = document.root();
    let top_level: Vec<&str> = root
        .as_object()
        .map(|fields| fields.iter().map(|(name, _)| name.as_str()).collect())
        .unwrap_or_default();
    let textures: Vec<_> = emote
        .runtime
        .texture_sources()
        .values()
        .map(|source| {
            json!({
                "name": source.name,
                "resource": source.resource_index,
                "width": source.width,
                "height": source.height,
                "format": source.format,
                "compress": source.compress,
                "bit_count": source.bit_count,
                "icons": source.icons.len(),
            })
        })
        .collect();
    let scene_summary = if scene {
        Some(json!({
            "sprites": emote.scene().sprites.len(),
            "layers": emote.scene().layer_states.len(),
            "draw_frame_info": emote.scene().draw_frame_info.len(),
            "bounds": emote.scene().bounds.map(|bounds| json!([
                bounds.min_x, bounds.min_y, bounds.max_x, bounds.max_y
            ])),
            "composite_mask_owners": emote.scene().composite_mask_owners.len(),
        }))
    } else {
        None
    };
    let timeline_names = emote.timeline_names();
    let timeline_details: Vec<_> = timeline_names
        .iter()
        .map(|name| {
            json!({
                "name": name,
                "frames": emote.runtime.timeline_total_frame_count(name),
                "loop": emote.runtime.is_loop_timeline(name),
                "loop_begin": emote.runtime.timeline_loop_begin(name),
                "loop_end": emote.runtime.timeline_loop_end(name),
            })
        })
        .collect();

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "version": document.version(),
            "encrypted": document.encrypted(),
            "key": key,
            "resources": document.resources(),
            "extra_resources": document.extra_resources(),
            "spec": emote.spec(),
            "screen_size": emote.screen_size(),
            "top_level": top_level,
            "motions": emote.motion_names()?,
            "timelines": timeline_names,
            "timeline_details": timeline_details,
            "variables": emote.variables().keys().collect::<Vec<_>>(),
            "textures": textures,
            "scene": scene_summary,
        }))?
    );
    Ok(())
}

fn need(args: &mut impl Iterator<Item = String>, option: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{option} needs a value"))
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
