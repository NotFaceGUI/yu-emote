//! Interactive window viewer for a single PSB model.
//!
//! ```text
//! psb-window <model.psb> [--key HEX] [--canvas WxH] [--scale S]
//! ```
//!
//! Controls:
//! - scroll: zoom (`SetScale`)
//! - left drag: move (`SetCoord`)
//! - `1`..`5`: face presets (neutral / smile / angry / closed / surprised)
//! - `R`: reset view, `Esc`: quit
//!
//! The model advances at the native 60 Hz and renders through
//! `EmotePlayer::render`; the resulting `GpuFrame` (the same one the kernel
//! host consumes) is blitted to the window surface.

use std::sync::Arc;
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use yu_emote::{
    Emote, EmoteDevice, EmoteDeviceOptions, EmoteMaskMode, EmotePlayer, EmotePlayerOptions,
};

const BLIT_WGSL: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VsOut {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    var t = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );
    var out: VsOut;
    out.pos = vec4<f32>(p[i], 0.0, 1.0);
    out.uv = t[i];
    return out;
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(src, samp, in.uv);
}
"#;

struct Gfx {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bind_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    canvas: [u32; 2],
    zoom: f32,
    coord: [f32; 2],
}

struct State {
    gfx: Gfx,
    player: EmotePlayer,
    last: Instant,
    accum: f32,
    frames: u64,
}

struct App {
    path: String,
    key: Option<u32>,
    canvas: Option<[u32; 2]>,
    scale: f32,
    state: Option<State>,
    dragging: bool,
    last_cursor: (f64, f64),
}

fn acquire(
    surface: &wgpu::Surface<'_>,
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
) -> Result<wgpu::SurfaceTexture, String> {
    match surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(frame)
        | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => Ok(frame),
        wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
            surface.configure(device, config);
            match surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(frame)
                | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => Ok(frame),
                other => Err(format!("获取 surface 纹理失败: {other:?}")),
            }
        }
        other => Err(format!("获取 surface 纹理失败: {other:?}")),
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let emote = match Emote::load(&self.path, self.key, None) {
            Ok(emote) => emote,
            Err(error) => {
                eprintln!("加载失败: {error}");
                event_loop.exit();
                return;
            }
        };
        let natural = emote
            .screen_size()
            .map(|s| [s[0].max(1.0) as u32, s[1].max(1.0) as u32])
            .unwrap_or([800, 600]);
        let canvas = self.canvas.unwrap_or(natural);

        let win_w = (canvas[0] as f64 * 0.8).clamp(480.0, 1500.0);
        let win_h = (canvas[1] as f64 * 0.8).clamp(360.0, 1250.0);
        let attrs = Window::default_attributes()
            .with_title(format!("psb-window · {}x{}", canvas[0], canvas[1]))
            .with_inner_size(LogicalSize::new(win_w, win_h));
        let window = match event_loop.create_window(attrs) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                eprintln!("创建窗口失败: {error}");
                event_loop.exit();
                return;
            }
        };

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = match instance.create_surface(window.clone()) {
            Ok(surface) => surface,
            Err(error) => {
                eprintln!("创建 surface 失败: {error}");
                event_loop.exit();
                return;
            }
        };
        let adapter = match pollster::block_on(instance.request_adapter(
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
                apply_limit_buckets: false,
            },
        )) {
            Ok(adapter) => adapter,
            Err(error) => {
                eprintln!("无可用适配器: {error}");
                event_loop.exit();
                return;
            }
        };
        let (device, queue) = match pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("psb-window"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults()
                    .using_resolution(adapter.limits()),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            },
        )) {
            Ok(pair) => pair,
            Err(error) => {
                eprintln!("创建设备失败: {error}");
                event_loop.exit();
                return;
            }
        };

        let caps = surface.get_capabilities(&adapter);
        // 优先非 sRGB:玩家纹理会按存储值混合,不能隐式线性化。
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| {
                matches!(
                    f,
                    wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
                )
            })
            .unwrap_or(caps.formats[0]);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit-layout"),
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
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit-pl"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blit-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let device_facade = match EmoteDevice::new(
            device.clone(),
            queue.clone(),
            EmoteDeviceOptions {
                screen: canvas,
                mask_mode: EmoteMaskMode::DEFAULT,
                mask_region_clipping: true,
                protect_translucent_texture_color: true,
            },
        ) {
            Ok(facade) => facade,
            Err(error) => {
                eprintln!("E-mote 设备初始化失败: {error}");
                event_loop.exit();
                return;
            }
        };
        let player = match device_facade.create_player_from_emote_with_options(
            emote,
            EmotePlayerOptions {
                canvas,
                view: None,
                margin: 20.0,
            },
        ) {
            Ok(player) => player,
            Err(error) => {
                eprintln!("创建玩家失败: {error}");
                event_loop.exit();
                return;
            }
        };

        window.request_redraw();
        self.state = Some(State {
            gfx: Gfx {
                window,
                surface,
                config,
                device,
                queue,
                pipeline,
                bind_layout,
                sampler,
                canvas,
                zoom: self.scale,
                coord: [0.0, 0.0],
            },
            player,
            last: Instant::now(),
            accum: 0.0,
            frames: 0,
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                use winit::keyboard::{Key, NamedKey};
                match event.logical_key {
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    Key::Character(ref ch) => match ch.as_str() {
                        "1" => {
                            let _ = state.player.set_variable("face_mouth", 0.0);
                            let _ = state.player.set_variable("face_eye_open", 0.0);
                        }
                        "2" => {
                            let _ = state.player.set_variable("face_mouth", 30.0);
                            let _ = state.player.set_variable("face_eyebrow", 40.0);
                        }
                        "3" => {
                            let _ = state.player.set_variable("face_mouth", 10.0);
                            let _ = state.player.set_variable("face_eyebrow", 20.0);
                        }
                        "4" => {
                            let _ = state.player.set_variable("face_eye_open", 10.0);
                        }
                        "5" => {
                            let _ = state.player.set_variable("head_slant", 30.0);
                        }
                        "r" | "R" => {
                            state.gfx.zoom = 1.0;
                            state.gfx.coord = [0.0, 0.0];
                            state.player.set_scale(1.0);
                            state.player.set_coord(0.0, 0.0);
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let d = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => (p.y as f32) / 60.0,
                };
                state.gfx.zoom = (state.gfx.zoom * (1.0 + d * 0.1)).clamp(0.1, 4.0);
                state.player.set_scale(state.gfx.zoom);
            }
            WindowEvent::MouseInput {
                state: st,
                button: MouseButton::Left,
                ..
            } => {
                self.dragging = st == ElementState::Pressed;
            }
            WindowEvent::CursorMoved { position, .. } => {
                if self.dragging {
                    let (lx, ly) = self.last_cursor;
                    let dx = (position.x - lx) as f32;
                    let dy = (position.y - ly) as f32;
                    state.gfx.coord[0] += dx;
                    state.gfx.coord[1] += dy;
                    state.player.set_coord(state.gfx.coord[0], state.gfx.coord[1]);
                }
                self.last_cursor = (position.x, position.y);
            }
            WindowEvent::Resized(size) => {
                state.gfx.config.width = size.width.max(1);
                state.gfx.config.height = size.height.max(1);
                state.gfx.surface.configure(&state.gfx.device, &state.gfx.config);
            }
            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                let dt = now.duration_since(state.last).as_secs_f32();
                state.last = now;
                state.accum += dt * 60.0;
                let ticks = state.accum.floor().min(8.0);
                state.accum -= ticks;
                if ticks > 0.0 {
                    if let Err(error) = state.player.progress(ticks) {
                        eprintln!("progress: {error}");
                    }
                }
                let frame = match state.player.render() {
                    Ok(frame) => frame,
                    Err(error) => {
                        eprintln!("render: {error}");
                        return;
                    }
                };
                if let Err(error) = state.present(&frame) {
                    eprintln!("present: {error}");
                }
                state.frames += 1;
                if state.frames % 60 == 0 {
                    state.gfx.window.set_title(&format!(
                        "psb-window · {}x{} · frame {} · zoom {:.2}",
                        state.gfx.canvas[0],
                        state.gfx.canvas[1],
                        state.frames,
                        state.gfx.zoom
                    ));
                }
                state.gfx.window.request_redraw();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(state) = self.state.as_ref() {
            state.gfx.window.request_redraw();
        }
    }
}

impl State {
    fn present(&mut self, frame: &yu_emote::GpuFrame) -> Result<(), String> {
        let gfx = &mut self.gfx;
        let (fw, fh) = (frame.width as f32, frame.height as f32);
        let (ww, wh) = (gfx.config.width as f32, gfx.config.height as f32);
        let s = (ww / fw).min(wh / fh);
        let (dw, dh) = (fw * s, fh * s);
        let (ox, oy) = ((ww - dw) * 0.5, (wh - dh) * 0.5);

        let bind = gfx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit-bind"),
            layout: &gfx.bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&frame.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&gfx.sampler),
                },
            ],
        });

        let surface_tex = acquire(&gfx.surface, &gfx.device, &gfx.config)?;
        let view = surface_tex
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = gfx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("blit") });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blit"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.07,
                            g: 0.07,
                            b: 0.08,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // 等比装入窗口:只在内容矩形里画。
            pass.set_viewport(ox, oy, dw, dh, 0.0, 1.0);
            pass.set_pipeline(&gfx.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        gfx.queue.submit(Some(enc.finish()));
        gfx.queue.present(surface_tex);
        Ok(())
    }
}

fn main() {
    // winit 要求 EventLoop 建在进程主线程(`initializing the event loop
    // outside of the main thread is a cross-platform hazard`)。PSB v2 的
    // 嵌套深度(36 层)不需要内核那种 64 MiB 工作线程栈;若以后要喂
    // PSB v4,再按平台开关 `EventLoopBuilderExtWindows::with_any_thread`。
    run();
}

fn run() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("用法: psb-window <model.psb> [--key HEX] [--canvas WxH] [--scale S]");
        return;
    };
    let mut key = None;
    let mut canvas = None;
    let mut scale = 1.0f32;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--key" => {
                let raw = args.next().unwrap_or_default();
                let hex = raw.trim_start_matches("0x").trim_start_matches("0X");
                key = u32::from_str_radix(hex, 16).ok();
            }
            "--canvas" => {
                let raw = args.next().unwrap_or_default();
                if let Some((w, h)) = raw.split_once('x') {
                    canvas = Some([w.parse().unwrap_or(800), h.parse().unwrap_or(600)]);
                }
            }
            "--scale" => {
                scale = args.next().unwrap_or_default().parse().unwrap_or(1.0);
            }
            other => {
                eprintln!("未知参数: {other}");
                return;
            }
        }
    }

    let event_loop = match EventLoop::new() {
        Ok(loop_) => loop_,
        Err(error) => {
            eprintln!("事件循环创建失败: {error}");
            return;
        }
    };
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        path,
        key,
        canvas,
        scale,
        state: None,
        dragging: false,
        last_cursor: (0.0, 0.0),
    };
    if let Err(error) = event_loop.run_app(&mut app) {
        eprintln!("事件循环失败: {error}");
    }
}
