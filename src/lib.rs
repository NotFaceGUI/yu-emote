//! # yu-emote
//!
//! Clean-room E-mote (EMT/PSB) data layer and YU-RIS adapter.
//!
//! E-mote is a 2D character-animation middleware by M2 Co., Ltd. YU-RIS games
//! load E-mote models through an optional plugin (`YSEmote.dll` →
//! `emotedriver.dll`). This crate is an independent Rust implementation of the
//! **data and scheduling layers**, designed to be usable natively and on
//! `wasm32`:
//!
//! - [`psb`]: PSB/MDF container parsing (delegates the binary format to the
//!   MIT-licensed `emote-psb` crate).
//! - [`model`]: deterministic, renderer-independent summary of an E-mote PSB
//!   tree (version, top-level sections, node counts, resource counts).
//! - [`yuris`]: YU-RIS glue — `EMOTE` parameter recording, `EMOTEINFO`
//!   queries, capability reporting, suspension and dirty-frame tracking.
//!
//! Rendering (wgpu/WebGPU) and timeline evaluation are the next milestones;
//! they are **not** claimed by this version. See `docs/ARCHITECTURE.md`.
//!
//! ## Example
//!
//! ```no_run
//! use yu_emote::model::ModelSummary;
//! use yu_emote::psb;
//!
//! let document = psb::read_document("emote_test.psb")?;
//! let summary = ModelSummary::from_document(&document);
//! println!("{}", serde_json::to_string_pretty(&summary)?);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## Licensing
//!
//! Dual-licensed under MIT OR Apache-2.0. The PSB/MDF binary parser is the
//! MIT-licensed `emote-psb` crate; see `NOTICE` for attributions.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod model;
pub mod psb;
pub mod yuris;

pub use error::EmoteError;
pub use model::{ModelSummary, PsbNodeCounts};
pub use psb::PsbDocument;
pub use yuris::{Capability, EmoteArg, YurisEmote};

/// Crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
