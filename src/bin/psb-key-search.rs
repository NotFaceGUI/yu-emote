//! `psb-key-search`: recover the single 32-bit E-mote key of a body-encrypted
//! PSB file.
//!
//! The search is provided by the vendored `eluna` runtime: candidates are
//! filtered by the PSB btree headers and root object tag before a full parse.
//! The recovered key is a **per-game** value; it is never hard-coded here.
//!
//! ```text
//! psb-key-search <model.psb> [threads] [--start HEX] [--end HEX]
//! ```

use std::fs;
use std::process::ExitCode;

use yu_emote::psb::{PsbBruteforceOptions, bruteforce_emote_key};

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
        eprintln!("usage: psb-key-search <model.psb> [threads] [--start HEX] [--end HEX]");
        return Ok(());
    };
    let mut threads = 0usize;
    let mut options = PsbBruteforceOptions::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--start" => options.start_key = parse_key(&need(&mut args, "--start")?)?,
            "--end" => options.end_key = parse_key(&need(&mut args, "--end")?)?,
            other => {
                threads = other
                    .parse()
                    .map_err(|_| format!("invalid thread count `{other}`"))?;
            }
        }
    }
    options.threads = threads;

    let data = fs::read(&path)?;
    let started = std::time::Instant::now();
    match bruteforce_emote_key(&data, options)? {
        Some(result) => {
            println!(
                "key=0x{:08X} tested={} elapsed_ms={}",
                result.key,
                result.tested_keys,
                started.elapsed().as_millis()
            );
            Ok(())
        }
        None => Err(format!(
            "no key found in range 0x{:08X}..=0x{:08X} after {} tests",
            options.start_key, options.end_key, "all"
        )
        .into()),
    }
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
