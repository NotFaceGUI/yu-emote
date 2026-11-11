# Official E-mote API mapping

This is the interface ledger for the kernel handoff: what the official E-mote
SDK provides, what `yu-emote` exposes in return, and what is deliberately
missing. The implementation is [`src/player.rs`](../src/player.rs).

## Evidence

- Official interface names and signatures: the auto-generated
  [`iemote.h`](https://github.com/Project-AZUSA/FreeMote-SDK/blob/master/Windows-DX9/iemote.h)
  header shipped with the E-mote SDK (`IEmoteDevice`, `IEmotePlayer`,
  `EmoteCreate`, `EmoteFilterTexture`). It is used as an interface reference;
  no official code is copied.
- YU-RIS 0.495 loading path: `LoadLibraryA("YSEmote.dll")`, `GetProcAddress`
  ordinal `1..3` and `101..256`; `YSEmote.dll` forwards to `emotedriver.dll`.
  Ordinal 1 returns `1000`. Device slots are `101..121`, player slots
  `201..256`. See the kernel's `docs/emote-parity.md` §1.4 and §1.5.
- Runtime behavior: the vendored `eluna` runtime, which already carries the
  recovered timeline/transition/physics semantics.

`yu-emote` keeps the official **method vocabulary and argument order**. Rust
does not have default arguments, so official methods with
`frameCount = 0, easing = 0` defaults have an immediate variant and a
`*_timed` variant. The `frameCount` unit is the native 1/60-second tick, the
same unit as `Progress` and `SetVariable`.

## Device (`IEmoteDevice`)

| Official | `yu-emote` | Status |
|---|---|---|
| `EmoteCreate(InitParam)` | `EmoteDevice::new(device, queue, options)?` / `EmoteDevice::headless(options)?` | Supported; wgpu instead of D3D9, no `InitParam` |
| `CreatePlayer(image, size, out**)` | `EmoteDevice::create_player(bytes, key, motion)` | Supported |
| `CreatePlayer(num, images, sizes, out**)` | `EmoteDevice::create_player_from_emote` per model | Multi-object PSB is not a supported asset shape |
| `SetScreenRect(w, h)` | `EmoteDevice::set_screen_rect` / `EmotePlayer::set_canvas` | Supported |
| `SetMaskMode` / `GetMaskMode` | `set_mask_mode` / `mask_mode` | Alpha supported; stencil returns `UnsupportedFeature` |
| `SetMaskRegionClipping` / `Get…` | `set_mask_region_clipping` / `mask_region_clipping` | State only; alpha coverage is always clipped to the mask region (official default `true`) |
| `SetProtectTranslucentTextureColor` / `Get…` | `set_protect_translucent_texture_color` / `…` | State only; renderer does not consume it yet |
| `GetAvailableShaderModel` / `SetShaderModel` / `GetShaderModel` | `available_shader_model` / `set_shader_model` / `shader_model` | Reports `ShaderModel::Wgpu`; D3D9 SM2/SM3 rejected |
| `SetMipMapEnabled` / `Get…` | `set_mip_map_enabled` / `mip_map_enabled` | `true` returns `UnsupportedFeature`; `Get` is always `false` |
| `SetAlphaOp` / `GetAlphaOp` | — | Unsupported; native blend table is fixed in the pipelines |
| `SetPixelateDivision` / `Get…` | — | Unsupported; pixelate is outstanding M3 work |
| `SetMaxTextureSize` | `set_max_texture_size` / `max_texture_size` | Stored; wgpu limits still apply |
| `AddRef` / `Release` / `RefCount` | Rust ownership | Not applicable |
| `OnDeviceLost` / `OnRenderTarget` | — | Not applicable; wgpu owns device-loss handling and the host composites the returned view |
| `EmoteFilterTexture` | — | Not implemented (per-body-key texture filter step belongs to the loader) |

## Player state (`IEmotePlayer`)

| Official | `yu-emote` | Status |
|---|---|---|
| `Show` / `Hide` / `IsHidden` | `show` / `hide` / `is_hidden` | Supported; hidden players produce an empty plan |
| `SetSmoothing` / `GetSmoothing` | `set_smoothing` / `smoothing` | State only; the runtime stores it, no consumer yet |
| `SetMeshDivisionRatio` / `Get…` | `set_mesh_division_ratio` / `mesh_division_ratio` | State only; tessellation still comes from authored plan data |
| `SetQueuing` / `GetQueuing` | `set_queuing` / `queuing` | Supported |
| `SetHairScale` / `Get…` | `set_hair_scale` / `hair_scale` | Supported |
| `SetPartsScale` / `Get…` | `set_parts_scale` / `parts_scale` | Supported |
| `SetBustScale` / `Get…` | `set_bust_scale` / `bust_scale` | Supported |
| `SetCoord` / `GetCoord` | `set_coord` / `set_coord_timed` / `get_coord` | Supported, including timed easing |
| `SetScale` / `GetScale` | `set_scale` / `set_scale_timed` / `get_scale` | Supported, including timed easing |
| `SetRot` / `GetRot` | `set_rot` / `set_rot_timed` / `get_rot` | Supported; radians, same as the runtime/reference player |
| `SetColor` / `GetColor` | `set_color` / `get_color` | State only; `frameCount > 0` is rejected because the runtime does not interpolate it |
| `SetGrayscale` / `Get…` | `set_grayscale` / `get_grayscale` | State only; `frameCount > 0` is rejected |
| `Clone` / `AssignState` | — | Unsupported |
| `AddRef` / `Release` / `RefCount` | Rust ownership | Not applicable |

`SetCoord`/`SetScale`/`SetRot` feed the same player transform as the reference
renderer: `rotate((point - center) * player_scale) + coord`, then the host/auto
camera. `ViewTransform::player` and `ViewTransform::compose` implement this.
The default camera is a **fixed** fit computed at creation/`set_canvas`, unlike
the reference viewer's per-frame auto-fit; this keeps animation previews
stable. Use `set_view` for explicit framing.

## Variables

| Official | `yu-emote` | Status |
|---|---|---|
| `CountVariables` | `variable_count` | Supported; order is deterministic but label-sorted |
| `GetVariableLabelAt` | `variable_label_at` | Supported |
| `CountVariableFrameAt` | `variable_frame_count_at` | Supported |
| `GetVariableFrameLabelAt` | `variable_frame_label_at` | Supported |
| `GetVariableFrameValueAt` | `variable_frame_value_at` | Supported |
| `SetVariable` / `GetVariable` | `set_variable` / `set_variable_timed` / `get_variable` | Supported; zero ticks applies immediately, positive ticks starts a native transition. `get_variable` returns `Option<f32>` so unknown labels stay explicit |
| `SetVariableDiff` / `GetVariableDiff` | `set_variable_diff` / `get_variable_diff` | Supported |

`EmotePlayer::set_variables` is a `yu-emote` batch helper for one `EMOTE`
call: it applies the values immediately and rebuilds the scene once instead of
once per variable.

## Timelines

| Official | `yu-emote` | Status |
|---|---|---|
| `CountMainTimelines` / `GetMainTimelineLabelAt` | `main_timeline_count` / `main_timeline_label_at` | Supported |
| `CountDiffTimelines` / `GetDiffTimelineLabelAt` | `diff_timeline_count` / `diff_timeline_label_at` | Supported |
| `CountPlayingTimelines` / `GetPlayingTimelineLabelAt` / `GetPlayingTimelineFlagsAt` | `playing_timeline_count` / `playing_timeline_label_at` / `playing_timeline_flags_at` | Supported |
| `IsLoopTimeline` | `is_loop_timeline` | Supported |
| `GetTimelineTotalFrameCount` | `timeline_total_frame_count` | Supported (returns `Option<f32>`) |
| `PlayTimeline(label, flags)` | `play_timeline(label, flags)` | Supported (flags `PARALLEL`/`DIFFERENCE`) |
| `IsTimelinePlaying(label)` | `is_timeline_playing` | Supported; empty label means “any” |
| `StopTimeline(label)` | `stop_timeline` | Supported; empty label stops all |
| `SetTimelineBlendRatio` / `Get…` | `set_timeline_blend_ratio` / `timeline_blend_ratio` | Supported |
| `FadeInTimeline` / `FadeOutTimeline` | `fade_in_timeline` / `fade_out_timeline` | Supported |

## Animation and render

| Official | `yu-emote` | Status |
|---|---|---|
| `IsAnimating` | `is_animating` | Supported (includes facade tweens) |
| `Skip` | `skip` | Supported |
| `Pass` / `Progress` | `pass` / `progress` | Supported; `progress` takes 1/60-second ticks |
| `IsModified` | `is_modified` / `clear_modified` | Supported |
| `Render` | `render` → `GpuFrame` | Returns a `TextureView` over a cached `Rgba8Unorm` target; the host composites it. `render_cpu` is the deterministic CPU oracle |
| `SetOuterForce` / `GetOuterForce` | `set_outer_force` / `get_outer_force` | Supported |
| `StartWind` / `StopWind` | `start_wind` / `stop_wind` | Supported |
| `SetOuterRot` / `GetOuterRot` | `set_outer_rot` / `get_outer_rot` | Supported |

## Stereovision and character profile

| Official | `yu-emote` | Status |
|---|---|---|
| `SetStereovisionEnabled` / `Get…` | `set_stereovision_enabled` / `stereovision_enabled` | Supported |
| `SetStereovisionVolume` / `Get…` | `set_stereovision_level` / `stereovision_level` | Renamed to the native field; volume semantics are not separately evidenced |
| `SetStereovisionParallaxRatio` / `Get…` | `set_stereovision_fov` / `stereovision_fov` | Native FOV is exposed; the official parallax-ratio mapping is not evidenced |
| `SetStereovisionRenderScreen` / `Get…` | `set_stereovision_render_screen` / `stereovision_render_screen` | Supported |
| `IsCharaProfileAvailable` | `is_chara_profile_available` | Supported |
| `GetCharaHeight` | `chara_height` | Supported (returns `Option<f32>`) |
| `CountCharaProfiles` / `GetCharaProfileLabelAt` / `GetCharaProfile` | `chara_profile_count` / `chara_profile_label_at` / `chara_profile` | Supported |

## Kernel usage

```rust
// Once, on the kernel's existing wgpu device:
let device = yu_emote::EmoteDevice::new(ctx.device.clone(), ctx.queue.clone(), Default::default())?;
device.set_screen_rect(viewport_w, viewport_h);

// Per character:
let mut player = device.create_player(&psb_bytes, Some(key), None)?;
player.set_canvas(viewport_w, viewport_h);
// M0: leave the view unset; the compositor places this texture.
// Use `player.set_view(Some(host_view))` only for direct screen-space render.
player.play_timeline("待機ループ00", 0)?;
// Optional memory saving: textures are already uploaded on the GPU.
player.release_decoded_textures();

// Per frame:
player.progress(delta_seconds * 60.0)?;
if player.is_modified() {
    let frame = player.render()?;
    // composite frame.view at the character layer
    player.clear_modified();
}
```

`EmoteDevice::capability()` returns `Capability::Ready`; keep
`SYSTEMINFO[EMOTE]` at `0` until the host actually presents the returned
texture, as required by the kernel M0 decision.

### `EMOTE` / `EMOTEINFO` mapping

The kernel emits named `Domain::Emote` arguments. The official ERIS wrappers
(`es_Emote.yst`, `es_Sprite.yst`) show the parameter names; the host adapter
maps:

| Script input | Player call |
|---|---|
| `ID` | host player-map key / layer association |
| `VAR`, `VAR2`, `VARNUM`, `TIME` | `set_variable_timed(var, var2, time_ms * 0.06, 0.0)?`; `VARNUM` is a batch; use `set_variables` when `TIME == 0` |
| `LABEL`, `PLAY=1` | `play_timeline(label, 0)?` (normal play) |
| `LABEL`, `PLAY=2` | additive play; tentatively `play_timeline(label, PARALLEL_DIFFERENCE.flags)` — verify against the oracle |
| `LABEL`, `STOP=1` | `stop_timeline(label)?` |
| `LABEL`, `STOP=2`, `TIME` | tentatively `fade_out_timeline(label, time_ms * 0.06, 0.0)?` |
| `SKIP=1` | `skip()?` |
| `DATA` | host-owned model identity/load; not a player method |
| visible/enable | `show()` / `hide()`; the exact parameter name is still being evidenced, record unknown names |

`EMOTEINFO` fields observed in the official wrappers:

| Field | Player call |
|---|---|
| `ANIMATION=1` | `is_animating()` → `1`/`0` |
| `LABEL=... PLAY=1` | `is_timeline_playing(label)` → `1`/`0` |
| `DATA=1` | host-owned E-mote data ID string |
| anything else | explicit `None`/error; do not invent |

`Value::Str` is raw bytes. Decode it with the project's script codec
(CP932/patch codec) before passing a label to `&str` methods. `TIME` is
milliseconds; `time_ms * 0.06` is already the native tick count.

`time` in `EMOTE` is milliseconds. The official handler multiplies it by
`0.06` before calling the player (`0.06 = 60/1000`), so `time * 0.06` is
already the native 1/60-second `frameCount` unit. Do not multiply by 60 again.
Do not hard-code parameter slot numbers; names are the stable surface.

## Known gaps

These are tracked, not guessed:

- Timed `SetColor`/`SetGrayscale` (the runtime stores a single value).
- `SetSmoothing` and `SetMeshDivisionRatio` are stored but not consumed by the
  scene builder or renderer.
- Timed `SetCoord`/`SetScale`/`SetRot` are implemented as facade tweens; the
  native player transition queue/`SetQueuing` interaction for those three is
  not evidenced.
- Stencil mask mode, pixelate, outer-stencil reference counting, type-4
  particles and type-10 feedback history (renderer work).
- D3D9 shader-model/mipmap/alpha-op selection.
- `EmoteFilterTexture` (loader-side texture filter step).
- Index enumeration order is label-sorted, not authored PSB order. Name-based
  access is exact.
