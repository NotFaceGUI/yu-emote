//! Error type for `yu-emote`.

use std::io;

use thiserror::Error;

/// Errors returned by `yu-emote`.
#[derive(Debug, Error)]
pub enum EmoteError {
    /// An I/O error occurred while reading or writing.
    #[error("I/O: {0}")]
    Io(#[from] io::Error),

    /// The PSB container could not be decoded.
    #[error("PSB: {0}")]
    Psb(#[from] eluna::PsbError),

    /// The E-mote schema in the PSB tree could not be interpreted.
    #[error("E-mote schema: {0}")]
    Schema(#[from] eluna::EmoteSchemaError),

    /// The runtime rejected a control call.
    #[error("E-mote runtime: {0}")]
    Runtime(#[from] eluna::EmoteRuntimeError),

    /// A model does not contain the requested motion.
    #[error("unknown motion `{0}`")]
    UnknownMotion(String),

    /// A model does not contain the requested timeline.
    #[error("unknown timeline `{0}`")]
    UnknownTimeline(String),

    /// A model does not contain the requested variable.
    #[error("unknown variable `{0}`")]
    UnknownVariable(String),

    /// A texture page uses a format this build cannot decode.
    #[error("unsupported texture format `{format}`")]
    UnsupportedTextureFormat {
        /// Format name from `source/tex#N/texture/type`.
        format: String,
    },

    /// A texture page references no resource, or the index is out of range.
    #[error("texture page `{page}` has no resource (index {index:?})")]
    TextureResourceMissing {
        /// Page name (`tex#0`, ...).
        page: String,
        /// Referenced resource index.
        index: Option<usize>,
    },

    /// The resource length does not match the declared dimensions.
    #[error("texture {width}x{height} needs {expected} bytes, resource has {actual}")]
    TextureSizeMismatch {
        /// Declared width.
        width: u32,
        /// Declared height.
        height: u32,
        /// Expected byte count.
        expected: usize,
        /// Actual byte count.
        actual: usize,
    },

    /// The declared dimensions overflow `usize`.
    #[error("texture dimensions {width}x{height} overflow")]
    TextureSizeOverflow {
        /// Declared width.
        width: u32,
        /// Declared height.
        height: u32,
    },

    /// The GPU renderer could not be created or read back.
    #[error("GPU: {0}")]
    Gpu(String),

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
