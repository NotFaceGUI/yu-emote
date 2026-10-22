//! Deterministic pose dump for one E-mote motion.
//!
//! ```text
//! psb-pose <model.psb> [--key HEX] [--object NAME] [--motion NAME]
//!          [--time FRAMES] [--step FRAMES] [--count N] [--var NAME=VALUE]...
//! ```
//!
//! Outputs the evaluated layer tree (frame state, local/world transforms,
//! opacity, visibility) as JSON. This is the M2 comparison surface against the
//! official runtime; it does not render.

use std::collections::BTreeMap;
use std::process::ExitCode;

use yu_emote::pose::Pose;
use yu_emote::psb;
use yu_emote::schema::Model;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!(
            "usage: psb-pose <model.psb> [--key HEX] [--object NAME] [--motion NAME] \
             [--time FRAMES] [--step FRAMES] [--count N] [--var NAME=VALUE]..."
        );
        return ExitCode::FAILURE;
    };
    let mut key = None;
    let mut object = None;
    let mut motion = None;
    let mut time = 0.0f64;
    let mut step = 1.0f64;
    let mut count = 1usize;
    let mut variables = BTreeMap::new();
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
            "--time" => match parse_f64(&mut args, "--time") {
                Ok(value) => time = value,
                Err(code) => return code,
            },
            "--step" => match parse_f64(&mut args, "--step") {
                Ok(value) => step = value,
                Err(code) => return code,
            },
            "--count" => {
                let Some(value) = args.next() else {
                    eprintln!("--count needs a number");
                    return ExitCode::FAILURE;
                };
                match value.parse() {
                    Ok(value) => count = value,
                    Err(error) => {
                        eprintln!("invalid --count `{value}`: {error}");
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
    let object = match object {
        Some(object) => object,
        None => model
            .metadata
            .character
            .clone()
            .or_else(|| model.objects.keys().next().cloned())
            .unwrap_or_default(),
    };
    let motion = match motion {
        Some(motion) => motion,
        None => model
            .metadata
            .base_motion
            .clone()
            .or_else(|| {
                model
                    .objects
                    .get(&object)
                    .and_then(|object| object.motions.keys().next().cloned())
            })
            .unwrap_or_default(),
    };

    let mut poses = Vec::with_capacity(count);
    for index in 0..count {
        let sample = time + step * index as f64;
        let Some(pose) = Pose::evaluate(&model, &object, &motion, sample, &variables) else {
            eprintln!("no motion `{object}/{motion}`");
            return ExitCode::FAILURE;
        };
        poses.push(pose);
    }

    let output = if count == 1 {
        serde_json::to_string_pretty(&poses[0])
    } else {
        serde_json::to_string_pretty(&poses)
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

fn parse_f64(args: &mut impl Iterator<Item = String>, name: &str) -> Result<f64, ExitCode> {
    let Some(value) = args.next() else {
        eprintln!("{name} needs a number");
        return Err(ExitCode::FAILURE);
    };
    value.parse().map_err(|error| {
        eprintln!("invalid {name} `{value}`: {error}");
        ExitCode::FAILURE
    })
}
