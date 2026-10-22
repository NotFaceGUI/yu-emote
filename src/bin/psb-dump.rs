//! Deterministic E-mote model dump.
//!
//! ```text
//! psb-dump <model.psb> [--key 0x175058c9] [--raw] [--resources DIR] [--decode]
//! ```
//!
//! - `--raw` adds the complete sorted PSB tree under `model.raw`.
//! - `--resources DIR` writes every embedded resource as `resource-NNN.bin`.
//! - `--decode` additionally decodes each texture page to `page-<name>.rgba8`
//!   in `DIR` (raw RGBA8, width/height are in the JSON header).

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use serde::Serialize;
use sha2::{Digest, Sha256};
use yu_emote::psb::{self, PsbDocument};
use yu_emote::schema::Model;
use yu_emote::texture;

#[derive(Serialize)]
struct ContainerFacts {
    version: u16,
    encrypted: bool,
    resources: usize,
    extra_resources: usize,
    key_used: bool,
}

#[derive(Serialize)]
struct ResourceFacts {
    index: usize,
    bytes: usize,
    sha256: String,
}

#[derive(Serialize)]
struct Dump {
    container: ContainerFacts,
    model: Model,
    resources: Vec<ResourceFacts>,
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: psb-dump <model.psb> [--key HEX] [--raw] [--resources DIR] [--decode]");
        return ExitCode::FAILURE;
    };
    let mut key = None;
    let mut include_raw = false;
    let mut resource_dir: Option<PathBuf> = None;
    let mut decode = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--key" => {
                let Some(value) = args.next() else {
                    eprintln!("--key needs a value");
                    return ExitCode::FAILURE;
                };
                match parse_key(&value) {
                    Ok(value) => key = Some(value),
                    Err(message) => {
                        eprintln!("{message}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "--raw" => include_raw = true,
            "--resources" => {
                let Some(value) = args.next() else {
                    eprintln!("--resources needs a directory");
                    return ExitCode::FAILURE;
                };
                resource_dir = Some(PathBuf::from(value));
            }
            "--decode" => decode = true,
            other => {
                eprintln!("unknown argument `{other}`");
                return ExitCode::FAILURE;
            }
        }
    }
    if decode && resource_dir.is_none() {
        eprintln!("--decode requires --resources DIR");
        return ExitCode::FAILURE;
    }

    let document = match psb::read_document_with_key(&path, key) {
        Ok(document) => document,
        Err(error) => {
            eprintln!("{path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut model = Model::from_document(&document);
    if !include_raw {
        model.raw = serde_json::Value::Null;
    }

    let resources = document
        .resource_data
        .iter()
        .enumerate()
        .map(|(index, bytes)| ResourceFacts {
            index,
            bytes: bytes.len(),
            sha256: hex(&Sha256::digest(bytes)),
        })
        .collect();

    if let Some(dir) = &resource_dir {
        if let Err(error) = fs::create_dir_all(dir) {
            eprintln!("{}: {error}", dir.display());
            return ExitCode::FAILURE;
        }
        for (index, bytes) in document.resource_data.iter().enumerate() {
            let path = dir.join(format!("resource-{index:03}.bin"));
            if let Err(error) = fs::write(&path, bytes) {
                eprintln!("{}: {error}", path.display());
                return ExitCode::FAILURE;
            }
        }
        if decode {
            if let Err(error) = decode_pages(&document, &model, dir) {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
        }
    }

    let dump = Dump {
        container: ContainerFacts {
            version: document.version,
            encrypted: document.encrypted,
            resources: document.resources,
            extra_resources: document.extra_resources,
            key_used: document.key.is_some(),
        },
        model,
        resources,
    };
    match serde_json::to_writer_pretty(std::io::stdout(), &dump) {
        Ok(()) => {
            println!();
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn decode_pages(
    document: &PsbDocument,
    model: &Model,
    dir: &std::path::Path,
) -> Result<(), String> {
    for page in &model.sources {
        let decoded = texture::decode_page(document, page)
            .map_err(|error| format!("{}: {error}", page.name))?;
        let safe_name: String = page
            .name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let path = dir.join(format!("page-{safe_name}.rgba8"));
        fs::write(&path, &decoded.rgba8).map_err(|error| format!("{}: {error}", path.display()))?;
        println!(
            "{}: {}x{} rgba8 ({} bytes)",
            path.display(),
            decoded.width,
            decoded.height,
            decoded.rgba8.len()
        );
    }
    Ok(())
}

fn parse_key(value: &str) -> Result<u32, String> {
    let value = value.trim();
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16).map_err(|error| format!("invalid key `{value}`: {error}"))
    } else {
        value
            .parse::<u32>()
            .map_err(|error| format!("invalid key `{value}`: {error}"))
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}
