//! PSB/MDF container access.
//!
//! The binary value format is implemented by the MIT-licensed `emote-psb`
//! crate. This module adds:
//!
//! - file-level auto-detection (PSB vs. MDF);
//! - E-mote body decryption (the PSB v1/v2 "body-only" encryption used by
//!   YU-RIS games), with an explicit key;
//! - header facts exposed together with the decoded root value.
//!
//! ## Body encryption
//!
//! E-mote PSB files usually have a clean header (`flags == 0`) but an encrypted
//! body; the root byte at `entrypoint` is not `0x21` (`object`). The cipher is
//! the same 6-word stream used by public tooling (FreeMote's
//! `PsbStreamContext`, eluna's `PsbDecryptionKey::emote_key`):
//!
//! ```text
//! state = [0x075BCD15, 0x159A55E5, 0x1F123BB5, key, 0, 0]
//! per byte: regenerate state[4] when it reaches 0, then plain ^= state[4] & 0xFF
//! ```
//!
//! This is an independent implementation of the documented stream; it is not
//! copied from FreeMote (CC BY-NC-SA) or eluna (no license).

use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use emote_psb::mdf::MdfReader;
use emote_psb::psb::read::PsbFile;
use emote_psb::value::PsbValue;

use crate::error::EmoteError;

const PSB_HEADER_MIN: usize = 0x28;
const PSB_FLAG_HEADER_ENCRYPTED: u16 = 0x0001;
const PSB_FLAG_BODY_ENCRYPTED: u16 = 0x0002;
const PSB_TYPE_OBJECT: u8 = 0x21;

/// Parsed PSB document plus its container facts.
///
/// Binary resources (`Resource`/`ExtraResource` nodes) are kept as indices;
/// their bytes are not loaded into memory. Use the underlying `emote-psb`
/// stream API if byte access is needed.
#[derive(Debug)]
pub struct PsbDocument {
    /// PSB format version (2 for the YU-RIS `emote_test.psb` sample).
    pub version: u16,
    /// Whether the PSB header marks the payload as encrypted.
    pub encrypted: bool,
    /// Number of embedded binary resources (texture atlas pages, etc.).
    pub resources: usize,
    /// Number of extra resources (version 4+).
    pub extra_resources: usize,
    /// Decoded root value.
    pub root: PsbValue,
    /// Key used to decrypt the body, if any.
    pub key: Option<u32>,
}

/// Header facts needed for decryption and body range checks.
#[derive(Debug, Clone, Copy)]
pub struct PsbHeader {
    /// PSB format version.
    pub version: u16,
    /// Header flags (`1` = header encrypted, `2` = body encrypted).
    pub flags: u16,
    /// Name btree offset.
    pub name_offset: u32,
    /// Resource offset table offset; the body ends here.
    pub resource_offset: u32,
    /// Root value offset.
    pub entrypoint: u32,
}

impl PsbHeader {
    /// Parse the fixed 0x28-byte PSB v1/v2 header.
    ///
    /// # Errors
    ///
    /// Returns [`EmoteError::InvalidSignature`] or [`EmoteError::InvalidHeader`]
    /// when the buffer is not a well-formed header.
    pub fn parse(bytes: &[u8]) -> Result<Self, EmoteError> {
        if bytes.len() < PSB_HEADER_MIN {
            return Err(EmoteError::InvalidHeader);
        }
        if u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) != emote_psb::PSB_SIGNATURE
        {
            return Err(EmoteError::InvalidSignature);
        }
        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        let flags = u16::from_le_bytes([bytes[6], bytes[7]]);
        let name_offset = u32::from_le_bytes([bytes[0x0C], bytes[0x0D], bytes[0x0E], bytes[0x0F]]);
        let resource_offset =
            u32::from_le_bytes([bytes[0x18], bytes[0x19], bytes[0x1A], bytes[0x1B]]);
        let entrypoint = u32::from_le_bytes([bytes[0x24], bytes[0x25], bytes[0x26], bytes[0x27]]);
        Ok(Self {
            version,
            flags,
            name_offset,
            resource_offset,
            entrypoint,
        })
    }

    /// Whether the value body is encrypted.
    ///
    /// E-mote PSB v2 files often keep `flags == 0` and still encrypt the body;
    /// the reliable check is the root tag at `entrypoint`.
    pub fn body_encrypted(self, bytes: &[u8]) -> bool {
        self.flags & PSB_FLAG_BODY_ENCRYPTED != 0
            || bytes.get(self.entrypoint as usize) != Some(&PSB_TYPE_OBJECT)
    }
}

/// E-mote PSB body stream cipher.
///
/// Byte-compatible with FreeMote's `PsbStreamContext` and eluna's
/// `PsbDecryptor`; see the module docs for the state schedule.
#[derive(Debug, Clone, Copy)]
pub struct PsbStreamCipher {
    key1: u32,
    key2: u32,
    key3: u32,
    key4: u32,
    current: u32,
}

impl PsbStreamCipher {
    /// Create a cipher for one E-mote key (the 4th state word).
    pub const fn new(key: u32) -> Self {
        Self {
            key1: 0x075B_CD15,
            key2: 0x159A_55E5,
            key3: 0x1F12_3BB5,
            key4: key,
            current: 0,
        }
    }

    /// XOR `data` in place (encryption and decryption are the same operation).
    pub fn apply(&mut self, data: &mut [u8]) {
        for byte in data {
            if self.current == 0 {
                self.refill();
            }
            *byte ^= self.current as u8;
            self.current >>= 8;
        }
    }

    fn refill(&mut self) {
        let a = self.key1 ^ (self.key1 << 11);
        let b = self.key4;
        let c = a ^ b ^ ((a ^ (b >> 11)) >> 8);
        self.key1 = self.key2;
        self.key2 = self.key3;
        self.key3 = b;
        self.key4 = c;
        self.current = c;
    }
}

/// Read a plain PSB stream.
///
/// # Errors
///
/// Returns [`EmoteError::PsbOpen`] or [`EmoteError::PsbDecode`] if the header
/// or value tree cannot be read.
pub fn read_psb<R: BufRead + Seek>(reader: R) -> Result<PsbDocument, EmoteError> {
    let mut file = PsbFile::open(reader)?;
    let version = file.version;
    let encrypted = file.encrypted;
    let resources = file.resources();
    let extra_resources = file.extra_resources();
    let root: PsbValue = file.deserialize_root()?;
    Ok(PsbDocument {
        version,
        encrypted,
        resources,
        extra_resources,
        root,
        key: None,
    })
}

/// Read a file as PSB or MDF, detected by its 4-byte signature.
///
/// Encrypted bodies require [`read_document_with_key`].
///
/// # Errors
///
/// Returns [`EmoteError::InvalidSignature`] for an unknown container, plus the
/// errors documented by [`read_psb`].
pub fn read_document(path: impl AsRef<Path>) -> Result<PsbDocument, EmoteError> {
    read_document_with_key(path, None)
}

/// Read a file as PSB or MDF, decrypting a body-encrypted PSB with `key`.
///
/// `key` is the E-mote per-game 32-bit key. Plain PSB files ignore it.
pub fn read_document_with_key(
    path: impl AsRef<Path>,
    key: Option<u32>,
) -> Result<PsbDocument, EmoteError> {
    let bytes = read_container(path.as_ref())?;
    read_psb_bytes(bytes, key)
}

/// Parse an in-memory PSB, decrypting its body when needed.
pub fn read_psb_bytes(bytes: Vec<u8>, key: Option<u32>) -> Result<PsbDocument, EmoteError> {
    let header = PsbHeader::parse(&bytes)?;
    let encrypted = header.body_encrypted(&bytes);
    if header.flags & PSB_FLAG_HEADER_ENCRYPTED != 0 {
        return Err(EmoteError::EncryptedHeaderUnsupported);
    }
    let mut bytes = bytes;
    if encrypted {
        let Some(key) = key else {
            return Err(EmoteError::EncryptedPsbRequiresKey);
        };
        let start = header.name_offset as usize;
        let end = header.resource_offset as usize;
        if start >= end || end > bytes.len() {
            return Err(EmoteError::InvalidHeader);
        }
        PsbStreamCipher::new(key).apply(&mut bytes[start..end]);
    }
    let mut document = read_psb(Cursor::new(bytes))?;
    document.key = if encrypted { key } else { None };
    Ok(document)
}

fn read_container(path: &Path) -> Result<Vec<u8>, EmoteError> {
    let mut file = File::open(path)?;
    let mut magic = [0u8; 4];
    if file.read_exact(&mut magic).is_err() {
        return Err(EmoteError::InvalidSignature);
    }
    file.seek(SeekFrom::Start(0))?;
    match u32::from_le_bytes(magic) {
        emote_psb::PSB_SIGNATURE => {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            Ok(bytes)
        }
        emote_psb::PSB_MDF_SIGNATURE => {
            let mut mdf = MdfReader::open(BufReader::new(file))?;
            let mut bytes = Vec::with_capacity(mdf.size() as usize);
            mdf.read_to_end(&mut bytes)?;
            Ok(bytes)
        }
        _ => Err(EmoteError::InvalidSignature),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cipher_is_an_involution() {
        let original: Vec<u8> = (0u8..=255).collect();
        let mut bytes = original.clone();
        PsbStreamCipher::new(0x1750_58C9).apply(&mut bytes);
        assert_ne!(bytes, original);
        PsbStreamCipher::new(0x1750_58C9).apply(&mut bytes);
        assert_eq!(bytes, original);
    }

    #[test]
    fn cipher_matches_documented_schedule_vector() {
        // Keystream for key 0 under the documented 6-word schedule:
        // first 4 bytes are the first generated word, little-endian.
        let mut zero = [0u8; 4];
        PsbStreamCipher::new(0).apply(&mut zero);
        let first = u32::from_le_bytes(zero);
        assert_eq!(first, 0xD9EA_5670);
    }
}
