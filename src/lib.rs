//! # yu-emote
//!
//! E-mote (EMT/PSB) data layer and YU-RIS adapter.
//!
//! The runtime behavior is referenced from the open-source `eluna` project
//! (MPL-2.0); see `NOTICE` and `README.md`.
//!
//! E-mote is a 2D character-animation middleware by M2 Co., Ltd. YU-RIS games
//! load E-mote models through an optional plugin (`YSEmote.dll` →
//! `emotedriver.dll`). This crate is an independent Rust implementation of the
//! **data and scheduling layers**, designed to be usable natively and on
//! `wasm32`:
//!
//! - [`psb`]: PSB/MDF container parsing (delegates the binary format to the
//!   MIT-licensed `emote-psb` crate).
//! - [`schema`]: typed model extraction (screen, atlas pages, objects,
//!   motions, layers, variables, timelines, controllers).
//! - [`texture`]: RGBA4444 atlas decoding (evidenced by the official DLL).
//! - [`anim`]: deterministic variable/timeline state (M2).
//! - [`pose`]: layer/frame evaluation and transform composition (M2).
//! - [`plan`]: pose to textured mesh draw list (M3 geometry bridge).
//! - [`soft`]: deterministic CPU rasterizer for validation and screenshots.
//! - [`render`]: WGSL variants and Bezier mesh generation (M3).
//! - [`model`]: deterministic, renderer-independent summary of an E-mote PSB
//!   tree (version, top-level sections, node counts, resource counts).
//! - [`yuris`]: YU-RIS glue — `EMOTE` parameter recording, `EMOTEINFO`
//!   queries, capability reporting, suspension and dirty-frame tracking.
//!
//! Timeline playback, graph/physics controllers and a deterministic CPU
//! rasterizer are implemented; the wgpu delivery pipeline is still pending.
//! See `docs/ARCHITECTURE.md`.
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

pub mod anim;
pub mod control;
pub mod error;
pub mod model;
pub mod physics;
pub mod plan;
pub mod player;
pub mod pose;
pub mod psb;
pub mod render;
pub mod schema;
pub mod soft;
pub mod texture;
pub mod yuris;

pub use anim::Animator;
pub use error::EmoteError;
pub use model::{ModelSummary, PsbNodeCounts};
pub use plan::{DrawItem, RenderPlan};
pub use player::Player;
pub use pose::{FrameState, LayerPose, Pose};
pub use psb::PsbDocument;
pub use schema::Model;
pub use soft::{RenderImage, SoftStats};
pub use texture::DecodedTexture;
pub use yuris::{Capability, EmoteArg, YurisEmote};

/// Crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
