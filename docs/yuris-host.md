# YU-RIS host integration

This is the contract for wiring `yu-emote` into the YU-RIS kernel. The kernel's
M0 decision is **RGBA texture + layer transform**, not a new `Op::Emote`; this
document describes that path.

The official `IEmoteDevice`/`IEmotePlayer` method shape is preserved by the
[`player`](official-api.md) facade. Use it for new host code; the lower-level
`Emote` + `GpuRenderer` path below stays available for tools that already own
the model/plan.

## Recommended: official-shaped device/player

```rust
use yu_emote::{EmoteDevice, EmoteDeviceOptions};

// Once, on the kernel's existing wgpu device:
let mut emote_device = EmoteDevice::new(
    ctx.device.clone(),
    ctx.queue.clone(),
    EmoteDeviceOptions { screen: [800, 600], ..Default::default() },
)?;

// Per character:
let mut emote = emote_device.create_player(&psb_bytes, key, None)?;
emote.set_canvas(viewport_w, viewport_h);
// M0 path: leave the outer view unset and let the compositor place this
// texture. `set_view` is for rendering directly into final screen space.
emote.play_timeline("待機ループ00", 0)?;
// Optional: textures are already uploaded; drop the CPU copies.
emote.release_decoded_textures();

// Per frame:
emote.progress(delta_seconds * 60.0)?;
if emote.is_modified() {
    let frame = emote.render()?; // wgpu::TextureView over Rgba8Unorm
    // composite frame.view with the E-mote layer transform
    emote.clear_modified();
}
```

The player exposes the official vocabulary (`show`/`hide`, `set_coord`,
`set_scale`, `set_rot`, `set_variable`, `play_timeline`, `progress`,
`render`, ...). `EmoteDevice::capability()` is `Capability::Ready`. See
[`official-api.md`](official-api.md) for the complete mapping and the
explicitly unsupported methods.

`EmotePlayer` uploads textures during creation. GPU-only hosts can call
`player.release_decoded_textures()` immediately afterwards to drop the CPU
copies; `render()` keeps working, `render_cpu()` then returns
`UnsupportedFeature`. Each player currently owns its renderer state
(pipelines/target/buffers), so multi-character sessions should create players
off the critical path and budget accordingly. `docs/review-2026-11-11.md`
tracks the shared-context and PSB-copy optimizations.

## Device sharing

The kernel already owns `yu-render/wgpu`'s `GpuContext`. Construct the E-mote
renderer on the same device and queue:

```rust
let mut emote_gpu = yu_emote::GpuRenderer::new(
    ctx.device.clone(),
    ctx.queue.clone(),
);
```

`GpuRenderer` creates its own pipelines, sampler and bind-group layouts on that
device. It does not create a second instance/adapter. Target format is
`wgpu::TextureFormat::Rgba8Unorm`, matching `yu-backend-wgpu::TARGET_FORMAT`
and its gamma-space (non-sRGB) blending.

For headless CLI/tests only:

```rust
let (mut renderer, adapter_info) = yu_emote::GpuRenderer::headless()?;
```

## Per-model setup

1. Obtain the `.psb` bytes from `yu-vfs` (the `SP.ST` path or the character
   object resolved by `EMOTE`).
2. Supply the per-game key only from configuration/`YURIS_EMOTE_KEY`; never
   hard-code it in the kernel.
3. `Emote::from_bytes(bytes, key, motion)`.
4. `let textures = emote.textures()?;`
5. `emote_gpu.upload_textures(&textures)?`.

The model is heavy to build (PSB parse + scene evaluation). Create it off the
critical path or behind a loading transition.

## Per-frame path

```rust
// 1) Advance the native 60 Hz timeline/physics.
emote.advance_ticks(delta_ticks)?;

// 2) Evaluate the scene and build the draw plan.
let plan = emote.plan([viewport_w, viewport_h]);

// 3) Upload/encode and submit the GPU pass.
let frame = emote_gpu.render(&plan, &textures, view)?;

// 4) Hand frame.view (wgpu::TextureView) to the compositor.
```

`view` maps model pixels to the E-mote target texture:

```rust
let view = soft::ViewTransform { scale, rotation: 0.0, offset };
```

`rotation` is in radians. `ViewTransform::player(center, coord, scale, rot)`
builds the official player transform, and `ViewTransform::compose` folds it
under a host camera. `EmotePlayer::set_view(Some(view))` installs a
**screen-space** outer camera applied after the player transform; it is not a
model-space fit. Leave it `None` for the fixed fit camera.

Choose the canvas to match the character's on-screen rectangle. Rendering a
full 800×600 target is fine; a tight character-sized target is cheaper and the
host places it with its layer transform. `ViewTransform::fit(&plan, margin)` is
only for previews.

## Compositor handoff

The kernel's `CompositorFrame`/`Op` currently has no external-texture variant.
The M0-compatible options are:

1. **Host composite (smallest kernel change).** Render E-mote to its own
   `TextureView`, then draw it in the host's final pass at the E-mote layer's
   position and z-order. No `yu-gfx` change.
2. **External texture key (additive contract).** Add a backend-neutral
   `TextureRef::External(ExternalTextureId)` and a host registry mapping IDs to
   `wgpu::TextureView`. Both `yu-backend-soft` and `yu-backend-wgpu` resolve it
   at draw time. This keeps z-order inside `CompositorFrame`; it needs a design
   section and owner approval because it touches the shared contract.
3. **`Op::Emote`** is explicitly rejected by the kernel's M0 decision.

Until one is implemented, `SYSTEMINFO[EMOTE]` must stay `0`. Set it to `1`
only when the host can actually present the character.

## EMOTE / EMOTEINFO mapping

The VM emits `Domain::Emote` with named arguments. The host adapter should map:

| Input | Runtime call |
|---|---|
| numeric `BUST`/`HAIR`/`PARTS`/... | `set_variable_timed(name, value, time * 0.06, easing)?` |
| string timeline/motion name | `play_timeline(name, mode)` |
| visible/flag | `runtime.show()/hide()` or the SDK visibility slot |
| unknown name | record, do not guess |
| `EMOTEINFO` | query through the runtime; unknown fields stay explicit |

`EMOTE`'s `TIME` is milliseconds. The official handler converts it with
`× 0.06` (`60/1000`) before calling the player, so `time * 0.06` is already in
the runtime's native 1/60-second tick unit. Do not multiply by 60 again.

## Threading and stack

`eluna` scene evaluation is recursive. Run the runtime and `plan()` on a
thread with at least 32 MiB of stack; the GPU encode path itself is shallow.
The CPU rasterizer additionally uses a worker per band; set
`soft::set_thread_count(n)` if the host needs a fixed budget (`0` = auto).
The GPU path does not use those workers.

## Licensing

`yu-emote` is MIT OR Apache-2.0; the vendored `eluna` runtime is MPL-2.0.
Linking it into the GPL-3.0-or-later kernel is compatible, but the kernel's
"all newly written" statement must be amended to the existing E-mote subsystem
exception. The vendored source and license are in `vendor/eluna/`.
