# yu-emote architecture

## Goals

- Faithful, deterministic E-mote runtime for YU-RIS: PSB model data,
  parameters/motions, mesh deformation and rendering.
- One animation implementation, not a project-local approximation. The
  vendored `eluna` runtime owns scene evaluation; this project adapts data,
  rendering and YU-RIS integration around it.
- Clear separation between **E-mote runtime** and **YU-RIS glue**.

## Non-goals (for now)

- Authoring/editing E-mote models or producing `.psb` for the closed runtime.
- Reusing M2's binaries or the non-commercial FreeMote WebGL runtime in the
  deliverable. They are oracles only.
- Guessing schema fields or animation semantics without evidence.

## Layers

```text
┌──────────────────────────────────────────────────────────────────┐
│ vendor/eluna  (MPL-2.0, upstream 9a00029)                        │
│   psb        PSB/MDF parse, decryption, resources                │
│   emote      schema, motion scenes, mesh chain, anchors, clips   │
│   runtime    timelines, transitions, loops, physics, controls    │
│   sdk        EmoteRuntime (SDK-shaped player state machine)      │
└──────────────────────────────┬───────────────────────────────────┘
                               │ evaluated EmoteStaticScene
┌──────────────────────────────┴───────────────────────────────────┐
│ yu-emote                                                         │
│                                                                  │
│  psb       YU-RIS loader, explicit per-game key, resources       │
│  runtime   Emote<EmoteRuntime + PsbDocument> convenience layer   │
│  texture   RGBA4444 / 32-bpp spec order / RL / PNG decode        │
│  plan      sprite scene -> textured triangle draw list           │
│  soft      deterministic CPU rasterizer (blends, clip, masks)    │
│  yuris     EMOTE/EMOTEINFO, capability, suspend, dirty           │
│  bins      psb-info, psb-dump, psb-key-search, render, anim      │
└──────────────────────────────────────────────────────────────────┘
```

`plan`/`soft` never re-evaluate animation. They consume the sprite list,
world transforms, mesh patches and `DrawFrameInfo` produced by eluna, so the
runtime semantics (transitions, loops, joins, physics) are shared with the
reference implementation.

## YU-RIS glue: suspension and optimization

YU-RIS has its own task scheduler and redraw policy. The adapter exposes:

- `Capability` → mirrored to `SYSTEMINFO[EMOTE]`; `Ready` when the runtime and
  renderer are linked, `DataOnly` for inspection-only hosts.
- `suspend()` / `resume()`: freezes the deterministic clock. A suspended
  character keeps its last pose (engine behavior per the ERIS manual).
- `tick(dt_ms) -> bool` and `take_dirty()`: the host can skip E-mote work and
  re-compositing when nothing changed.
- Parameter recording by name; unknown names are retained, never coerced.

This stays outside the runtime: other hosts can drive `EmoteRuntime` with
their own scheduler.

## PSB body encryption

YU-RIS E-mote files often have a clean header (`flags == 0`) but an encrypted
value body. `PsbDocument::from_bytes(bytes, key)` passes an explicit
`PsbDecryptionKey::emote_key(key)` to the vendored parser. `psb-key-search`
covers the single-DWORD key space. The key is **per game** and is never
hardcoded in the library; tests read it from `YURIS_EMOTE_KEY`.

## Rendering

`plan` reproduces the reference `eluna_player` vertex contract:

- rectangle → center-relative scale → rotation → world transform;
- 4x4 Bezier patch sampling for `mesh.bp`;
- four corner colors in `0xRRGGBBAA` order with alpha × layer opacity;
- native emission order (`DrawFrameInfo`).

`soft` rasterizes with barycentric coverage and bilinear sampling, then applies
the native texture stage (MODULATE2X, modes 3/4 premultiply, mode 5
complement), the D3D9 blend table (`sub_1041A190`), type-7 clip rectangles and
type-12 wipe. Inner stencil owners are resolved from eluna's structural
`native_draw_key` chain and rasterized into union coverage buffers. Outer
stencil phases, feedback history and particles are reported as skipped.

## Stack size

eluna scene evaluation is recursive (nested motions, mesh chains). Debug builds
need a large stack; `.cargo/config.toml` sets a 32 MiB main-thread stack for
`x86_64-pc-windows-msvc` and `RUST_MIN_STACK=33554432` for tests. Library
consumers on other targets should run the runtime on a thread with a comparable
stack.

## Evidence rules

1. Official engine behavior / official DLL output (oracle).
2. Official data (`emote_test.psb`) and the SDK manual.
3. Reference implementations: `eluna` (vendored runtime), `eluna_player`
   (renderer math), FreeMote/NekoWebShow/KrKr2-Next as behavioral references.
4. No evidence → explicit `Unsupported`/`NotReady`, never a guessed value.

## Milestones

- **M1 (done)** PSB v2/v4 data, `RGBA4444`/`RGBA8`/RL textures, per-game key,
  deterministic dumps, YU-RIS adapter.
- **M2 (done via eluna)** timeline command streams, transitions/easing,
  authored loops, DPLAY, nested motions, mesh chain, anchors, clips, joins,
  physics, clamp/eye/selectors. `psb-anim` starts authored loops at `loopBegin`
  and can settle complete loops before recording.
- **M3 (renderer bridge done)** scene → draw plan, native blends, MODULATE2X,
  clip, wipe, inner masks, PNG/WebM output. Static-frame parity against
  `eluna_player` is ~0.3/255 mean absolute RGB error (edge/gamma only).
  Outstanding: outer stencil reference counting, particles, feedback,
  stereovision, wgpu delivery.
- **M4** parity against official `YSEmote.dll` screenshots with declared
  per-channel thresholds.

## Licensing

The project as a whole is MIT OR Apache-2.0. `vendor/eluna` is MPL-2.0 and is
kept with its source, license and `PROVENANCE.md`. `src/plan.rs`,
`src/soft.rs` and `src/texture.rs` adapt renderer math from `eluna_player`
(MIT OR Apache-2.0); see `NOTICE`. FreeMote (CC BY-NC-SA) and NekoWebShow
(AGPL-3.0) / KrKr2-Next (GPL-3.0) are references only, not dependencies.
