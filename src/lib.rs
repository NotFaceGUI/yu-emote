//! # yu-emote
//!
//! E-mote (EMT/PSB) runtime and YU-RIS adapter.
//!
//! E-mote is a 2D character-animation middleware by M2 Co., Ltd. YU-RIS games
//! load E-mote models through an optional plugin (`YSEmote.dll` →
//! `emotedriver.dll`). This crate provides the YU-RIS-facing pieces:
//!
//! - [`psb`]: PSB/MDF access, body decryption with an explicit per-game key
//!   and embedded resource ranges;
//! - [`runtime`]: load and drive an E-mote model;
//! - [`texture`]: `RGBA4444` / 32-bpp / RL texture decoding;
//! - [`plan`]: evaluated scene → textured triangle draw plan;
//! - [`soft`]: deterministic CPU rasterizer used for screenshots and the demo;
//! - [`yuris`]: YU-RIS `EMOTE` parameter recording, capability reporting,
//!   suspension and dirty-frame tracking.
//!
//! ## Runtime ownership
//!
//! The animation core is **not** implemented in this crate. PSB parsing,
//! motion/timeline evaluation, mesh-chain deformation, anchors, clips, joins,
//! physics and the SDK state machine come from the vendored
//! [`eluna`](https://github.com/xmoezzz/eluna) runtime under `vendor/eluna`
//! (MPL-2.0). This crate adapts that runtime to YU-RIS data and to rendering.
//!
//! Vendor provenance and the one local PSB v4 parsing fix are documented in
//! `vendor/eluna/PROVENANCE.md`. See `NOTICE` and `README.md` for licensing.
//!
//! ## Example
//!
//! ```no_run
//! use yu_emote::{Emote, soft};
//!
//! let mut emote = Emote::load("model.psb", None, None)?;
//! emote.play_timeline("idle", yu_emote::TimelinePlayMode::LOOP)?;
//! emote.advance_ticks(1.0)?;
//!
//! let plan = emote.plan([900, 1400]);
//! let textures = emote.textures()?;
//! let view = soft::ViewTransform::fit(&plan, 24.0);
//! let (image, _stats) = soft::render_with_textures(&plan, &textures, view)?;
//! # let _ = image;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod gpu;
pub mod plan;
pub mod psb;
pub mod runtime;
pub mod soft;
pub mod texture;
pub mod yuris;

pub use eluna;
pub use error::EmoteError;
pub use gpu::{GpuFrame, GpuRenderer, GpuStats};
pub use plan::{DrawItem, MaskGroup, PlanVertex, RenderPlan};
pub use psb::{PsbDocument, PsbFile, PsbValue};
pub use runtime::{
    EMOTE_TICKS_PER_SECOND, ElunaPlayer, Emote, EmoteDrawPass, EmoteMeshPatch, EmoteStaticSprite,
    EmoteTransformMode, TimelinePlayMode,
};
pub use texture::DecodedTexture;
pub use yuris::{Capability, EmoteArg, YurisEmote};

/// Crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
