//! wgpu backend for the evaluated E-mote draw plan.
//!
//! This is the path that matches the YU-RIS kernel: the same `wgpu::Device`
//! can be passed to [`GpuRenderer::new`], the target format is
//! `Rgba8Unorm`, and blending happens on stored (gamma-encoded) values without
//! implicit sRGB conversion.
//!
//! The renderer consumes [`RenderPlan`] items directly:
//!
//! - one vertex/index buffer per frame, one indexed draw per item;
//! - native blend modes 0..5 as separate pipelines;
//! - the same MODULATE2X / mode 3/4/5 texture stage as [`crate::soft`];
//! - type-7 clip and type-12 wipe;
//! - inner stencil owners as full-canvas alpha masks (sources rendered with
//!   MAX blending, then multiplied into the color pass). This matches the CPU
//!   renderer's coverage semantics; GPU stencil reference counting is not used
//!   yet and multi-mask items use the first owner only (counted in stats).
//!
//! The output is a fresh `TextureView` over a cached `Rgba8Unorm` target. The
//! kernel can bind it as a texture; [`GpuRenderer::read_frame`] is for tests
//! and CLI screenshots.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::mpsc;

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt as _;

use crate::error::EmoteError;
use crate::plan::{DrawItem, RenderPlan};
use crate::soft::ViewTransform;
use crate::texture::DecodedTexture;

/// Target format used by the kernel and this renderer.
pub const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

const SHADER: &str = include_str!("../shaders/gpu/emote.wgsl");

/// GPU frame statistics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct GpuStats {
    /// Indexed color draws issued.
    pub draw_calls: u64,
    /// Color triangles represented by those draws.
    pub triangles: u64,
    /// Color vertices uploaded.
    pub vertices: u64,
    /// Mask passes (one per used inner stencil owner).
    pub mask_passes: u64,
    /// Items with feedback history, skipped by the CPU/GPU path.
    pub skipped_feedback: u64,
    /// Items whose texture resource was not uploaded.
    pub missing_textures: u64,
    /// Items with more than one inner mask owner (first owner used).
    pub multi_mask_items: u64,
}

/// A rendered frame. The view stays valid until the next `render` call.
pub struct GpuFrame {
    /// Texture view over the renderer's color target.
    pub view: wgpu::TextureView,
    /// Frame width.
    pub width: u32,
    /// Frame height.
    pub height: u32,
    /// Draw statistics.
    pub stats: GpuStats,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ViewUniform {
    scale: [f32; 2],
    offset: [f32; 2],
    size: [f32; 2],
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuVertex {
    position: [f32; 2],
    texcoord: [f32; 2],
    color: [f32; 4],
    blend_mode: u32,
    clip_rect: [f32; 4],
    wipe: [f32; 3],
    _pad: u32,
}

struct TextureEntry {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

struct Target {
    width: u32,
    height: u32,
    texture: wgpu::Texture,
}

struct MaskTarget {
    width: u32,
    height: u32,
    view: wgpu::TextureView,
}

#[derive(Clone, Copy)]
struct ItemRange {
    item: usize,
    start: u32,
    count: u32,
}

struct Scene {
    vertices: Vec<GpuVertex>,
    indices: Vec<u32>,
    ranges: Vec<ItemRange>,
    stats: GpuStats,
}

/// wgpu renderer for E-mote draw plans.
pub struct GpuRenderer {
    _instance: Option<wgpu::Instance>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    texture_layout: wgpu::BindGroupLayout,
    pipelines: [wgpu::RenderPipeline; 6],
    mask_pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    _white_mask_texture: wgpu::Texture,
    white_mask: wgpu::TextureView,
    textures: BTreeMap<u32, TextureEntry>,
    bind_groups: BTreeMap<(u32, u64), wgpu::BindGroup>,
    target: Option<Target>,
    masks: Vec<MaskTarget>,
    vertex_buffer: Option<wgpu::Buffer>,
    index_buffer: Option<wgpu::Buffer>,
    vertex_capacity: u64,
    index_capacity: u64,
}

impl GpuRenderer {
    /// Build a renderer on an existing device/queue (the kernel path).
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yu-emote view uniform"),
            size: std::mem::size_of::<ViewUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yu-emote view layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<ViewUniform>() as u64
                    ),
                },
                count: None,
            }],
        });
        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yu-emote view bind group"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yu-emote texture layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yu-emote pipeline layout"),
            bind_group_layouts: &[Some(&uniform_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yu-emote shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipelines = std::array::from_fn(|mode| {
            create_color_pipeline(
                &device,
                &pipeline_layout,
                &shader,
                native_blend_state(mode as u32),
                (mode as u32 & 0xFF0F) as usize,
            )
        });
        let mask_pipeline = create_mask_pipeline(&device, &pipeline_layout, &shader);

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yu-emote sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            lod_min_clamp: 0.0,
            lod_max_clamp: 0.0,
            compare: None,
            anisotropy_clamp: 1,
            border_color: None,
        });

        let white_mask_texture = device.create_texture_with_data(
            &queue,
            &wgpu::TextureDescriptor {
                label: Some("yu-emote white mask"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: TARGET_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &[255, 255, 255, 255],
        );
        let white_mask = white_mask_texture.create_view(&wgpu::TextureViewDescriptor::default());

        Self {
            _instance: None,
            device,
            queue,
            uniform_buffer,
            uniform_bind_group,
            texture_layout,
            pipelines,
            mask_pipeline,
            sampler,
            _white_mask_texture: white_mask_texture,
            white_mask,
            textures: BTreeMap::new(),
            bind_groups: BTreeMap::new(),
            target: None,
            masks: Vec::new(),
            vertex_buffer: None,
            index_buffer: None,
            vertex_capacity: 0,
            index_capacity: 0,
        }
    }

    /// Create a headless device and renderer (CLI/tests). The kernel should
    /// use [`GpuRenderer::new`] with its own device instead.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn headless() -> Result<(Self, wgpu::AdapterInfo), EmoteError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            apply_limit_buckets: false,
        }))
        .map_err(|error| EmoteError::Gpu(format!("request_adapter: {error}")))?;
        let info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("yu-emote device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .map_err(|error| EmoteError::Gpu(format!("request_device: {error}")))?;
        let mut renderer = Self::new(device, queue);
        renderer._instance = Some(instance);
        Ok((renderer, info))
    }

    /// Upload decoded textures once per model. Existing resources are kept.
    pub fn upload_textures(
        &mut self,
        textures: &BTreeMap<u32, DecodedTexture>,
    ) -> Result<(), EmoteError> {
        for (&index, texture) in textures {
            if self.textures.contains_key(&index) {
                continue;
            }
            let expected = texture.width as usize * texture.height as usize * 4;
            if texture.width == 0 || texture.height == 0 || texture.rgba8.len() != expected {
                return Err(EmoteError::TextureSizeMismatch {
                    width: texture.width,
                    height: texture.height,
                    expected,
                    actual: texture.rgba8.len(),
                });
            }
            let gpu = self.device.create_texture_with_data(
                &self.queue,
                &wgpu::TextureDescriptor {
                    label: Some("yu-emote atlas page"),
                    size: wgpu::Extent3d {
                        width: texture.width,
                        height: texture.height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: TARGET_FORMAT,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                &texture.rgba8,
            );
            let view = gpu.create_view(&wgpu::TextureViewDescriptor::default());
            self.textures.insert(
                index,
                TextureEntry {
                    _texture: gpu,
                    view,
                },
            );
        }
        Ok(())
    }

    /// Drop all uploaded textures (model switch).
    pub fn clear_textures(&mut self) {
        self.textures.clear();
        self.bind_groups.clear();
    }

    /// Render one plan. `view` maps model pixels to target pixels.
    pub fn render(
        &mut self,
        plan: &RenderPlan,
        textures: &BTreeMap<u32, DecodedTexture>,
        view: ViewTransform,
    ) -> Result<GpuFrame, EmoteError> {
        self.upload_textures(textures)?;
        let width = plan.width.max(1);
        let height = plan.height.max(1);
        let scene = build_scene(plan, textures);
        self.ensure_target(width, height)?;
        self.ensure_buffers(&scene.vertices, &scene.indices);

        let uniform = ViewUniform {
            scale: [view.scale, view.scale],
            offset: view.offset,
            size: [width as f32, height as f32],
            _pad: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniform));
        if !scene.vertices.is_empty() {
            self.queue.write_buffer(
                self.vertex_buffer.as_ref().expect("vertex buffer"),
                0,
                bytemuck::cast_slice(&scene.vertices),
            );
        }
        if !scene.indices.is_empty() {
            self.queue.write_buffer(
                self.index_buffer.as_ref().expect("index buffer"),
                0,
                bytemuck::cast_slice(&scene.indices),
            );
        }

        let mut stats = scene.stats;
        let range_by_item: BTreeMap<usize, ItemRange> = scene
            .ranges
            .iter()
            .map(|range| (range.item, *range))
            .collect();

        let mut group_ids: Vec<usize> = Vec::new();
        for item in &plan.items {
            if !color_item(item) {
                continue;
            }
            if item.resource.is_none() || item_texture(item, textures).is_none() {
                continue;
            }
            for &group in &item.mask_groups {
                if !group_ids.contains(&group) {
                    group_ids.push(group);
                }
            }
        }
        group_ids.sort_unstable();
        self.ensure_masks(group_ids.len(), width, height);

        let mask_slot: BTreeMap<usize, usize> = group_ids
            .iter()
            .enumerate()
            .map(|(slot, group)| (*group, slot))
            .collect();

        let mut mask_draws: Vec<(usize, Range<u32>, wgpu::BindGroup)> = Vec::new();
        for (slot, &group) in group_ids.iter().enumerate() {
            let Some(owner) = plan.mask_groups.get(group) else {
                continue;
            };
            for &source in &owner.sources {
                let Some(range) = range_by_item.get(&source).copied() else {
                    continue;
                };
                let Some(item) = plan.items.get(source) else {
                    continue;
                };
                let Some(resource) = item.resource else {
                    continue;
                };
                let white = self.white_mask.clone();
                let Some(bind) = self.bind_group(resource, &white, 0) else {
                    continue;
                };
                mask_draws.push((slot, range.start..range.start + range.count, bind));
            }
        }

        let mut color_draws: Vec<(u32, Range<u32>, wgpu::BindGroup)> = Vec::new();
        for (index, item) in plan.items.iter().enumerate() {
            if !color_item(item) {
                continue;
            }
            let Some(resource) = item.resource else {
                continue;
            };
            let Some(range) = range_by_item.get(&index).copied() else {
                continue;
            };
            if item.mask_groups.len() > 1 {
                stats.multi_mask_items += 1;
            }
            let mask_slot_index = item
                .mask_groups
                .first()
                .and_then(|group| mask_slot.get(group))
                .copied();
            let (mask_view, mask_key) = match mask_slot_index {
                Some(slot) => (self.masks[slot].view.clone(), slot as u64 + 1),
                None => (self.white_mask.clone(), 0),
            };
            let Some(bind) = self.bind_group(resource, &mask_view, mask_key) else {
                continue;
            };
            color_draws.push((
                item.blend_mode & 0xFF0F,
                range.start..range.start + range.count,
                bind,
            ));
        }
        stats.draw_calls = (color_draws.len() + mask_draws.len()) as u64;
        stats.mask_passes = mask_draws
            .iter()
            .map(|(slot, _, _)| *slot)
            .collect::<std::collections::BTreeSet<_>>()
            .len() as u64;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("yu-emote encoder"),
            });

        if !self.masks.is_empty() {
            for (slot, target) in self.masks.iter().enumerate() {
                let draws: Vec<&(usize, Range<u32>, wgpu::BindGroup)> = mask_draws
                    .iter()
                    .filter(|(draw_slot, _, _)| *draw_slot == slot)
                    .collect();
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("yu-emote mask pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &target.view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.mask_pipeline);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                if let Some(vertex_buffer) = &self.vertex_buffer {
                    pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                }
                if let Some(index_buffer) = &self.index_buffer {
                    pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                }
                for (_, range, bind) in draws {
                    pass.set_bind_group(1, bind, &[]);
                    pass.draw_indexed(range.clone(), 0, 0..1);
                }
            }
        }

        let target = self.target.as_ref().expect("target created");
        let view = target
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("yu-emote color pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            if let Some(vertex_buffer) = &self.vertex_buffer {
                pass.set_vertex_buffer(0, vertex_buffer.slice(..));
            }
            if let Some(index_buffer) = &self.index_buffer {
                pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            }
            let mut current_mode = u32::MAX;
            for (mode, range, bind) in &color_draws {
                if *mode != current_mode {
                    let index = if *mode <= 5 { *mode as usize } else { 0 };
                    pass.set_pipeline(&self.pipelines[index]);
                    current_mode = *mode;
                }
                pass.set_bind_group(1, bind, &[]);
                pass.draw_indexed(range.clone(), 0, 0..1);
            }
        }
        self.queue.submit(Some(encoder.finish()));

        Ok(GpuFrame {
            view,
            width,
            height,
            stats,
        })
    }

    /// Read the last rendered frame back as tightly packed RGBA8.
    pub fn read_frame(&self, frame: &GpuFrame) -> Result<Vec<u8>, EmoteError> {
        let target = self
            .target
            .as_ref()
            .ok_or_else(|| EmoteError::Gpu("no rendered target".to_owned()))?;
        let width = frame.width;
        let height = frame.height;
        let unpadded_row = u64::from(width) * 4;
        let alignment = u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let padded_row = unpadded_row.div_ceil(alignment) * alignment;
        let size = padded_row * u64::from(height);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yu-emote readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("yu-emote readback encoder"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row as u32),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let slice = buffer.slice(..);
        let (sender, receiver) = mpsc::channel();
        self.queue.submit(Some(encoder.finish()));
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| EmoteError::Gpu(format!("poll: {error}")))?;
        receiver
            .recv()
            .map_err(|_| EmoteError::Gpu("readback callback lost".to_owned()))?
            .map_err(|error| EmoteError::Gpu(format!("map: {error}")))?;
        let mapped = slice
            .get_mapped_range()
            .map_err(|error| EmoteError::Gpu(format!("mapped range: {error}")))?;
        let mut rgba = Vec::with_capacity((unpadded_row * u64::from(height)) as usize);
        for row in 0..height as usize {
            let start = row * padded_row as usize;
            rgba.extend_from_slice(&mapped[start..start + unpadded_row as usize]);
        }
        drop(mapped);
        buffer.unmap();
        Ok(rgba)
    }

    fn bind_group(
        &mut self,
        resource: u32,
        mask: &wgpu::TextureView,
        mask_key: u64,
    ) -> Option<wgpu::BindGroup> {
        let key = (resource, mask_key);
        if let Some(bind) = self.bind_groups.get(&key) {
            return Some(bind.clone());
        }
        let entry = self.textures.get(&resource)?;
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yu-emote sprite bind group"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&entry.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(mask),
                },
            ],
        });
        self.bind_groups.insert(key, bind.clone());
        Some(bind)
    }

    fn ensure_target(&mut self, width: u32, height: u32) -> Result<(), EmoteError> {
        if self
            .target
            .as_ref()
            .is_some_and(|target| target.width == width && target.height == height)
        {
            return Ok(());
        }
        let max = self.device.limits().max_texture_dimension_2d;
        if width > max || height > max {
            return Err(EmoteError::Gpu(format!(
                "target {width}x{height} exceeds max_texture_dimension_2d={max}"
            )));
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("yu-emote color target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        self.target = Some(Target {
            width,
            height,
            texture,
        });
        Ok(())
    }

    fn ensure_masks(&mut self, count: usize, width: u32, height: u32) {
        let recreate = self.masks.len() != count
            || self
                .masks
                .iter()
                .any(|mask| mask.width != width || mask.height != height);
        if !recreate {
            return;
        }
        self.masks.clear();
        self.bind_groups.retain(|(_, mask_key), _| *mask_key == 0);
        for _ in 0..count {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("yu-emote mask target"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: TARGET_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            self.masks.push(MaskTarget {
                width,
                height,
                view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            });
        }
    }

    fn ensure_buffers(&mut self, vertices: &[GpuVertex], indices: &[u32]) {
        let vertex_bytes = std::mem::size_of_val(vertices) as u64;
        if self.vertex_buffer.is_none() || self.vertex_capacity < vertex_bytes {
            let capacity = vertex_bytes.next_power_of_two().max(4096);
            self.vertex_buffer = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("yu-emote vertices"),
                size: capacity,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.vertex_capacity = capacity;
        }
        let index_bytes = std::mem::size_of_val(indices) as u64;
        if self.index_buffer.is_none() || self.index_capacity < index_bytes {
            let capacity = index_bytes.next_power_of_two().max(4096);
            self.index_buffer = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("yu-emote indices"),
                size: capacity,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.index_capacity = capacity;
        }
    }
}

fn build_scene(plan: &RenderPlan, textures: &BTreeMap<u32, DecodedTexture>) -> Scene {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut ranges = Vec::new();
    let mut stats = GpuStats::default();
    for (item_index, item) in plan.items.iter().enumerate() {
        if item.resource.is_none() {
            if color_item(item) {
                stats.skipped_feedback += 1;
            }
            continue;
        }
        if item_texture(item, textures).is_none() {
            if color_item(item) {
                stats.missing_textures += 1;
            }
            continue;
        }
        if item.vertices.is_empty() || item.indices.is_empty() {
            continue;
        }
        let base = vertices.len() as u32;
        let clip = item.clip_rect.unwrap_or([-1.0e30, -1.0e30, 1.0e30, 1.0e30]);
        let wipe = [item.wipe[1], item.wipe[2], item.wipe[0]];
        for vertex in &item.vertices {
            vertices.push(GpuVertex {
                position: vertex.position,
                texcoord: vertex.uv,
                color: vertex.color,
                blend_mode: item.blend_mode,
                clip_rect: clip,
                wipe,
                _pad: 0,
            });
        }
        let start = indices.len() as u32;
        for index in &item.indices {
            indices.push(base + index);
        }
        ranges.push(ItemRange {
            item: item_index,
            start,
            count: item.indices.len() as u32,
        });
        if color_item(item) {
            stats.vertices += item.vertices.len() as u64;
            stats.triangles += (item.indices.len() / 3) as u64;
        }
    }
    Scene {
        vertices,
        indices,
        ranges,
        stats,
    }
}

fn color_item(item: &DrawItem) -> bool {
    !matches!(
        item.pass,
        crate::EmoteDrawPass::MaskGeneration | crate::EmoteDrawPass::StencilCompositeMask
    )
}

fn item_texture<'a>(
    item: &DrawItem,
    textures: &'a BTreeMap<u32, DecodedTexture>,
) -> Option<&'a DecodedTexture> {
    item.resource.and_then(|resource| textures.get(&resource))
}

fn native_blend_state(mode: u32) -> wgpu::BlendState {
    use wgpu::{BlendComponent, BlendFactor, BlendOperation};
    let low = mode & 0xFF0F;
    let (operation, src_factor, dst_factor) = match low {
        1 => (BlendOperation::Add, BlendFactor::SrcAlpha, BlendFactor::One),
        2 | 5 => (
            BlendOperation::ReverseSubtract,
            BlendFactor::SrcAlpha,
            BlendFactor::One,
        ),
        3 => (
            BlendOperation::Add,
            BlendFactor::Dst,
            BlendFactor::OneMinusSrcAlpha,
        ),
        4 => (
            BlendOperation::Add,
            BlendFactor::OneMinusDst,
            BlendFactor::One,
        ),
        _ => (
            BlendOperation::Add,
            BlendFactor::SrcAlpha,
            BlendFactor::OneMinusSrcAlpha,
        ),
    };
    let color = BlendComponent {
        src_factor,
        dst_factor,
        operation,
    };
    let alpha = if low == 0 {
        BlendComponent {
            src_factor: BlendFactor::One,
            dst_factor: BlendFactor::OneMinusSrcAlpha,
            operation: BlendOperation::Add,
        }
    } else {
        BlendComponent {
            src_factor: BlendFactor::Zero,
            dst_factor: BlendFactor::One,
            operation: BlendOperation::Add,
        }
    };
    wgpu::BlendState { color, alpha }
}

fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<GpuVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 8,
                shader_location: 1,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 16,
                shader_location: 2,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 32,
                shader_location: 3,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 36,
                shader_location: 4,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 52,
                shader_location: 5,
            },
        ],
    }
}

fn create_color_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    blend: wgpu::BlendState,
    _mode: usize,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("yu-emote color pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(vertex_layout())],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: TARGET_FORMAT,
                blend: Some(blend),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn create_mask_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
) -> wgpu::RenderPipeline {
    let max = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Max,
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("yu-emote mask pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(vertex_layout())],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_mask"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: TARGET_FORMAT,
                blend: Some(wgpu::BlendState {
                    color: max,
                    alpha: max,
                }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}
