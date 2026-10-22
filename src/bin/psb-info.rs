//! `psb-info`: print a deterministic JSON summary of an E-mote PSB/MDF file.

use std::process::ExitCode;

use yu_emote::model::ModelSummary;
use yu_emote::psb;

fn parse_key(raw: &str) -> Option<u32> {
    let trimmed = raw.trim();
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16).ok()
    } else {
        trimmed.parse().ok()
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: psb-info <model.psb|model.mdf> [--key 0x...]");
        return ExitCode::FAILURE;
    };
    let mut key = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--key" => {
                let Some(value) = args.next() else {
                    eprintln!("psb-info: --key requires a value");
                    return ExitCode::FAILURE;
                };
                match parse_key(&value) {
                    Some(value) => key = Some(value),
                    None => {
                        eprintln!("psb-info: invalid key `{value}`");
                        return ExitCode::FAILURE;
                    }
                }
            }
            other => {
                eprintln!("psb-info: unknown argument `{other}`");
                return ExitCode::FAILURE;
            }
        }
    }

    match psb::read_document_with_key(&path, key) {
        Ok(document) => {
            let summary = ModelSummary::from_document(&document);
            match serde_json::to_string_pretty(&summary) {
                Ok(json) => {
                    println!("{json}");
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("psb-info: JSON encode failed: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Err(error) => {
            eprintln!("psb-info: {error}");
            ExitCode::FAILURE
        }
    }
}
