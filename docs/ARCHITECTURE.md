# yu-emote architecture

## Goals

- Faithful, deterministic reimplementation of the E-mote runtime pieces a
  YU-RIS game needs: PSB model data, parameters/motions, mesh deformation and
  rendering.
- Native and `wasm32-unknown-unknown` from the same Rust code.
- Clear separation between **E-mote core** and **YU-RIS glue**.

## Non-goals (for now)

- Authoring/editing E-mote models or producing `.psb` for the closed runtime.
- Reusing M2's binaries or the non-commercial FreeMote WebGL runtime in the
  deliverable. They are oracles only.
- Guessing schema fields or animation semantics without evidence.

## Layers

```text
┌─────────────────────────────────────────────────────────────┐
│ yu-emote                                                    │
│                                                             │
│  psb      PSB/MDF container (emote-psb, MIT)                │
│   │                                                         │
│  model    generic tree summary; schema extraction (M1)      │
│   │                                                         │
│  anim     parameters, timelines, part visibility (M2)       │
│   │                                                         │
│  render   mesh/skin, atlas, 58 shader variants (M3)         │
│                                                             │
│  yuris    EMOTE/EMOTEINFO, capability, suspend, dirty       │
└─────────────────────────────────────────────────────────────┘
```

Dependency direction is top-down only. `yuris` may depend on `model`/`anim`,
but the core never depends on YU-RIS concepts. The kernel consumes the adapter
through domain events; it does not link the renderer into `yu-gfx`.

## YU-RIS glue: suspension and optimization

YU-RIS has its own task scheduler and redraw policy. The adapter exposes:

- `Capability` → mirrored to `SYSTEMINFO[EMOTE]`; keep `Unavailable`/`DataOnly`
  until a renderer is ready so scripts do not take the E-mote branch.
- `suspend()` / `resume()`: freezes the deterministic clock. A suspended
  character keeps its last pose (engine behavior per the ERIS manual).
- `tick(dt_ms) -> bool` and `take_dirty()`: the host can skip E-mote work and
  re-compositing when nothing changed.
- Parameter recording by name; unknown names are retained, never coerced.

This is deliberately not in the data core: other engines/hosts can drive the
same core with their own scheduler.

## PSB body encryption

YU-RIS E-mote files often have a clean header (`flags == 0`) but an encrypted
value body; the root tag at `entrypoint` is not `0x21`. The body range
`[name_offset, resource_offset)` is XORed with the documented 6-word stream
(`0x075BCD15, 0x159A55E5, 0x1F123BB5, key, 0, 0`). `yu-emote` exposes this as
`read_document_with_key(path, Some(key))` and ships `psb-key-search` for the
single-DWORD key space. The key is **per game** and is never hardcoded in the
library; tests read it from `YURIS_EMOTE_KEY`.

## wasm strategy

- Data layer: pure Rust; `emote-psb` uses only memory/IO abstractions and is
  expected to compile for `wasm32-unknown-unknown`. (Verified at the crate
  level by the upstream project; this repository will add a wasm CI target.)
- Renderer: wgpu/WebGPU, matching the kernel's browser backend. A WebGL2
  fallback, if needed for older browsers, is a separate backend.
- The official `YSEmote.dll`/`emotedriver.dll` are Win32 x86 + D3D9 and cannot
  be used in wasm. They serve as pixel/behavior oracles only.

## Evidence rules

1. Official engine behavior / official DLL output (oracle).
2. Official data (`emote_test.psb`) and the SDK manual.
3. Public tooling behavior (`emote-psb`, `amadeus-emote-loader`, FreeMote,
   `eluna`) as *clues*, not truth.
4. No evidence → explicit `Unsupported`/`NotReady`, never a guessed value.

## Milestones

- **M1** PSB schema/model extraction: metadata, textures, parts, bones,
  motion/timeline structure; deterministic dumps; normalization matching
  `amadeus-emote-loader`'s documented rules (Apache-2.0, with attribution).
- **M2** animation: `VAR/PLAY/DPLAY/STOP/DSTOP/VO`, differential timelines,
  mouth sync; fixed-tick evaluation.
- **M3** renderer: 27 ps_2_0 + 27 ps_3_0 + 4 vertex variants mapped to WGSL;
  atlas, alpha mask, pixelate; mesh skinning.
- **M4** parity: official DLL screenshots vs. this renderer; per-channel
  thresholds declared before measurement.

## Licensing

MIT OR Apache-2.0. Third-party attributions are in `NOTICE`. Apache-2.0 code
adopted from `amadeus-emote-loader` will keep its header. No code from
`eluna` (no license) or FreeMote (CC BY-NC-SA) is copied.
