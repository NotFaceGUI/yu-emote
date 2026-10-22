//! Deterministic render-plan dump for one E-mote motion.
//!
//! ```text
//! psb-plan <model.psb> [--key HEX] [--object NAME] [--motion NAME]
//!          [--time FRAMES] [--var NAME=VALUE]... [--full]
//! ```
//!
//! Default output is a compact summary (item counts, vertex/triangle totals,
//! skipped reasons). `--full` emits the complete vertex/index payload.

use std::collections::BTreeMap;
use std::process::ExitCode;

use yu_emote::plan::RenderPlan;
use yu_emote::pose::Pose;
use yu_emote::psb;
use yu_emote::schema::Model;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!(
            "usage: psb-plan <model.psb> [--key HEX] [--object NAME] [--motion NAME] \
             [--time FRAMES] [--var NAME=VALUE]... [--full]"
        );
        return ExitCode::FAILURE;
    };
    let mut key = None;
    let mut object = None;
    let mut motion = None;
    let mut time = 0.0f64;
    let mut variables = BTreeMap::new();
    let mut full = false;
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
            "--time" => {
                let Some(value) = args.next() else {
                    eprintln!("--time needs a number");
                    return ExitCode::FAILURE;
                };
                match value.parse() {
                    Ok(value) => time = value,
                    Err(error) => {
                        eprintln!("invalid --time `{value}`: {error}");
                        return ExitCode::FAILURE;
                    }
                }
            }
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
            "--full" => full = true,
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
    let plan = RenderPlan::build(&model, &pose);
    let output = if full {
        serde_json::to_string_pretty(&plan)
    } else {
        serde_json::to_string_pretty(&plan.summary_json())
    };
    match output {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
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
