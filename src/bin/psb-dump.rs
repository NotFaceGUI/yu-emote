//! Deterministic E-mote model dump.
//!
//! ```text
//! psb-dump <model.psb> [--key HEX] [--raw] [--resources DIR] [--decode]
//! ```
//!
//! - `--raw` adds the complete PSB tree under `model.raw`.
//! - `--resources DIR` writes every embedded resource as `resource-NNN.bin`.
//! - `--decode` additionally writes each decoded texture page as
//!   `page-<name>.rgba8` plus dimensions in the JSON header.
//!
//! PSB `Resource(n)` values are rendered as `{"resource": n}` so the output
//! is unambiguous.

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use serde_json::{Value, json};
use yu_emote::psb::{PsbValue, PsbValue as ValueTree};
use yu_emote::{Emote, PsbDocument};

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
        eprintln!("usage: psb-dump <model.psb> [--key HEX] [--raw] [--resources DIR] [--decode]");
        return Ok(());
    };
    let mut key = None;
    let mut raw = false;
    let mut resources: Option<PathBuf> = None;
    let mut decode = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--key" => key = Some(parse_key(&need(&mut args, "--key")?)?),
            "--raw" => raw = true,
            "--resources" => resources = Some(PathBuf::from(need(&mut args, "--resources")?)),
            "--decode" => decode = true,
            other => return Err(format!("unknown argument `{other}`").into()),
        }
    }

    let document = PsbDocument::read(&path, key)?;
    let mut header = json!({
        "version": document.version(),
        "encrypted": document.encrypted(),
        "key": key,
        "resources": document.resources(),
        "extra_resources": document.extra_resources(),
        "spec": spec_of(document.root()),
        "top_level": top_level(document.root()),
    });

    if let Some(dir) = &resources {
        fs::create_dir_all(dir)?;
        for index in 0..document.resources() {
            if let Some(bytes) = document.resource(index) {
                fs::write(dir.join(format!("resource-{index:03}.bin")), bytes)?;
            }
        }
    }

    if decode {
        let emote = Emote::load(&path, key, None)?;
        let textures = emote.textures()?;
        let mut pages = Vec::new();
        let dir = resources.clone().unwrap_or_else(|| PathBuf::from("."));
        fs::create_dir_all(&dir)?;
        for (index, texture) in &textures {
            let name = format!("page-{index:03}");
            fs::write(dir.join(format!("{name}.rgba8")), &texture.rgba8)?;
            pages.push(json!({
                "resource": index,
                "file": format!("{name}.rgba8"),
                "width": texture.width,
                "height": texture.height,
            }));
        }
        header["pages"] = Value::Array(pages);
    }

    if raw {
        header["model"] = json!({ "raw": value_to_json(document.root()) });
    }

    println!("{}", serde_json::to_string_pretty(&header)?);
    Ok(())
}

fn top_level(root: &ValueTree) -> Vec<&str> {
    root.as_object()
        .map(|fields| fields.iter().map(|(name, _)| name.as_str()).collect())
        .unwrap_or_default()
}

fn value_to_json(value: &PsbValue) -> Value {
    match value {
        PsbValue::Null => Value::Null,
        PsbValue::Bool(value) => json!(value),
        PsbValue::Int(value) => json!(value),
        PsbValue::Float(value) => json!(value),
        PsbValue::Double(value) => json!(value),
        PsbValue::String(value) => json!(value),
        PsbValue::Resource(index) => json!({ "resource": index }),
        PsbValue::ExtraResource(index) => json!({ "extra_resource": index }),
        PsbValue::List(values) => Value::Array(values.iter().map(value_to_json).collect()),
        PsbValue::Object(fields) => {
            let mut map = serde_json::Map::new();
            for (name, value) in fields {
                map.insert(name.clone(), value_to_json(value));
            }
            Value::Object(map)
        }
        PsbValue::Compiler(tag) => json!({ "compiler": format!("{tag:?}") }),
    }
}

fn spec_of(root: &ValueTree) -> Option<String> {
    root.as_object()?
        .iter()
        .find(|(name, _)| name == "spec")
        .and_then(|(_, value)| value.as_str())
        .map(str::to_owned)
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
