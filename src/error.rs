//! Error type for `yu-emote`.

use std::io;

use thiserror::Error;

/// Errors returned by `yu-emote`.
#[derive(Debug, Error)]
pub enum EmoteError {
    /// An I/O error occurred while reading or writing.
    #[error("I/O: {0}")]
    Io(#[from] io::Error),

    /// The file does not start with the PSB or MDF signature.
    #[error("not a PSB or MDF file")]
    InvalidSignature,

    /// The PSB header is truncated or contains impossible offsets.
    #[error("invalid PSB header")]
    InvalidHeader,

    /// The PSB header itself is encrypted (PSB v3+); not supported yet.
    #[error("encrypted PSB header is not supported yet")]
    EncryptedHeaderUnsupported,

    /// The PSB body is encrypted and no key was supplied.
    #[error("PSB body is encrypted; supply the E-mote key")]
    EncryptedPsbRequiresKey,

    /// The PSB header or index could not be opened.
    #[error("PSB open: {0}")]
    PsbOpen(#[from] emote_psb::psb::error::PsbOpenError),

    /// The PSB value tree could not be decoded.
    #[error("PSB decode: {0}")]
    PsbDecode(#[from] emote_psb::value::de::Error),

    /// The MDF container could not be opened.
    #[error("MDF open: {0}")]
    MdfOpen(#[from] emote_psb::mdf::error::MdfOpenError),

    /// A parameter was used with a type the adapter does not understand.
    #[error("unsupported type for E-mote parameter `{name}`")]
    UnsupportedParameterType {
        /// Parameter name as it appears in the YU-RIS command table.
        name: String,
    },

    /// An `EMOTEINFO` field has no evidence-backed semantics yet.
    #[error("unsupported EMOTEINFO field `{0}`")]
    UnsupportedInfo(String),

    /// The adapter is not allowed to advance animation yet.
    #[error("E-mote runtime is not ready (capability: {0})")]
    NotReady(crate::yuris::Capability),
}
