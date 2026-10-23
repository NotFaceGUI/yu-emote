# yu-emote

E-mote (EMT/PSB) runtime for YU-RIS, written in Rust. The animation core is
the vendored [`eluna`](https://github.com/xmoezzz/eluna) runtime; this project
adds the YU-RIS/PSB loading path, texture decoding, the renderer bridge and the
demo/tooling.

> E-mote is a 2D character-animation middleware by M2 Co., Ltd. This project is
> not affiliated with M2. It only reads E-mote data files.

## Architecture

```
vendor/eluna/        vendored eluna_rs (MPL-2.0)
  PSB parse, decryption, motion/timeline evaluation, mesh chain,
  anchors, clips, joins, physics, clamp/eye/selectors, SDK player state
src/psb.rs           YU-RIS PSB/MDF loading + explicit per-game key
src/runtime.rs       load/drive wrapper around eluna::EmoteRuntime
src/texture.rs       RGBA4444 / 32-bpp / RL / PNG texture decoding
src/plan.rs          evaluated scene -> textured triangle draw plan
src/soft.rs          deterministic CPU rasterizer (blends, clip, inner masks)
src/yuris.rs         YU-RIS EMOTE/EMOTEINFO adapter state
src/bin/*            psb-info, psb-dump, psb-key-search, psb-render, psb-anim
```

There is no second, project-local animation implementation. Scene evaluation,
timeline command streams, transitions, loops, physics and masks all come from
`eluna::EmoteRuntime`. See `vendor/eluna/PROVENANCE.md` for the upstream commit
and the single local PSB v4 parsing fix.

## Status

| Layer | State |
|---|---|
| PSB/MDF parsing, body decryption, key search | ✅ (vendored eluna) |
| E-mote motion/timeline runtime (60 Hz, transitions, easing, loops, DPLAY, joins) | ✅ (vendored eluna) |
| Mesh-chain deformation, anchors, type-7 clip, composite masks | ✅ (vendored eluna) |
| Bust/hair/parts physics, clamp, eye/selector controls | ✅ (vendored eluna) |
| YU-RIS adapter: `EMOTE` params, `EMOTEINFO`, capability, suspend/dirty | ✅ |
| Texture decode: `RGBA4444`, 32-bpp spec order, RL, PNG fallback | ✅ |
| Draw plan + CPU rasterizer: native blends 0..5, MODULATE2X, clip, wipe, inner masks | ✅ |
| CLI: info/dump/key search/render/timeline PNG sequence | ✅ |
| Demo: 4 real models, expressions, 30 fps alpha-WebM idle loop + one-shot gesture | ✅ |
| wgpu delivery pipeline | ⏳ |
| Outer-stencil reference counting, type-4 particles, type-10 feedback, stereovision in the CPU renderer | ⏳ |
| Pixel parity vs. official `YSEmote.dll` | ⏳ M4 |

Static-frame renderer parity was checked against the reference `eluna_player`
WebGPU output at the same camera: mean absolute RGB difference is ~0.3/255 and
the amplified diff shows only edge/gamma differences, with no displaced
geometry. Animation frames are produced by the same evaluated scene, so
detached parts and missing transitions are fixed at the runtime layer rather
than patched in the renderer.

## Usage

```bash
# Model summary (motions, timelines, variables, textures, scene)
cargo run --release --bin psb-info -- /path/to/model.psb

# Body-encrypted PSB: recover the per-game key, then pass it explicitly
cargo run --release --bin psb-key-search -- /path/to/model.psb
cargo run --release --bin psb-info -- /path/to/model.psb --key 0x...

# Deterministic dump (+ raw tree, resources, decoded textures)
cargo run --release --bin psb-dump -- model.psb --raw --resources out --decode

# One frame at tick 0 / at a timeline time, with variable-driven expressions
cargo run --release --bin psb-render -- model.psb --time 0 --out frame.png
cargo run --release --bin psb-render -- model.psb --timeline 待機ループ00 --time 60 \
  --var face_mouth=30 --var face_eyebrow=40 --out smile.png

# Timeline to PNG sequence (native 60 Hz stepping, fixed camera)
cargo run --release --bin psb-anim -- model.psb --timeline 待機ループ00 \
  --fps 30 --seconds 7 --out-dir frames
```

`--view SCALE,X,Y` pins the camera so every frame/pose of a clip shares the same
transform. Without it, the first rendered frame is fitted into `--canvas WxH`
(default `900x1400`) with `--margin` (default 20 px).

The per-game E-mote key is **never** hard-coded in the library. CLI tools take
`--key`; tests take `YURIS_EMOTE_KEY`. Do not commit a game key or game assets.

## Demo

```bash
cargo build --release --bins
python demo/make_demo.py \
  --model "Azuki=/path/to/azuki-maid.pure.psb" \
  --model "Chocola=/path/to/chocola-maid.pure.psb" \
  --model "Coconut=/path/to/coconut-maid.pure.psb" \
  --model "Vanilla=/path/to/vanilla-maid.pure.psb"
# -> target/emote-demo/index.html
```

The page embeds expression stills, a 30 fps alpha-WebM idle loop (authored
421-tick timeline) and a one-shot gesture clip (`にっこり`). The `<video>`
elements carry `autoplay muted loop playsinline`, and clip switches cross-fade.
Game assets remain the property of their rights holders and are not
redistributed.

## Verified data

- Previously audited YU-RIS ERIS sample `emote_test.psb` (PSB v2, per-game body
  key): 2 `RGBA4444` atlas pages, 6 objects, 21 motions, 234 layers, 888
  keyframes, 24 variables, 11 timelines. The sample is not present in the
  current workspace; the v4 models below are the current end-to-end fixtures.
- Four Nekopara maid models (PSB v4 `spec: ems`): one 4096×4096 `RGBA8` atlas,
  36 draw items / 28,512 triangles at the base motion, 115 timelines, 86
  variables.

## Evidence-backed facts

Facts recovered from the official SDK 0.495 binaries and the sample:

- **Texture order.** `RGBA4444` uses `D3DFMT_A4R4G4B4` (alpha bits 15..12),
  expanded with `n*17` (`sub_10054BA0`). 32-bpp `RGBA8` pages depend on the
  authored `spec`: `ems`/`common` store RGBA bytes, Windows DIBs store BGRA.
- **Frame colors.** Corner colors are `0xRRGGBBAA`; layer opacity multiplies
  only the alpha byte. MODULATE2X frames use `0x80` RGB as neutral.
- **Blend table.** `sub_1041A190` maps modes 0..5 to the D3D9 factors
  `[INVSRCALPHA, ONE, ONE, INVSRCALPHA, ONE, ONE]` (dest) and
  `[SRCALPHA, SRCALPHA, SRCALPHA, DESTCOLOR, INVDESTCOLOR, SRCALPHA]` (src),
  with REVSUBTRACT for modes 2/5. The high nibble `0x10` selects MODULATE2X.
- **Timeline command streams.** A variable frame writes at
  `next.time - time - 1` ticks; type-0 frames are cursor-only; `pow(progress,
  exponent)` comes from the native easing table; loops advance to `loopEnd`
  non-inclusively, seek `loopBegin`, then consume the remainder.
- **Draw order.** `DrawFrameInfo` is sorted by resolved Z, not authored
  priority. Rear hair sits behind the body; front hair and eyes are in front.
- **Stencil chain.** Each color item walks `stencil_parent_native_key`; inner
  owners (phase 1) contribute a union-of-sources coverage buffer, outer owners
  (phase 2) clear/initialize the reference. The CPU path implements the inner
  union and reports outer phases as skipped.
- **YSEmote ABI.** Exports are stdcall (`RET imm`); ordinal 254 calls player
  `+0xd4` (`Update(seconds)`), ordinal 255 calls `+0xd8`.

The exact function addresses and remaining gaps are recorded in the kernel
repository's `docs/emote-parity.md`.

## Design boundaries

- **No fake semantics.** Unsupported renderer features are skipped/counted;
  unknown `EMOTEINFO` fields return an explicit error.
- **YU-RIS glue is separate** from the runtime: task suspension, capability and
  dirty-frame tracking live in `src/yuris.rs`.
- **Large stack.** eluna scene evaluation is recursive; debug binaries and test
  threads need a large stack. `.cargo/config.toml` sets a 32 MiB stack for the
  Windows MSVC target and `RUST_MIN_STACK` for tests.

## 参考实现与许可 / References and licensing

This project is **not clean-room**:

- [`eluna`](https://github.com/xmoezzz/eluna) — vendored at commit
  `9a000293968392daa34c83f82b991f7ba25ccc29`. The `eluna_rs` crate is
  **MPL-2.0** and is kept in `vendor/eluna` with its license and provenance.
  It is the sole animation runtime used here.
- The reference player `eluna_player` (MIT OR Apache-2.0) supplied the
  vertex/blend/rasterization math adapted in `src/plan.rs` and `src/soft.rs`.
- [`FreeMote`](https://github.com/UlyssesWu/FreeMote) (CC BY-NC-SA 4.0) —
  format facts and visual oracle only; no code incorporated.
- [`Chocola-X/NekoWebShow`](https://github.com/Chocola-X/NekoWebShow) (AGPL-3.0)
  and [`reAAAq/KrKr2-Next`](https://github.com/reAAAq/KrKr2-Next) (GPL-3.0) were
  inspected as behavioral references only; no code was copied.

`yu-emote` itself is **MIT OR Apache-2.0**. The vendored `eluna` files remain
MPL-2.0. See `NOTICE`, `LICENSE-MPL-2.0` and `vendor/eluna/PROVENANCE.md`.

The official `YSEmote.dll` / `emotedriver.dll` are used only as an
interoperability oracle; no official code or shader source is copied.

## Related

- YU-RIS kernel: <https://github.com/NotFaceGUI/yuris-kernel>
- E-mote audit/evidence ledger (in the kernel repo): `docs/emote-parity.md`
- Task book: `docs/tasks/task-emote-runtime.md`
