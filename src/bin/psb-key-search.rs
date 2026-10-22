//! `psb-key-search`: recover the single 32-bit E-mote key of a body-encrypted
//! PSB file.
//!
//! The search uses the documented 6-word PSB stream schedule. Candidates are
//! filtered by the name-btree array headers and the root object tag before a
//! full parse; this is the same key space public tooling brute-forces.
//!
//! Usage: `psb-key-search <model.psb> [threads]`

use std::fs;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use yu_emote::psb::{PsbHeader, PsbStreamCipher};

fn parse_array(pt: &[u8], off: usize, total: usize) -> Option<(u64, u8, usize, usize)> {
    let ty = *pt.get(off)?;
    if !(0x0D..=0x14).contains(&ty) {
        return None;
    }
    let len_n = (ty - 0x0C) as usize;
    let mut len: u64 = 0;
    for i in 0..len_n {
        len |= (*pt.get(off + 1 + i)? as u64) << (8 * i);
    }
    let item_ty = *pt.get(off + 1 + len_n)?;
    if !(0x0D..=0x14).contains(&item_ty) {
        return None;
    }
    let item = item_ty - 0x0C;
    let body = (len as usize).checked_mul(item as usize)?;
    let total_size = 1 + len_n + 1 + body;
    if total_size > total {
        return None;
    }
    Some((len, item, total_size, off + 1 + len_n + 1))
}

fn values_ok(pt: &[u8], off: usize, len: u64, item: u8) -> bool {
    let n = (len as usize).min(1024);
    for i in 0..n {
        let mut v: u64 = 0;
        for b in 0..item as usize {
            let Some(&byte) = pt.get(off + i * item as usize + b) else {
                return false;
            };
            v |= (byte as u64) << (8 * b);
        }
        if v > 8_000_000 {
            return false;
        }
    }
    true
}

fn candidate_ok(body: &[u8], key: u32, root_rel: usize) -> bool {
    let n = root_rel + 1;
    if n > body.len() {
        return false;
    }
    let mut buf = vec![0u8; n];
    buf.copy_from_slice(&body[..n]);
    PsbStreamCipher::new(key).apply(&mut buf);

    let Some((l1, i1, s1, d1)) = parse_array(&buf, 0, body.len()) else {
        return false;
    };
    if l1 == 0 || l1 > 1_000_000 || i1 == 0 || i1 > 4 || !values_ok(&buf, d1, l1, i1) {
        return false;
    }
    let Some((l2, i2, s2, d2)) = parse_array(&buf, s1, body.len()) else {
        return false;
    };
    if l2 == 0 || l2 > 8_000_000 || i2 == 0 || i2 > 4 || !values_ok(&buf, d2, l2.min(1024), i2) {
        return false;
    }
    let Some((l3, i3, s3, d3)) = parse_array(&buf, s1 + s2, body.len()) else {
        return false;
    };
    if l3 == 0 || l3 > 1_000_000 || i3 == 0 || i3 > 4 || !values_ok(&buf, d3, l3.min(1024), i3) {
        return false;
    }
    if s1 + s2 + s3 > root_rel {
        return false;
    }
    buf[root_rel] == 0x21
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: psb-key-search <model.psb> [threads]");
        return ExitCode::FAILURE;
    };
    let threads = args
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
        });

    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("psb-key-search: {error}");
            return ExitCode::FAILURE;
        }
    };
    let header = match PsbHeader::parse(&bytes) {
        Ok(header) => header,
        Err(error) => {
            eprintln!("psb-key-search: {error}");
            return ExitCode::FAILURE;
        }
    };
    if !header.body_encrypted(&bytes) {
        println!("not encrypted; no key needed");
        return ExitCode::SUCCESS;
    }
    let start = header.name_offset as usize;
    let end = header.resource_offset as usize;
    let root = header.entrypoint as usize;
    if start >= end || end > bytes.len() || root < start || root >= end {
        eprintln!("psb-key-search: invalid header offsets");
        return ExitCode::FAILURE;
    }
    let body = &bytes[start..end];
    let root_rel = root - start;
    eprintln!("body {start:#x}..{end:#x}, root {root:#x}, threads {threads}");

    let found = AtomicBool::new(false);
    let found_key = AtomicU32::new(0);
    let total: u64 = 1u64 << 32;
    let chunk = total.div_ceil(threads as u64);

    std::thread::scope(|scope| {
        for t in 0..threads {
            let start_key = (t as u64) * chunk;
            if start_key >= total {
                break;
            }
            let end_key = (start_key + chunk).min(total);
            let found = &found;
            let found_key = &found_key;
            scope.spawn(move || {
                let mut k = start_key;
                while k < end_key {
                    if found.load(Ordering::Relaxed) {
                        break;
                    }
                    let key = k as u32;
                    let mut stream = [0u8; 4];
                    PsbStreamCipher::new(key).apply(&mut stream);
                    let p0 = body[0] ^ stream[0];
                    if (0x0D..=0x14).contains(&p0) {
                        let len_n = (p0 - 0x0C) as usize;
                        let mut probe = [0u8; 10];
                        PsbStreamCipher::new(key).apply(&mut probe);
                        let mut len: u64 = 0;
                        for i in 0..len_n {
                            len |= ((body[1 + i] ^ probe[1 + i]) as u64) << (8 * i);
                        }
                        let item_ty = body[1 + len_n] ^ probe[1 + len_n];
                        if (0x0D..=0x14).contains(&item_ty) {
                            let item = item_ty - 0x0C;
                            let total1 =
                                1 + len_n + 1 + (len as usize).saturating_mul(item as usize);
                            if len > 0
                                && len <= 1_000_000
                                && total1 <= root_rel
                                && candidate_ok(body, key, root_rel)
                            {
                                found_key.store(key, Ordering::Relaxed);
                                found.store(true, Ordering::Release);
                                break;
                            }
                        }
                    }
                    k += 1;
                }
            });
        }
    });

    if found.load(Ordering::Acquire) {
        println!("found key: 0x{:08x}", found_key.load(Ordering::Relaxed));
        ExitCode::SUCCESS
    } else {
        println!("key not found");
        ExitCode::FAILURE
    }
}
