//! PSB container access for the YU-RIS E-mote path.
//!
//! Binary parsing, MDF/LZ4 normalization and the body cipher live in the
//! vendored `eluna` runtime (`vendor/eluna`). This module only wraps them in
//! the small shape `yu-emote` needs: normalized bytes plus the decoded tree,
//! with an optional per-game E-mote key.
//!
//! The key is a per-game 32-bit value. This crate never hard-codes it; callers
//! pass it explicitly (the CLI tools read `--key` or `YURIS_EMOTE_KEY`).

use std::fs;
use std::path::Path;

pub use eluna::psb::{
    PsbBruteforceOptions, PsbBruteforceResult, PsbDecryptionKey, PsbError, PsbFile,
    PsbNormalizeOptions, PsbValue, bruteforce_emote_key,
};

use crate::error::EmoteError;

/// A parsed PSB, together with the normalized bytes its resources point into.
#[derive(Debug)]
pub struct PsbDocument {
    /// Input after MDF/LZ4/container normalization and body decryption.
    pub data: Vec<u8>,
    /// Decoded PSB tree and resource ranges.
    pub psb: PsbFile,
    /// Per-game E-mote key, when body decryption was requested.
    pub key: Option<u32>,
}

impl PsbDocument {
    /// Read a PSB/MDF file and decode it with an optional E-mote key.
    pub fn read(path: impl AsRef<Path>, key: Option<u32>) -> Result<Self, EmoteError> {
        Self::from_bytes(&fs::read(path)?, key)
    }

    /// Decode PSB/MDF bytes with an optional E-mote key.
    pub fn from_bytes(bytes: &[u8], key: Option<u32>) -> Result<Self, EmoteError> {
        let options = PsbNormalizeOptions {
            decrypt_key: key.map(PsbDecryptionKey::emote_key),
            ..PsbNormalizeOptions::default()
        };
        let (data, psb) = PsbFile::parse_normalized(bytes, &options)?;
        Ok(Self { data, psb, key })
    }

    /// Normalized container bytes (the resource ranges index into this).
    pub fn normalized(&self) -> &[u8] {
        &self.data
    }

    /// Decoded root value.
    pub fn root(&self) -> &PsbValue {
        &self.psb.root
    }

    /// PSB format version.
    pub fn version(&self) -> u16 {
        self.psb.version
    }

    /// Whether the input flags marked the container as encrypted.
    pub fn encrypted(&self) -> bool {
        self.psb.encrypted
    }

    /// Number of embedded resources.
    pub fn resources(&self) -> usize {
        self.psb.resources.len()
    }

    /// Number of extra resources (PSB v4+).
    pub fn extra_resources(&self) -> usize {
        self.psb.extra_resources.len()
    }

    /// Borrow one embedded resource.
    pub fn resource(&self, index: usize) -> Option<&[u8]> {
        self.psb.resource_bytes(&self.data, index)
    }

    /// Borrow one extra resource.
    pub fn extra_resource(&self, index: usize) -> Option<&[u8]> {
        self.psb.extra_resource_bytes(&self.data, index)
    }
}
