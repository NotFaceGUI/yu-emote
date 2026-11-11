//! Official-shaped device/player facade for YU-RIS host integration.
//!
//! The official E-mote SDK (`emotedriver.dll`, reached through `YSEmote.dll`
//! ordinals in YU-RIS) exposes two interfaces:
//!
//! - `IEmoteDevice`: creates players and owns device-wide render options;
//! - `IEmotePlayer`: one character's state, variables, timelines and `Render`.
//!
//! This module keeps that vocabulary over the vendored `eluna` runtime and the
//! wgpu renderer, so the kernel can drive a character without adopting PSB,
//! mesh or timeline internals. Method names follow the official header
//! (`Show`, `SetCoord`, `SetVariable`, `PlayTimeline`, `Progress`, ...) in
//! Rust snake_case.
//!
//! Deliberate differences from the D3D9 SDK:
//!
//! - [`EmotePlayer::render`] returns a [`GpuFrame`] over the host's
//!   `wgpu::Device`; the host composites the `TextureView`. It does not draw
//!   into an implicit D3D9 render target.
//! - COM refcounting, `Clone`/`AssignState`, stencil mask mode, shader-model
//!   selection, mipmaps, alpha-op selection, pixelate division and feedback
//!   history are outside this facade. [`official_api_support`] is the
//!   authoritative support ledger.
//! - Index-based variable/timeline enumeration is deterministic but sorted by
//!   label (the runtime stores labels in ordered maps), not authored order.
//!
//! See `docs/official-api.md` for the full official-to-`yu-emote` mapping.

use std::collections::BTreeMap;
use std::path::Path;

pub use eluna::EmoteMaskMode;
use eluna::EmotePlayerControl;
use eluna::api::{TimelinePlayMode, transform_order_mask};
use serde::Serialize;

use crate::error::EmoteError;
use crate::gpu::{GpuFrame, GpuRenderer};
use crate::plan::RenderPlan;
use crate::runtime::Emote;
use crate::soft::{self, RenderImage, SoftStats, ViewTransform};
use crate::texture::DecodedTexture;
use crate::yuris::Capability;

/// Version returned by `YSEmote.dll` ordinal 1 in SDK 0.495.
pub const OFFICIAL_INTERFACE_VERSION: u32 = 1000;

/// One 1/60-second tick, the unit used by official `Progress`/`SetVariable`.
pub use eluna::EMOTE_TICKS_PER_SECOND;

/// Device-wide render options mirrored from `IEmoteDevice`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmoteDeviceOptions {
    /// Screen rectangle used as the default player canvas (`SetScreenRect`).
    pub screen: [u32; 2],
    /// Official mask mode. The wgpu path implements alpha coverage; stencil is
    /// rejected instead of silently substituted.
    pub mask_mode: EmoteMaskMode,
    /// Official `SetMaskRegionClipping`. Stored for diagnostics; alpha masks
    /// are already clipped to their coverage region.
    pub mask_region_clipping: bool,
    /// Official `SetProtectTranslucentTextureColor`. Stored for diagnostics;
    /// not yet consumed by the renderer.
    pub protect_translucent_texture_color: bool,
}

impl Default for EmoteDeviceOptions {
    fn default() -> Self {
        Self {
            screen: [800, 600],
            mask_mode: EmoteMaskMode::DEFAULT,
            mask_region_clipping: true,
            protect_translucent_texture_color: true,
        }
    }
}

/// Shader-model value reported by the wgpu backend.
///
/// The official enum only has no-shader / SM2 / SM3. `Wgpu` is an explicit
/// `yu-emote` extension so callers never mistake the wgpu pipeline for D3D9
/// shader model 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ShaderModel {
    /// No shader path.
    NoShader,
    /// D3D9 shader model 2 (not available in this backend).
    Model2,
    /// D3D9 shader model 3 (not available in this backend).
    Model3,
    /// Native wgpu pipeline.
    Wgpu,
}

/// Per-player render/placement options.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmotePlayerOptions {
    /// Render-target size in pixels.
    pub canvas: [u32; 2],
    /// Optional outer camera transform (host layer placement). It is applied
    /// **after** the official player transform, so it is a screen-space
    /// placement, not a model-space fit. When `None`, the initial model bounds
    /// are fitted into the canvas and kept fixed.
    pub view: Option<ViewTransform>,
    /// Margin in pixels for the automatic fixed camera.
    pub margin: f32,
}

impl Default for EmotePlayerOptions {
    fn default() -> Self {
        Self {
            canvas: [800, 600],
            view: None,
            margin: 20.0,
        }
    }
}

/// Official-shaped E-mote device: creates players on the host's wgpu device.
pub struct EmoteDevice {
    #[cfg(not(target_arch = "wasm32"))]
    _instance: Option<wgpu::Instance>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    options: EmoteDeviceOptions,
    shader_model: ShaderModel,
    max_texture_size: [u32; 2],
}

impl EmoteDevice {
    /// Build a device facade over an existing wgpu device/queue (the kernel
    /// path; the device is shared, not re-created).
    ///
    /// `EmoteDeviceOptions::mask_mode` must be [`EmoteMaskMode::Alpha`];
    /// requesting stencil is an explicit error.
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        options: EmoteDeviceOptions,
    ) -> Result<Self, EmoteError> {
        if options.mask_mode == EmoteMaskMode::Stencil {
            return Err(EmoteError::UnsupportedFeature(
                "IEmoteDevice::SetMaskMode(stencil)",
            ));
        }
        Ok(Self {
            #[cfg(not(target_arch = "wasm32"))]
            _instance: None,
            device,
            queue,
            options,
            shader_model: ShaderModel::Wgpu,
            max_texture_size: [8192, 8192],
        })
    }

    /// Create a standalone device for CLI/tests.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn headless(options: EmoteDeviceOptions) -> Result<(Self, wgpu::AdapterInfo), EmoteError> {
        let (instance, device, queue, info) = crate::gpu::headless_context()?;
        let mut facade = Self::new(device, queue, options)?;
        facade._instance = Some(instance);
        Ok((facade, info))
    }

    /// Official device option snapshot.
    pub const fn options(&self) -> EmoteDeviceOptions {
        self.options
    }

    /// Current screen rectangle.
    pub const fn screen_rect(&self) -> [u32; 2] {
        self.options.screen
    }

    /// `IEmoteDevice::SetScreenRect`.
    pub fn set_screen_rect(&mut self, width: u32, height: u32) {
        self.options.screen = [width.max(1), height.max(1)];
    }

    /// `IEmoteDevice::GetMaskMode`.
    pub const fn mask_mode(&self) -> EmoteMaskMode {
        self.options.mask_mode
    }

    /// `IEmoteDevice::SetMaskMode`.
    ///
    /// Stencil mode is not implemented by the wgpu backend; passing it is an
    /// explicit error rather than a silent alpha substitution.
    pub fn set_mask_mode(&mut self, mode: EmoteMaskMode) -> Result<(), EmoteError> {
        match mode {
            EmoteMaskMode::Alpha => {
                self.options.mask_mode = mode;
                Ok(())
            }
            EmoteMaskMode::Stencil => Err(EmoteError::UnsupportedFeature(
                "IEmoteDevice::SetMaskMode(stencil)",
            )),
        }
    }

    /// `IEmoteDevice::SetMaskRegionClipping`.
    pub fn set_mask_region_clipping(&mut self, state: bool) {
        self.options.mask_region_clipping = state;
    }

    /// `IEmoteDevice::GetMaskRegionClipping`.
    pub const fn mask_region_clipping(&self) -> bool {
        self.options.mask_region_clipping
    }

    /// `IEmoteDevice::SetProtectTranslucentTextureColor`.
    pub fn set_protect_translucent_texture_color(&mut self, state: bool) {
        self.options.protect_translucent_texture_color = state;
    }

    /// `IEmoteDevice::GetProtectTranslucentTextureColor`.
    pub const fn protect_translucent_texture_color(&self) -> bool {
        self.options.protect_translucent_texture_color
    }

    /// `IEmoteDevice::GetAvailableShaderModel`.
    pub const fn available_shader_model(&self) -> ShaderModel {
        ShaderModel::Wgpu
    }

    /// `IEmoteDevice::GetShaderModel`.
    pub const fn shader_model(&self) -> ShaderModel {
        self.shader_model
    }

    /// `IEmoteDevice::SetShaderModel`.
    ///
    /// The wgpu backend cannot select a D3D9 shader model; only [`Wgpu`] is
    /// accepted.
    ///
    /// [`Wgpu`]: ShaderModel::Wgpu
    pub fn set_shader_model(&mut self, model: ShaderModel) -> Result<(), EmoteError> {
        if model == ShaderModel::Wgpu {
            self.shader_model = model;
            Ok(())
        } else {
            Err(EmoteError::UnsupportedFeature(
                "IEmoteDevice::SetShaderModel(D3D9)",
            ))
        }
    }

    /// `IEmoteDevice::SetMipMapEnabled`; mipmaps are not generated.
    pub fn set_mip_map_enabled(&mut self, state: bool) -> Result<(), EmoteError> {
        if state {
            Err(EmoteError::UnsupportedFeature(
                "IEmoteDevice::SetMipMapEnabled",
            ))
        } else {
            Ok(())
        }
    }

    /// `IEmoteDevice::GetMipMapEnabled`; the wgpu path never generates mipmaps.
    pub const fn mip_map_enabled(&self) -> bool {
        false
    }

    /// Maximum texture dimensions. Stored for diagnostics; wgpu limits still
    /// apply at upload time.
    pub const fn max_texture_size(&self) -> [u32; 2] {
        self.max_texture_size
    }

    /// `IEmoteDevice::SetMaxTextureSize`.
    pub fn set_max_texture_size(&mut self, width: u32, height: u32) {
        self.max_texture_size = [width.max(1), height.max(1)];
    }

    /// Create a player from PSB bytes, mirroring `IEmoteDevice::CreatePlayer`.
    pub fn create_player(
        &self,
        bytes: &[u8],
        key: Option<u32>,
        motion: Option<&str>,
    ) -> Result<EmotePlayer, EmoteError> {
        let emote = Emote::from_bytes(bytes, key, motion)?;
        self.create_player_from_emote(emote)
    }

    /// Create a player from an already-loaded [`Emote`].
    pub fn create_player_from_emote(&self, emote: Emote) -> Result<EmotePlayer, EmoteError> {
        self.create_player_from_emote_with_options(
            emote,
            EmotePlayerOptions {
                canvas: self.options.screen,
                ..EmotePlayerOptions::default()
            },
        )
    }

    /// Create a player from an [`Emote`] with explicit player options
    /// (canvas, outer view, camera margin).
    pub fn create_player_from_emote_with_options(
        &self,
        emote: Emote,
        options: EmotePlayerOptions,
    ) -> Result<EmotePlayer, EmoteError> {
        EmotePlayer::with_renderer(self.device.clone(), self.queue.clone(), emote, options)
    }

    /// The shared wgpu device.
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// The shared wgpu queue.
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Capability reported to `SYSTEMINFO[EMOTE]`.
    pub const fn capability(&self) -> Capability {
        Capability::Ready
    }
}

/// Official-shaped E-mote player.
///
/// State and timeline methods are evaluated by the vendored `eluna` runtime.
/// The player also owns decoded textures and, when built through
/// [`EmoteDevice`] or [`EmotePlayer::with_renderer`], a wgpu renderer.
pub struct EmotePlayer {
    emote: Emote,
    renderer: Option<GpuRenderer>,
    textures: BTreeMap<u32, DecodedTexture>,
    canvas: [u32; 2],
    margin: f32,
    outer_view: Option<ViewTransform>,
    model_center: [f32; 2],
    auto_view: ViewTransform,
    coord_tween: Option<Tween2>,
    scale_tween: Option<Tween>,
    rot_tween: Option<Tween>,
    variable_labels: Vec<String>,
    main_timelines: Vec<String>,
    diff_timelines: Vec<String>,
    chara_profiles: Vec<String>,
    /// Facade-owned dirty bit for changes the runtime does not flag
    /// (visibility, canvas, host view).
    dirty: bool,
}

impl EmotePlayer {
    /// Build a CPU-only player (planning, CPU raster, state APIs).
    pub fn new(emote: Emote, options: EmotePlayerOptions) -> Result<Self, EmoteError> {
        Self::build(emote, options, None)
    }

    /// Build a player on an existing wgpu device/queue.
    pub fn with_renderer(
        device: wgpu::Device,
        queue: wgpu::Queue,
        emote: Emote,
        options: EmotePlayerOptions,
    ) -> Result<Self, EmoteError> {
        let renderer = GpuRenderer::new(device, queue);
        Self::build(emote, options, Some(renderer))
    }

    /// Load PSB bytes into a CPU-only player.
    pub fn from_bytes(
        bytes: &[u8],
        key: Option<u32>,
        motion: Option<&str>,
        options: EmotePlayerOptions,
    ) -> Result<Self, EmoteError> {
        Self::new(Emote::from_bytes(bytes, key, motion)?, options)
    }

    /// Load a PSB file into a CPU-only player.
    pub fn load(
        path: impl AsRef<Path>,
        key: Option<u32>,
        motion: Option<&str>,
        options: EmotePlayerOptions,
    ) -> Result<Self, EmoteError> {
        Self::new(Emote::load(path, key, motion)?, options)
    }

    fn build(
        emote: Emote,
        options: EmotePlayerOptions,
        renderer: Option<GpuRenderer>,
    ) -> Result<Self, EmoteError> {
        let textures = emote.textures()?;
        let canvas = [options.canvas[0].max(1), options.canvas[1].max(1)];
        let probe = emote.plan(canvas);
        let model_center = probe
            .bounds()
            .map(|(min, max)| [(min[0] + max[0]) * 0.5, (min[1] + max[1]) * 0.5])
            .unwrap_or([0.0, 0.0]);
        let auto_view = ViewTransform::fit_camera(&probe, options.margin);
        let runtime = &emote.runtime;
        let variable_labels = runtime
            .variable_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let main_timelines = runtime
            .main_timeline_labels()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let diff_timelines = runtime
            .diff_timeline_labels()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let chara_profiles = runtime.chara_profile_labels();
        let mut player = Self {
            emote,
            renderer,
            textures,
            canvas,
            margin: options.margin,
            outer_view: options.view,
            model_center,
            auto_view,
            coord_tween: None,
            scale_tween: None,
            rot_tween: None,
            variable_labels,
            main_timelines,
            diff_timelines,
            chara_profiles,
            dirty: true,
        };
        if let Some(renderer) = player.renderer.as_mut() {
            renderer.upload_textures(&player.textures)?;
        }
        Ok(player)
    }

    /// The underlying [`Emote`] wrapper.
    pub fn emote(&self) -> &Emote {
        &self.emote
    }

    /// Mutable access for advanced callers (motion switching, selectors,
    /// ground-correction hook, ...).
    pub fn emote_mut(&mut self) -> &mut Emote {
        &mut self.emote
    }

    /// Decoded texture pages.
    pub fn textures(&self) -> &BTreeMap<u32, DecodedTexture> {
        &self.textures
    }

    /// Release the CPU-decoded texture pages after they were uploaded to the
    /// GPU.
    ///
    /// This saves the CPU copy (roughly `page_w * page_h * 4` bytes per page).
    /// [`Self::render`] keeps working through
    /// [`GpuRenderer::render_uploaded`]; [`Self::render_cpu`] then returns
    /// `UnsupportedFeature`.
    pub fn release_decoded_textures(&mut self) {
        self.textures.clear();
    }

    /// Capability of this player instance.
    ///
    /// A player built with [`EmotePlayer::new`] can evaluate and CPU-render but
    /// reports [`Capability::DataOnly`]; the device-created/wgpu path reports
    /// [`Capability::Ready`].
    pub const fn capability(&self) -> Capability {
        if self.renderer.is_some() {
            Capability::Ready
        } else {
            Capability::DataOnly
        }
    }

    /// Whether this player owns a wgpu renderer.
    pub const fn has_renderer(&self) -> bool {
        self.renderer.is_some()
    }

    /// Render-target size.
    pub const fn canvas(&self) -> [u32; 2] {
        self.canvas
    }

    /// Change the render-target size and recompute the fixed camera.
    pub fn set_canvas(&mut self, width: u32, height: u32) {
        self.canvas = [width.max(1), height.max(1)];
        self.dirty = true;
        let plan = self.emote.plan(self.canvas);
        self.model_center = plan
            .bounds()
            .map(|(min, max)| [(min[0] + max[0]) * 0.5, (min[1] + max[1]) * 0.5])
            .unwrap_or([0.0, 0.0]);
        self.auto_view = ViewTransform::fit_camera(&plan, self.margin);
    }

    /// Optional outer camera view (host layer placement).
    pub const fn view(&self) -> Option<ViewTransform> {
        self.outer_view
    }

    /// Set or clear the outer camera view.
    ///
    /// The view is composed after the official player transform (scale,
    /// rotation, coord). It is a screen-space placement, not a second
    /// model-space fit; leave it `None` to use the fixed fit camera.
    pub fn set_view(&mut self, view: Option<ViewTransform>) {
        self.outer_view = view;
        self.dirty = true;
    }

    // ---------------------------------------------------------------------
    // Visibility and device-like state
    // ---------------------------------------------------------------------

    /// `IEmotePlayer::Show`.
    pub fn show(&mut self) {
        self.emote.runtime.show();
        self.dirty = true;
    }

    /// `IEmotePlayer::Hide`.
    pub fn hide(&mut self) {
        self.emote.runtime.hide();
        self.dirty = true;
    }

    /// `IEmotePlayer::IsHidden`.
    pub fn is_hidden(&self) -> bool {
        !self.emote.runtime.inner_player().is_shown()
    }

    /// `IEmotePlayer::SetSmoothing`.
    ///
    /// State only: the runtime stores the flag, but neither the scene builder
    /// nor either renderer consumes it yet (official smoothing semantics are
    /// not recovered).
    pub fn set_smoothing(&mut self, state: bool) {
        self.emote.runtime.set_smoothing(state);
    }

    /// `IEmotePlayer::GetSmoothing`.
    pub fn smoothing(&self) -> bool {
        self.emote.runtime.smoothing()
    }

    /// `IEmotePlayer::SetMeshDivisionRatio`.
    ///
    /// State only: mesh tessellation currently comes from the authored
    /// `meshDivision`/plan data; the player ratio is stored but not applied.
    pub fn set_mesh_division_ratio(&mut self, ratio: f32) {
        self.emote.runtime.set_mesh_division_ratio(ratio);
    }

    /// `IEmotePlayer::GetMeshDivisionRatio`.
    pub fn mesh_division_ratio(&self) -> f32 {
        self.emote.runtime.mesh_division_ratio()
    }

    /// `IEmotePlayer::SetQueuing`.
    pub fn set_queuing(&mut self, state: bool) {
        self.emote.runtime.set_queuing(state);
    }

    /// `IEmotePlayer::GetQueuing`.
    pub fn queuing(&self) -> bool {
        self.emote.runtime.queuing()
    }

    /// `IEmotePlayer::SetHairScale`.
    pub fn set_hair_scale(&mut self, scale: f32) -> Result<(), EmoteError> {
        self.emote.runtime.set_hair_scale(scale)?;
        Ok(())
    }

    /// `IEmotePlayer::GetHairScale`.
    pub fn hair_scale(&self) -> f32 {
        self.emote.runtime.inner_player().hair_scale()
    }

    /// `IEmotePlayer::SetPartsScale`.
    pub fn set_parts_scale(&mut self, scale: f32) -> Result<(), EmoteError> {
        self.emote.runtime.set_parts_scale(scale)?;
        Ok(())
    }

    /// `IEmotePlayer::GetPartsScale`.
    pub fn parts_scale(&self) -> f32 {
        self.emote.runtime.inner_player().parts_scale()
    }

    /// `IEmotePlayer::SetBustScale`.
    pub fn set_bust_scale(&mut self, scale: f32) -> Result<(), EmoteError> {
        self.emote.runtime.set_bust_scale(scale)?;
        Ok(())
    }

    /// `IEmotePlayer::GetBustScale`.
    pub fn bust_scale(&self) -> f32 {
        self.emote.runtime.inner_player().bust_scale()
    }

    /// Current player coordinate (`GetCoord`).
    pub fn get_coord(&self) -> [f32; 2] {
        self.emote.runtime.inner_player().coord()
    }

    /// `IEmotePlayer::SetCoord` with the default zero frame count.
    pub fn set_coord(&mut self, x: f32, y: f32) {
        self.coord_tween = None;
        self.emote.runtime.set_coord(x, y);
    }

    /// `IEmotePlayer::SetCoord(x, y, frameCount, easing)`.
    pub fn set_coord_timed(&mut self, x: f32, y: f32, frame_count: f32, easing: f32) {
        let start = self.get_coord();
        self.coord_tween = None;
        if frame_count <= 0.0 || !frame_count.is_finite() {
            self.set_coord(x, y);
            return;
        }
        if x.is_finite() && y.is_finite() {
            self.coord_tween = Some(Tween2::new(
                start,
                [x, y],
                frame_count,
                native_easing_exponent(easing),
            ));
            self.dirty = true;
        }
    }

    /// Current player scale (`GetScale`).
    pub fn get_scale(&self) -> f32 {
        self.emote.runtime.inner_player().scale()
    }

    /// `IEmotePlayer::SetScale` with the default zero frame count.
    pub fn set_scale(&mut self, scale: f32) {
        self.scale_tween = None;
        self.emote.runtime.set_scale(scale);
    }

    /// `IEmotePlayer::SetScale(scale, frameCount, easing)`.
    pub fn set_scale_timed(&mut self, scale: f32, frame_count: f32, easing: f32) {
        let start = self.get_scale();
        self.scale_tween = None;
        if frame_count <= 0.0 || !frame_count.is_finite() {
            self.set_scale(scale);
            return;
        }
        if scale.is_finite() && scale > 0.0 {
            self.scale_tween = Some(Tween::new(
                start,
                scale,
                frame_count,
                native_easing_exponent(easing),
            ));
            self.dirty = true;
        }
    }

    /// Current player rotation in radians (`GetRot`).
    pub fn get_rot(&self) -> f32 {
        self.emote.runtime.inner_player().rot()
    }

    /// `IEmotePlayer::SetRot` with the default zero frame count.
    pub fn set_rot(&mut self, rot: f32) {
        self.rot_tween = None;
        self.emote.runtime.set_rot(rot);
    }

    /// `IEmotePlayer::SetRot(rot, frameCount, easing)`.
    pub fn set_rot_timed(&mut self, rot: f32, frame_count: f32, easing: f32) {
        let start = self.get_rot();
        self.rot_tween = None;
        if frame_count <= 0.0 || !frame_count.is_finite() {
            self.set_rot(rot);
            return;
        }
        if rot.is_finite() {
            self.rot_tween = Some(Tween::new(
                start,
                rot,
                frame_count,
                native_easing_exponent(easing),
            ));
            self.dirty = true;
        }
    }

    /// Current transform-order mask (`IEmotePlayer::transform_order_mask_t`).
    pub fn transform_order_mask(&self) -> u32 {
        self.emote.runtime.inner_player().transform_order_mask()
    }

    /// Set the transform-order mask. The position half is consumed by
    /// [`Self::effective_view`]; the physics half is consumed by the runtime.
    pub fn set_transform_order_mask(&mut self, mask: u32) -> Result<(), EmoteError> {
        self.emote.runtime.set_transform_order_mask(mask)?;
        Ok(())
    }

    /// `IEmotePlayer::SetColor`. Timed color transitions are not implemented;
    /// pass a zero frame count.
    pub fn set_color(
        &mut self,
        rgba: u32,
        frame_count: f32,
        easing: f32,
    ) -> Result<(), EmoteError> {
        if frame_count > 0.0 {
            return Err(EmoteError::UnsupportedFeature(
                "IEmotePlayer::SetColor with a non-zero frame count",
            ));
        }
        self.emote
            .runtime
            .inner_player_mut()
            .set_color_rgba(rgba, 0.0, easing);
        Ok(())
    }

    /// Current player color (`GetColor`).
    pub fn get_color(&self) -> u32 {
        self.emote.runtime.inner_player().color_rgba()
    }

    /// `IEmotePlayer::SetGrayscale`. Timed transitions are not implemented;
    /// pass a zero frame count.
    pub fn set_grayscale(
        &mut self,
        rate: f32,
        frame_count: f32,
        easing: f32,
    ) -> Result<(), EmoteError> {
        if frame_count > 0.0 {
            return Err(EmoteError::UnsupportedFeature(
                "IEmotePlayer::SetGrayscale with a non-zero frame count",
            ));
        }
        self.emote
            .runtime
            .inner_player_mut()
            .set_grayscale(rate, 0.0, easing);
        Ok(())
    }

    /// Current grayscale rate (`GetGrayscale`).
    pub fn get_grayscale(&self) -> f32 {
        self.emote.runtime.inner_player().grayscale()
    }

    // ---------------------------------------------------------------------
    // Variables
    // ---------------------------------------------------------------------

    /// `IEmotePlayer::CountVariables` (deterministic, label-sorted order).
    pub fn variable_count(&self) -> usize {
        self.variable_labels.len()
    }

    /// `IEmotePlayer::GetVariableLabelAt`.
    pub fn variable_label_at(&self, index: usize) -> Option<&str> {
        self.variable_labels.get(index).map(String::as_str)
    }

    /// `IEmotePlayer::CountVariableFrameAt`.
    pub fn variable_frame_count_at(&self, index: usize) -> Option<usize> {
        self.variable_label_at(index)
            .map(|name| self.emote.runtime.variable_frame_count(name))
    }

    /// `IEmotePlayer::GetVariableFrameLabelAt`.
    pub fn variable_frame_label_at(&self, index: usize, frame: usize) -> Option<&str> {
        let name = self.variable_label_at(index)?;
        self.emote.runtime.variable_frame_label_at(name, frame)
    }

    /// `IEmotePlayer::GetVariableFrameValueAt`.
    pub fn variable_frame_value_at(&self, index: usize, frame: usize) -> Option<f32> {
        let name = self.variable_label_at(index)?;
        self.emote.runtime.variable_frame_value_at(name, frame)
    }

    /// `IEmotePlayer::SetVariable` with the default zero frame count.
    pub fn set_variable(&mut self, name: &str, value: f32) -> Result<(), EmoteError> {
        self.emote.runtime.set_variable_immediate(name, value)?;
        Ok(())
    }

    /// Apply several zero-time variable writes with one scene rebuild.
    ///
    /// This is the batch form of `SetVariable(name, value, 0, 0)` and the
    /// preferred path for one `EMOTE` call that carries BUST/HAIR/PARTS/...
    /// together: [`Self::set_variable`] rebuilds the scene on every call.
    pub fn set_variables<'a, I>(&mut self, values: I) -> Result<(), EmoteError>
    where
        I: IntoIterator<Item = (&'a str, f32)>,
    {
        let mut changed = false;
        for (name, value) in values {
            self.emote
                .runtime
                .inner_player_mut()
                .set_variable_immediate(name, value);
            changed = true;
        }
        if changed {
            self.emote.runtime.rebuild_scene()?;
        }
        Ok(())
    }

    /// `IEmotePlayer::SetVariable(name, value, frameCount, easing)`.
    ///
    /// A zero frame count applies immediately; a positive one starts a native
    /// scalar transition evaluated on subsequent [`Self::progress`] calls.
    pub fn set_variable_timed(
        &mut self,
        name: &str,
        value: f32,
        frame_count: f32,
        easing: f32,
    ) -> Result<(), EmoteError> {
        if frame_count <= 0.0 || !frame_count.is_finite() {
            return self.set_variable(name, value);
        }
        self.emote
            .runtime
            .set_variable_timed(name, value, frame_count, easing);
        Ok(())
    }

    /// Current evaluated variable value (`GetVariable`).
    pub fn get_variable(&self, name: &str) -> Option<f32> {
        self.emote.runtime.variable_value(name)
    }

    /// `IEmotePlayer::SetVariableDiff`.
    pub fn set_variable_diff(
        &mut self,
        module: &str,
        name: &str,
        value: f32,
        frame_count: f32,
        easing: f32,
    ) -> Result<(), EmoteError> {
        self.emote
            .runtime
            .set_variable_diff(module, name, value, frame_count, easing)?;
        Ok(())
    }

    /// `IEmotePlayer::GetVariableDiff`.
    pub fn get_variable_diff(&self, module: &str, name: &str) -> Option<f32> {
        self.emote.runtime.get_variable_diff(module, name)
    }

    // ---------------------------------------------------------------------
    // Physics controls
    // ---------------------------------------------------------------------

    /// `IEmotePlayer::SetOuterForce`.
    pub fn set_outer_force(
        &mut self,
        label: &str,
        x: f32,
        y: f32,
        frame_count: f32,
        easing: f32,
    ) -> Result<(), EmoteError> {
        self.emote
            .runtime
            .set_outer_force(label, x, y, frame_count, easing)?;
        Ok(())
    }

    /// `IEmotePlayer::GetOuterForce`.
    pub fn get_outer_force(&self, label: &str) -> [f32; 2] {
        self.emote.runtime.inner_player().outer_force(label)
    }

    /// `IEmotePlayer::StartWind`.
    pub fn start_wind(&mut self, start: f32, goal: f32, speed: f32, pow_min: f32, pow_max: f32) {
        self.emote
            .runtime
            .start_wind(start, goal, speed, pow_min, pow_max);
    }

    /// `IEmotePlayer::StopWind`.
    pub fn stop_wind(&mut self) {
        self.emote.runtime.stop_wind();
    }

    /// `IEmotePlayer::SetOuterRot`.
    pub fn set_outer_rot(
        &mut self,
        rot: f32,
        frame_count: f32,
        easing: f32,
    ) -> Result<(), EmoteError> {
        self.emote.runtime.set_outer_rot(rot, frame_count, easing)?;
        Ok(())
    }

    /// `IEmotePlayer::GetOuterRot`.
    pub fn get_outer_rot(&self) -> f32 {
        self.emote.runtime.inner_player().outer_rot()
    }

    // ---------------------------------------------------------------------
    // Timelines
    // ---------------------------------------------------------------------

    /// `IEmotePlayer::CountMainTimelines`.
    pub fn main_timeline_count(&self) -> usize {
        self.main_timelines.len()
    }

    /// `IEmotePlayer::GetMainTimelineLabelAt`.
    pub fn main_timeline_label_at(&self, index: usize) -> Option<&str> {
        self.main_timelines.get(index).map(String::as_str)
    }

    /// `IEmotePlayer::CountDiffTimelines`.
    pub fn diff_timeline_count(&self) -> usize {
        self.diff_timelines.len()
    }

    /// `IEmotePlayer::GetDiffTimelineLabelAt`.
    pub fn diff_timeline_label_at(&self, index: usize) -> Option<&str> {
        self.diff_timelines.get(index).map(String::as_str)
    }

    /// `IEmotePlayer::CountPlayingTimelines`.
    pub fn playing_timeline_count(&self) -> usize {
        self.emote.runtime.playing_timeline_info().len()
    }

    /// `IEmotePlayer::GetPlayingTimelineLabelAt`.
    pub fn playing_timeline_label_at(&self, index: usize) -> Option<String> {
        self.emote
            .runtime
            .playing_timeline_info()
            .get(index)
            .map(|(label, _)| label.clone())
    }

    /// `IEmotePlayer::GetPlayingTimelineFlagsAt`.
    pub fn playing_timeline_flags_at(&self, index: usize) -> Option<u32> {
        self.emote
            .runtime
            .playing_timeline_info()
            .get(index)
            .map(|(_, flags)| *flags)
    }

    /// `IEmotePlayer::IsLoopTimeline`.
    pub fn is_loop_timeline(&self, label: &str) -> bool {
        self.emote.runtime.is_loop_timeline(label)
    }

    /// `IEmotePlayer::GetTimelineTotalFrameCount`.
    pub fn timeline_total_frame_count(&self, label: &str) -> Option<f32> {
        self.emote.runtime.timeline_total_frame_count(label)
    }

    /// `IEmotePlayer::PlayTimeline(label, flags)`.
    pub fn play_timeline(&mut self, label: &str, flags: u32) -> Result<(), EmoteError> {
        self.play_timeline_mode(label, TimelinePlayMode::from_flags(flags))
    }

    /// Play with an explicit [`TimelinePlayMode`] (looping/flag control).
    pub fn play_timeline_mode(
        &mut self,
        label: &str,
        mode: TimelinePlayMode,
    ) -> Result<(), EmoteError> {
        self.emote.play_timeline(label, mode)?;
        Ok(())
    }

    /// `IEmotePlayer::IsTimelinePlaying`. An empty label asks whether any
    /// timeline is playing, matching the native runtime.
    pub fn is_timeline_playing(&self, label: &str) -> bool {
        self.emote.runtime.is_timeline_playing(label)
    }

    /// `IEmotePlayer::StopTimeline`. An empty label stops every timeline.
    pub fn stop_timeline(&mut self, label: &str) -> Result<(), EmoteError> {
        self.emote.runtime.stop_timeline(label)?;
        Ok(())
    }

    /// `IEmotePlayer::SetTimelineBlendRatio`.
    pub fn set_timeline_blend_ratio(
        &mut self,
        label: &str,
        value: f32,
        frame_count: f32,
        easing: f32,
        stop_when_blend_done: bool,
    ) -> Result<(), EmoteError> {
        self.emote.runtime.set_timeline_blend_ratio(
            label,
            value,
            frame_count,
            easing,
            stop_when_blend_done,
        )?;
        Ok(())
    }

    /// `IEmotePlayer::GetTimelineBlendRatio`.
    pub fn timeline_blend_ratio(&self, label: &str) -> f32 {
        self.emote.runtime.timeline_blend_ratio(label)
    }

    /// `IEmotePlayer::FadeInTimeline`.
    pub fn fade_in_timeline(
        &mut self,
        label: &str,
        frame_count: f32,
        easing: f32,
    ) -> Result<(), EmoteError> {
        self.emote
            .runtime
            .fade_in_timeline(label, frame_count, easing)?;
        Ok(())
    }

    /// `IEmotePlayer::FadeOutTimeline`.
    pub fn fade_out_timeline(
        &mut self,
        label: &str,
        frame_count: f32,
        easing: f32,
    ) -> Result<(), EmoteError> {
        self.emote
            .runtime
            .fade_out_timeline(label, frame_count, easing)?;
        Ok(())
    }

    /// Jump one timeline to an absolute tick (host convenience).
    pub fn set_timeline_time(&mut self, label: &str, ticks: f32) -> Result<(), EmoteError> {
        self.emote.runtime.set_timeline_time(label, ticks)?;
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Animation, progress and rendering
    // ---------------------------------------------------------------------

    /// `IEmotePlayer::IsAnimating`.
    pub fn is_animating(&self) -> bool {
        self.emote.runtime.is_animating()
            || self.coord_tween.is_some()
            || self.scale_tween.is_some()
            || self.rot_tween.is_some()
    }

    /// `IEmotePlayer::Skip`: finish every pending transition immediately.
    pub fn skip(&mut self) -> Result<(), EmoteError> {
        if let Some(tween) = self.coord_tween.take() {
            let value = tween.finish();
            self.emote.runtime.set_coord(value[0], value[1]);
        }
        if let Some(tween) = self.scale_tween.take() {
            self.emote.runtime.set_scale(tween.finish());
        }
        if let Some(tween) = self.rot_tween.take() {
            self.emote.runtime.set_rot(tween.finish());
        }
        self.emote.runtime.inner_player_mut().skip();
        self.emote.runtime.rebuild_scene()?;
        self.dirty = true;
        Ok(())
    }

    /// `IEmotePlayer::Pass`.
    pub fn pass(&mut self) -> Result<(), EmoteError> {
        self.emote.runtime.pass()?;
        Ok(())
    }

    /// `IEmotePlayer::Progress(frameCount)`.
    ///
    /// `frame_count` is in native 1/60-second ticks.
    pub fn progress(&mut self, frame_count: f32) -> Result<(), EmoteError> {
        if frame_count.is_finite() && frame_count > 0.0 {
            self.step_tweens(frame_count);
        }
        self.emote.runtime.progress_ticks(frame_count.max(0.0))?;
        Ok(())
    }

    /// Advance by wall-clock seconds at the native 60 Hz rate.
    pub fn update_seconds(&mut self, seconds: f32) -> Result<(), EmoteError> {
        self.progress(seconds * EMOTE_TICKS_PER_SECOND)
    }

    /// Advance by milliseconds, capped like the official JS player (100 ms).
    pub fn update_milliseconds_capped(&mut self, milliseconds: f32) -> Result<(), EmoteError> {
        let capped = if milliseconds.is_finite() {
            milliseconds.clamp(0.0, eluna::EMOTE_UPDATE_MS_CAP)
        } else {
            0.0
        };
        self.progress(capped * EMOTE_TICKS_PER_SECOND / 1000.0)
    }

    /// `IEmotePlayer::IsModified`.
    pub fn is_modified(&self) -> bool {
        self.dirty || self.emote.runtime.is_modified()
    }

    /// Explicitly clear the modified flag.
    pub fn clear_modified(&mut self) {
        self.dirty = false;
        self.emote.runtime.clear_modified();
    }

    /// Build the draw plan for the current evaluated scene.
    ///
    /// A hidden player has no draw items, matching `Hide` + `Render`.
    pub fn plan(&self) -> RenderPlan {
        if self.is_hidden() {
            RenderPlan::empty(self.canvas)
        } else {
            self.emote.plan(self.canvas)
        }
    }

    /// Effective camera: official player transform, then the host/auto view.
    pub fn effective_view(&self) -> ViewTransform {
        let inner = self.emote.runtime.inner_player();
        let scale = inner.scale();
        let mut coord = inner.coord();
        // Official `IEmotePlayer::transform_order_mask_t` position channel:
        // `POSITION_TRANSLATE_TO_SCALE` folds the coordinate into the scale
        // before the shader adds it. The default mask sets this bit, so the
        // coordinate is scaled by the player scale in the normal case.
        if (inner.transform_order_mask() & transform_order_mask::POSITION_TRANSLATE_TO_SCALE) != 0 {
            coord[0] *= scale;
            coord[1] *= scale;
        }
        let player = ViewTransform::player(self.model_center, coord, scale, inner.rot());
        player.compose(self.outer_view.unwrap_or(self.auto_view))
    }

    /// `IEmotePlayer::Render` for the wgpu path.
    ///
    /// Returns a view over the player's cached `Rgba8Unorm` target. The kernel
    /// composites it with its own layer transform; no implicit D3D9 target is
    /// involved.
    pub fn render(&mut self) -> Result<GpuFrame, EmoteError> {
        let plan = self.plan();
        let view = self.effective_view();
        let renderer = self
            .renderer
            .as_mut()
            .ok_or(EmoteError::NotReady(Capability::DataOnly))?;
        if self.textures.is_empty() {
            renderer.render_uploaded(&plan, view)
        } else {
            renderer.render(&plan, &self.textures, view)
        }
    }

    /// Read a rendered frame back as tightly packed RGBA8 (screenshots/tests).
    pub fn read_frame(&self, frame: &GpuFrame) -> Result<Vec<u8>, EmoteError> {
        self.renderer
            .as_ref()
            .ok_or(EmoteError::NotReady(Capability::DataOnly))?
            .read_frame(frame)
    }

    /// CPU reference render of the same plan (tests, screenshots, diagnostics).
    pub fn render_cpu(&self) -> Result<(RenderImage, SoftStats), EmoteError> {
        if self.textures.is_empty() {
            return Err(EmoteError::UnsupportedFeature(
                "CPU render after releasing decoded textures",
            ));
        }
        let plan = self.plan();
        let view = self.effective_view();
        soft::render_with_textures(&plan, &self.textures, view)
    }

    // ---------------------------------------------------------------------
    // Stereovision and character profile
    // ---------------------------------------------------------------------

    /// `IEmotePlayer::SetStereovisionEnabled`.
    pub fn set_stereovision_enabled(&mut self, state: bool) -> Result<(), EmoteError> {
        self.emote.runtime.set_stereovision_enabled(state)?;
        Ok(())
    }

    /// `IEmotePlayer::GetStereovisionEnabled`.
    pub fn stereovision_enabled(&self) -> bool {
        self.emote.runtime.stereovision_enabled()
    }

    /// Native stereovision level. The official header calls the same control
    /// `SetStereovisionVolume`; the runtime's name is kept because the native
    /// field is `level`.
    pub fn set_stereovision_level(&mut self, level: f32) -> Result<(), EmoteError> {
        self.emote.runtime.set_stereovision_level(level)?;
        Ok(())
    }

    /// Native stereovision level.
    pub fn stereovision_level(&self) -> f32 {
        self.emote.runtime.stereovision_level()
    }

    /// Native stereovision FOV (official `SetStereovisionParallaxRatio` exists
    /// in later SDK headers but has no recovered equivalence here).
    pub fn set_stereovision_fov(&mut self, fov: f32) -> Result<(), EmoteError> {
        self.emote.runtime.set_stereovision_fov(fov)?;
        Ok(())
    }

    /// Native stereovision FOV.
    pub fn stereovision_fov(&self) -> f32 {
        self.emote.runtime.stereovision_fov()
    }

    /// `IEmotePlayer::SetStereovisionRenderScreen`.
    pub fn set_stereovision_render_screen(&mut self, index: usize) -> Result<(), EmoteError> {
        self.emote.runtime.set_stereovision_screen_index(index)?;
        Ok(())
    }

    /// `IEmotePlayer::GetStereovisionRenderScreen`.
    pub fn stereovision_render_screen(&self) -> usize {
        self.emote.runtime.stereovision_screen_index()
    }

    /// `IEmotePlayer::IsCharaProfileAvailable`.
    pub fn is_chara_profile_available(&self) -> bool {
        self.emote.runtime.is_chara_profile_available()
    }

    /// `IEmotePlayer::GetCharaHeight`.
    pub fn chara_height(&self) -> Option<f32> {
        self.emote.runtime.chara_height()
    }

    /// `IEmotePlayer::CountCharaProfiles`.
    pub fn chara_profile_count(&self) -> usize {
        self.chara_profiles.len()
    }

    /// `IEmotePlayer::GetCharaProfileLabelAt`.
    pub fn chara_profile_label_at(&self, index: usize) -> Option<&str> {
        self.chara_profiles.get(index).map(String::as_str)
    }

    /// `IEmotePlayer::GetCharaProfile`.
    pub fn chara_profile(&self, label: &str) -> Option<f32> {
        self.emote.runtime.chara_profile_value(label)
    }

    // ---------------------------------------------------------------------
    // Internal tweens
    // ---------------------------------------------------------------------

    fn step_tweens(&mut self, ticks: f32) {
        if let Some(tween) = &mut self.coord_tween {
            let (value, done) = tween.step(ticks);
            self.emote.runtime.set_coord(value[0], value[1]);
            if done {
                self.coord_tween = None;
            }
        }
        if let Some(tween) = &mut self.scale_tween {
            let (value, done) = tween.step(ticks);
            self.emote.runtime.set_scale(value);
            if done {
                self.scale_tween = None;
            }
        }
        if let Some(tween) = &mut self.rot_tween {
            let (value, done) = tween.step(ticks);
            self.emote.runtime.set_rot(value);
            if done {
                self.rot_tween = None;
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Tween {
    start: f32,
    target: f32,
    elapsed: f32,
    duration: f32,
    exponent: f32,
}

impl Tween {
    fn new(start: f32, target: f32, duration: f32, exponent: f32) -> Self {
        Self {
            start,
            target,
            elapsed: 0.0,
            duration: duration.max(0.0),
            exponent,
        }
    }

    fn step(&mut self, ticks: f32) -> (f32, bool) {
        self.elapsed = (self.elapsed + ticks).min(self.duration);
        let t = if self.duration <= 0.0 {
            1.0
        } else {
            (self.elapsed / self.duration).clamp(0.0, 1.0)
        };
        let value = self.start + (self.target - self.start) * t.powf(self.exponent);
        (value, t >= 1.0)
    }

    fn finish(self) -> f32 {
        self.target
    }
}

#[derive(Debug, Clone, Copy)]
struct Tween2 {
    start: [f32; 2],
    target: [f32; 2],
    elapsed: f32,
    duration: f32,
    exponent: f32,
}

impl Tween2 {
    fn new(start: [f32; 2], target: [f32; 2], duration: f32, exponent: f32) -> Self {
        Self {
            start,
            target,
            elapsed: 0.0,
            duration: duration.max(0.0),
            exponent,
        }
    }

    fn step(&mut self, ticks: f32) -> ([f32; 2], bool) {
        self.elapsed = (self.elapsed + ticks).min(self.duration);
        let t = if self.duration <= 0.0 {
            1.0
        } else {
            (self.elapsed / self.duration).clamp(0.0, 1.0)
        };
        let eased = t.powf(self.exponent);
        (
            [
                self.start[0] + (self.target[0] - self.start[0]) * eased,
                self.start[1] + (self.target[1] - self.start[1]) * eased,
            ],
            t >= 1.0,
        )
    }

    fn finish(self) -> [f32; 2] {
        self.target
    }
}

/// Native easing conversion (`sub_1026AD10`) used by the official scalar
/// transitions: `0`/non-finite = linear, negative = `1/(1-e)`, positive =
/// `e+1`; the result is the exponent of `pow(progress, exponent)`.
fn native_easing_exponent(easing: f32) -> f32 {
    if !easing.is_finite() || easing == 0.0 {
        1.0
    } else if easing < 0.0 {
        1.0 / (1.0 - easing)
    } else {
        easing + 1.0
    }
}

/// Support classification for one official method family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum OfficialSupport {
    /// Implemented with the official semantics.
    Supported,
    /// State is stored/queried, but the renderer does not consume it yet.
    StateOnly,
    /// Explicitly unavailable; callers get an error or a documented absence.
    Unsupported,
    /// D3D9/COM-specific; not meaningful on the wgpu path.
    NotApplicable,
}

/// Compact support ledger for the official `IEmotePlayer`/`IEmoteDevice`
/// surface. The detailed mapping is in `docs/official-api.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OfficialApiSupport {
    /// `YSEmote.dll` ordinal-1 interface version.
    pub version: u32,
    /// Methods implemented with official semantics.
    pub supported: &'static [&'static str],
    /// Methods whose state is kept but not yet rendered.
    pub state_only: &'static [&'static str],
    /// Methods that are explicitly unavailable.
    pub unsupported: &'static [&'static str],
    /// COM/D3D9-only methods with no wgpu equivalent.
    pub not_applicable: &'static [&'static str],
}

/// Official API support ledger for this build.
pub fn official_api_support() -> &'static OfficialApiSupport {
    &OFFICIAL_API_SUPPORT
}

/// Static support ledger (see [`official_api_support`]).
pub const OFFICIAL_API_SUPPORT: OfficialApiSupport = OfficialApiSupport {
    version: OFFICIAL_INTERFACE_VERSION,
    supported: &[
        "IEmoteDevice::CreatePlayer",
        "IEmoteDevice::SetScreenRect",
        "IEmoteDevice::GetMaskMode",
        "IEmoteDevice::SetMaskMode(alpha)",
        "IEmoteDevice::GetMipMapEnabled",
        "IEmotePlayer::Show",
        "IEmotePlayer::Hide",
        "IEmotePlayer::IsHidden",
        "IEmotePlayer::SetQueuing/GetQueuing",
        "IEmotePlayer::SetHairScale/GetHairScale",
        "IEmotePlayer::SetPartsScale/GetPartsScale",
        "IEmotePlayer::SetBustScale/GetBustScale",
        "IEmotePlayer::SetCoord/GetCoord",
        "IEmotePlayer::SetScale/GetScale",
        "IEmotePlayer::SetRot/GetRot",
        "IEmotePlayer::CountVariables",
        "IEmotePlayer::GetVariableLabelAt",
        "IEmotePlayer::CountVariableFrameAt",
        "IEmotePlayer::GetVariableFrameLabelAt",
        "IEmotePlayer::GetVariableFrameValueAt",
        "IEmotePlayer::SetVariable/GetVariable",
        "IEmotePlayer::SetVariableDiff/GetVariableDiff",
        "IEmotePlayer::SetOuterForce/GetOuterForce",
        "IEmotePlayer::StartWind/StopWind",
        "IEmotePlayer::CountMainTimelines",
        "IEmotePlayer::GetMainTimelineLabelAt",
        "IEmotePlayer::CountDiffTimelines",
        "IEmotePlayer::GetDiffTimelineLabelAt",
        "IEmotePlayer::CountPlayingTimelines",
        "IEmotePlayer::GetPlayingTimelineLabelAt",
        "IEmotePlayer::GetPlayingTimelineFlagsAt",
        "IEmotePlayer::IsLoopTimeline",
        "IEmotePlayer::GetTimelineTotalFrameCount",
        "IEmotePlayer::PlayTimeline",
        "IEmotePlayer::IsTimelinePlaying",
        "IEmotePlayer::StopTimeline",
        "IEmotePlayer::SetTimelineBlendRatio/GetTimelineBlendRatio",
        "IEmotePlayer::FadeInTimeline",
        "IEmotePlayer::FadeOutTimeline",
        "IEmotePlayer::IsAnimating",
        "IEmotePlayer::Skip",
        "IEmotePlayer::Pass",
        "IEmotePlayer::Progress",
        "IEmotePlayer::IsModified",
        "IEmotePlayer::Render",
        "IEmotePlayer::SetStereovisionEnabled/GetStereovisionEnabled",
        "IEmotePlayer::SetStereovisionRenderScreen/GetStereovisionRenderScreen",
        "IEmotePlayer::IsCharaProfileAvailable",
        "IEmotePlayer::GetCharaHeight",
        "IEmotePlayer::CountCharaProfiles",
        "IEmotePlayer::GetCharaProfileLabelAt",
        "IEmotePlayer::GetCharaProfile",
        "IEmotePlayer::SetOuterRot/GetOuterRot",
    ],
    state_only: &[
        "IEmoteDevice::SetMaskRegionClipping/GetMaskRegionClipping",
        "IEmotePlayer::SetColor/GetColor",
        "IEmotePlayer::SetGrayscale/GetGrayscale",
        "IEmotePlayer::SetSmoothing/GetSmoothing",
        "IEmotePlayer::SetMeshDivisionRatio/GetMeshDivisionRatio",
        "IEmoteDevice::SetProtectTranslucentTextureColor",
        "IEmoteDevice::SetMaxTextureSize",
    ],
    unsupported: &[
        "IEmotePlayer::Clone",
        "IEmotePlayer::AssignState",
        "IEmotePlayer::SetColor(timed)",
        "IEmotePlayer::SetGrayscale(timed)",
        "IEmotePlayer::SetStereovisionVolume/GetStereovisionVolume",
        "IEmotePlayer::SetStereovisionParallaxRatio/GetStereovisionParallaxRatio",
        "IEmoteDevice::SetMaskMode(stencil)",
        "IEmoteDevice::SetShaderModel(D3D9)",
        "IEmoteDevice::SetMipMapEnabled(true)",
        "IEmoteDevice::SetAlphaOp",
        "IEmoteDevice::SetPixelateDivision",
    ],
    not_applicable: &[
        "IEmotePlayer::AddRef/Release/RefCount",
        "IEmoteDevice::AddRef/Release/RefCount",
        "IEmoteDevice::OnDeviceLost",
        "IEmoteDevice::OnRenderTarget",
        "IEmoteDevice::GetAvailableShaderModel(D3D9)",
    ],
};
