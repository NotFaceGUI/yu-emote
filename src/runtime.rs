//! YU-RIS-facing runtime loader.
//!
//! The player itself is `eluna::sdk::EmoteRuntime` (vendored under
//! `vendor/eluna`). This module keeps the PSB document next to it so callers
//! can read the root `spec`, screen metadata and embedded resources without
//! re-parsing, and provides the small conveniences the CLI/demo tools need.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use eluna::sdk::EmoteRuntime;
use eluna::{EmoteLoadOptions, EmoteStaticScene};

use crate::error::EmoteError;
use crate::plan::RenderPlan;
use crate::psb::PsbDocument;
use crate::texture::{DecodedTexture, decode_runtime_textures};

pub use eluna::sdk::{EmoteHideOptions, EmoteNewOptions, EmoteShowOptions, EmoteTransOptions};
pub use eluna::{
    EMOTE_TICKS_PER_SECOND, ElunaPlayer, EmoteDrawPass, EmoteMeshPatch, EmoteStaticSprite,
    EmoteTransformMode, TimelinePlayMode,
};

/// A loaded E-mote model: normalized PSB plus the `eluna` playback runtime.
pub struct Emote {
    /// Playback/evaluation runtime.
    pub runtime: EmoteRuntime,
    /// Normalized PSB document (root tree, resources, `spec`).
    pub document: PsbDocument,
}

impl Emote {
    /// Load a PSB/MDF file with an optional per-game E-mote key.
    pub fn load(
        path: impl AsRef<Path>,
        key: Option<u32>,
        motion: Option<&str>,
    ) -> Result<Self, EmoteError> {
        Self::from_bytes(&fs::read(path)?, key, motion)
    }

    /// Decode PSB bytes and start the runtime.
    ///
    /// The document is normalized/decrypted once; the runtime is then created
    /// from those normalized bytes (no second decryption pass).
    pub fn from_bytes(
        bytes: &[u8],
        key: Option<u32>,
        motion: Option<&str>,
    ) -> Result<Self, EmoteError> {
        let document = PsbDocument::from_bytes(bytes, key)?;
        let options = EmoteLoadOptions {
            motion: motion.map(str::to_owned),
            // The caller drives timeline playback explicitly; auto-starting
            // would hide which timeline the host requested.
            autoplay_timeline: false,
            ..EmoteLoadOptions::default()
        };
        let runtime = EmoteRuntime::from_bytes(document.normalized(), options)?;
        Ok(Self { runtime, document })
    }

    /// Root `spec` string (`ems`, `common`, ...), if present.
    pub fn spec(&self) -> Option<&str> {
        crate::texture::spec_of(self.document.root())
    }

    /// Authored `screenSize`, if present.
    pub fn screen_size(&self) -> Option<[f32; 2]> {
        let screen = self
            .document
            .root()
            .as_object()?
            .iter()
            .find(|(name, _)| name == "screenSize")
            .map(|(_, value)| value)?;
        Some([
            screen.field_f32("width").unwrap_or(0.0),
            screen.field_f32("height").unwrap_or(0.0),
        ])
    }

    /// Current scene.
    pub fn scene(&self) -> &EmoteStaticScene {
        self.runtime.scene()
    }

    /// All motion names.
    pub fn motion_names(&self) -> Result<Vec<String>, EmoteError> {
        Ok(self.runtime.motion_names()?)
    }

    /// All timeline names.
    pub fn timeline_names(&self) -> Vec<String> {
        self.runtime
            .timeline_names()
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    /// Current variable values.
    pub fn variables(&self) -> BTreeMap<String, f32> {
        self.runtime.variable_values()
    }

    /// Apply immediate variable writes and rebuild after each write.
    ///
    /// `set_variable` only changes the transition target; the renderer needs
    /// the evaluated value now, so use the immediate path.
    pub fn set_variables(&mut self, values: &BTreeMap<String, f64>) -> Result<(), EmoteError> {
        for (name, value) in values {
            self.runtime.set_variable_immediate(name, *value as f32)?;
        }
        Ok(())
    }

    /// Start a timeline.
    pub fn play_timeline(&mut self, name: &str, mode: TimelinePlayMode) -> Result<(), EmoteError> {
        if !self.runtime.timeline_names().contains(&name) {
            return Err(EmoteError::UnknownTimeline(name.to_owned()));
        }
        self.runtime.play_timeline(name, mode)?;
        Ok(())
    }

    /// Advance by seconds at the native 60 Hz tick rate.
    pub fn advance_seconds(&mut self, seconds: f32) -> Result<(), EmoteError> {
        self.runtime.progress_seconds(seconds)?;
        Ok(())
    }

    /// Advance by native ticks.
    pub fn advance_ticks(&mut self, ticks: f32) -> Result<(), EmoteError> {
        self.runtime.progress_ticks(ticks)?;
        Ok(())
    }

    /// Decode every texture page of the model.
    pub fn textures(&self) -> Result<BTreeMap<u32, DecodedTexture>, EmoteError> {
        decode_runtime_textures(&self.runtime, self.spec())
    }

    /// Build a draw plan for the current scene.
    pub fn plan(&self, canvas: [u32; 2]) -> RenderPlan {
        RenderPlan::from_scene(self.runtime.scene(), canvas)
    }
}
