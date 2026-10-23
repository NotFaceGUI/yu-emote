# yu-emote

E-mote (EMT/PSB) runtime for YU-RIS, written in Rust and designed for native +
`wasm32` use. The runtime behavior is **referenced from the open-source E-mote
reimplementations listed below**, not developed as a clean-room design.

> E-mote is a 2D character-animation middleware by M2 Co., Ltd. This project is
> not affiliated with M2. It only reads/writes E-mote data files.

## Status

| Layer | State |
|---|---|
| PSB/MDF container parsing (via MIT `emote-psb`) | ✅ |
| Body decryption (E-mote key) + key search | ✅ (per-game key, not shipped) |
| PSB v4 `ems` sources (`source/tex`, one `RGBA8` page, 4-digit icons) | ✅ |
| Deterministic model summary (`psb-info`) | ✅ |
| YU-RIS adapter: `EMOTE` params, `EMOTEINFO` queries, capability, suspend/dirty | ✅ |
| Typed model schema: screen, atlas pages, objects/motions/layers/frames, variables, timelines, controllers (`psb-dump`) | ✅ M1 |
| Atlas decoding (`RGBA4444`, `A4R4G4B4` order evidenced from the DLL) | ✅ M1 |
| Deterministic timeline state (`Animator`: native 60 Hz command streams, transitions/easing exponents, authored loops, PLAY/DPLAY/STOP, differential fade) | ✅ M2 core |
| Layer/frame evaluation, top-level easing table (`ccc/acc/zcc/scc/occ/wcc`) and `cp` Bezier paths, nested motions, parameter tables/`parameterize`, resolved-Z draw order, native inheritance source, sprite-mesh sync + `meshCombinator`, empty-mesh identity handling, type-7 clip, type-9 anchor | ✅ M2 core |
| `Player`: timeline → controllers → pose → physics order; bust/hair/parts springs; clamp controls; deterministic eye blink | ✅ M2 core |
| Render plan (`psb-plan`) + deterministic CPU rasterizer (`psb-render`, PNG), native blend modes 0..5 + MODULATE2X, type-7 clipping | ✅ M3 baseline |
| Whole-timeline renderer (`psb-anim`) + 30 fps alpha-WebM demo clips | ✅ |
| Composite alpha masks (`stencilType & 4` + `stencilCompositeMaskLayerList`, type-3 source expansion) in the CPU path | ✅ M3 core |
| WGSL for all 58 official shader variants in [`shaders/`](shaders/) + cubic Bezier mesh tessellation | ✅ M3 core (wgpu pipeline pending) |
| Remaining M2/M3 gaps: type-4 particles, type-10 feedback, inner/outer stencil phases, eyebrow/mouth graph queues, selectors, wind, pixelate, wgpu | ⏳ |
| Pixel parity vs. official `YSEmote.dll` | ⏳ M4 |

The renderer is usable today for real game data: the YU-RIS test model and
four Nekopara maid models produce complete characters with correct
rear-hair/face/front-hair layering and variable-driven expressions.
[`demo/make_demo.py`](demo/make_demo.py) builds a self-contained character +
expression + full idle-loop demo page (`target/emote-demo/index.html`) from
those models. The idle loop is rendered by `psb-anim` as a 30 fps VP9/WebM
clip over the authored 421-tick (7 s) timeline. Every pose and animation frame
uses one fixed camera, and expression switches cross-fade.

## Usage

```bash
# Plain PSB
cargo run --bin psb-info -- /path/to/model.psb

# Body-encrypted PSB (recover the per-game key first)
cargo run --release --bin psb-key-search -- /path/to/model.psb
cargo run --bin psb-info -- /path/to/model.psb --key 0x175058c9

# Deterministic typed dump (+ optional raw tree, resources, decoded textures)
cargo run --bin psb-dump -- /path/to/model.psb --key 0x175058c9
cargo run --bin psb-dump -- model.psb --key 0x175058c9 --raw --resources out --decode

# Pose / draw plan / software render
cargo run --bin psb-pose -- model.psb --key 0x175058c9 --time 0
cargo run --bin psb-plan -- model.psb --key 0x175058c9 --time 0
cargo run --bin psb-render -- model.psb --key 0x175058c9 --time 0 --out out.png

# Expression/pose variants (variables map through the motion-local parameter table)
cargo run --bin psb-render -- model.psb --key 0x175058c9 --time 0 \
  --var face_mouth=30 --var face_eyebrow=40 --out smile.png

# Self-contained demo page (character + expression selector)
cargo build --bins
python demo/make_demo.py \
  --model "ERIS テスト=/path/to/emote_test.psb" \
  --model "ショコラ=/path/to/chocola-maid.pure.psb" \
  --model "バニラ=/path/to/vanilla-maid.pure.psb"
# → target/emote-demo/index.html
```

Verified against the YU-RIS ERIS sample `emote_test.psb` (PSB v2, body key
`0x175058c9`): 2 texture pages (`tex#0` 2048x2048 / `tex#1` 512x256, both
`RGBA4444`), 6 objects, 21 motions, 234 layers, 888 layer keyframes, 24
variables, 11 timelines. The key is data-specific and is **not** hardcoded in
the library.

Also verified against four Nekopara maid models (`chocola`, `vanilla`,
`azuki`, `coconut`) mirrored by
[`Chocola-X/NekoWebShow`](https://github.com/Chocola-X/NekoWebShow): PSB **v4**
(`spec: ems`), one 4096x4096 `RGBA8` atlas under `source/tex`, 87 icons,
21 objects / 48 motions, 34 draw items / 25,472 triangles at the base motion.
The game assets are used locally for testing and are **not redistributed** in
this repository or its demo output.

## Evidence-backed facts

These come from the official SDK 0.495 binaries and the official sample; they
are recorded here so the implementation can be audited against them:

- **Key source.** `PEmotePlayer`'s constructor calls `strtol` on the decimal
  string `"391141577"` at `emotedriver.dll` VA `0x10071350` and uses the result
  as the fourth word of the PSB body cipher (`0x175058C9` for this build).
  Games may ship a different `emotedriver.dll`; the key is not a constant of
  the model format.
- **Texture order.** `RGBA4444` pixels use the `D3DFMT_A4R4G4B4` bit layout
  (alpha bits 15..12, red 11..8, green 7..4, blue 3..0), expanded with `n*17`.
  This matches the DLL's converter at `0x10054BA0` and reproduces the official
  atlas colors; the opposite nibble order swaps hair/skin colors.
- **Shader table.** `emotedriver.dll` holds 58 unique D3D9 programs in the
  pointer table at VA `0x100866c0..0x100867ac`: 27 `ps_2_0`, 27 `ps_3_0`,
  3 `vs_2_0`, 1 `vs_3_0`. The 27 pixel programs are 13 plain, 13 alpha-mask
  (`cAlphaMaskTex.g`), 1 pixelate (`cPixelateTex`); `ps_3_0` masked variants
  add `float2 cScMul`. [`src/render.rs`](src/render.rs) records each variant's
  feature set.
- **YSEmote dispatch.** `YSEmote.dll` exports ordinal 2 = `EmoteCreate`,
  ordinal 3 = `EmoteFilterTexture`; ordinals 101..121 call the device vtable
  slots `+0x00..+0x50`, ordinals 201..256 call the player vtable slots
  `+0x00..+0xdc`. The engine's loader stores them at `yu-ris.exe` `0x801384..`.
- **PSB v4 `ems` shape.** Nekopara models use `source: {"tex": {...}}` (not
  `tex#0`/`tex#1`), icons keyed by four-digit strings (`"0020"`), and
  `texture/type == "RGBA8"` with a single 4096² resource. The decoder accepts
  both the v2 `RGBA4444` multi-page layout and the v4 `RGBA8` single-page
  layout.
- **Contracts.** `EMOTE` resolves the character, then applies named string /
  flag / numeric variables; numeric fades are converted with `value * 0.06`
  before reaching the player. `EMOTEINFO` calls player slot 45. Details and
  remaining gaps are in the kernel's `docs/emote-parity.md`.
- **Draw order.** The native manager emits sprites in `priority` order, then
  sorts them by resolved Z (`LayerInfo +620`, copied to DrawFrameInfo +36)
  ascending; `priority` is only the deterministic tie-break. Using `priority`
  alone puts rear hair in front of the face. Verified with the sample:
  rear hair `z=-10`, body `0`, face `3`, front hair `4`, eyes `5..6`.
- **Color order.** Frame corner colors are `0xRRGGBBAA` (low byte alpha), not
  `0xAARRGGBB`; the eyelid layer relies on `0x000000FF` being opaque black.
- **Parameter tables are motion-local.** `parameterize` indexes the owning
  motion's `parameter` array, and a referenced motion can itself carry
  `parameterize` (eyes/mouth/brows/tears/cheeks). A missing variable has the
  neutral value `0`, which is then mapped through `rangeBegin..rangeEnd`.
- **Frame hold.** A type-0 terminal frame holds the last authored keyframe in
  the same list; nested eye/mouth motions use this to stay visible after a
  type-0 gap.
- **Composite masks.** A type-12 layer with `stencilType & 4` and a non-empty
  `stencilCompositeMaskLayerList` clips its descendants by the union of the
  named source layers (resolved in the owner's motion scope). A type-0 source
  contributes its visible geometry; a type-3 source expands to the sprites
  emitted by its nested player. The CPU renderer rasterizes those sources into
  a coverage buffer; `add_mask` with opacity 0 is not a source (matching the
  native visible-sprite filter).
- **Sprite meshes.** `mesh.bp` is a 4x4 Bezier patch normalized to the drawn
  sprite's icon box, not a world-space warp. A layer passes its accumulated
  patch to children only when `meshTransform == 1` and
  `meshSyncChildMask & 8` (shape sync); patches combine by pointwise
  displacement from identity (`dst = next + (self - identity)`). This drives
  the eyelid/iris, head, body, mouth and eyebrow deformers. The other sync
  bits warp child coordinates/angle scale through the patch domain, and
  `meshCombinator.combinatorList` interpolates `rawMeshList` resources by
  variable.
- **Native inheritance source.** A layer with `inheritMask & 0x400000` is
  transparent for inheritance; descendants use the first ancestor above it.
  Partial inheritance removes the motion-root channels, builds the relative
  matrix and reapplies the root matrix outside it (`sub_10331060`).
- **Blend modes.** The CPU path implements the native `sub_1041A190` table
  (modes 0..5) and D3DTOP_MODULATE2X; the neutral vertex color for MODULATE2X
  frames is `0x808080FF`, which doubles to white.
- **YSEmote ABI.** The exported wrappers are **stdcall** (`RET imm`); declaring
  them `__cdecl` corrupts the stack after a few updates. Ordinal 254 calls
  player vtable `+0xd4` (`Update(seconds)`), ordinal 255 calls `+0xd8`, which
  forwards to the device renderer at `[impl+8]`.

```rust
use yu_emote::model::ModelSummary;
use yu_emote::psb;

let document = psb::read_document("emote_test.psb")?;
let summary = ModelSummary::from_document(&document);
println!("{}", serde_json::to_string_pretty(&summary)?);
```

YU-RIS adapter:

```rust
use yu_emote::yuris::{Capability, EmoteArg, YurisEmote};

let mut emote = YurisEmote::new(Capability::DataOnly);
emote.apply_emote([("BUST", EmoteArg::Int(128)), ("SMOOTH", EmoteArg::Int(1))])?;
// SYSTEMINFO[EMOTE] must stay 0 until a renderer is ready.
assert_eq!(emote.capability(), Capability::DataOnly);
```

## Design boundaries

- **No fake semantics.** Unknown `EMOTE` parameters are recorded; unknown
  `EMOTEINFO` fields return an explicit error instead of a guessed value.
- **YU-RIS glue is separate** from the E-mote core: task suspension, capability
  reporting and dirty-frame tracking live in `src/yuris.rs`; the data layer is
  engine-neutral.
- **wasm-friendly.** The data layer is pure Rust with no OS dependencies. The
  renderer (M3) will use wgpu/WebGPU so the same code runs natively and on
  `wasm32-unknown-unknown`. The official Windows DLL cannot run in wasm and is
  only used as an oracle.

## 参考实现与许可 / References and licensing

This project is **not clean-room**. It deliberately follows the recovered
behavior and data interpretation of these open-source projects:

- [`eluna`](https://github.com/xmoezzz/eluna) (`eluna_rs`, **MPL-2.0**) —
  the main behavioral reference for the native runtime: timeline command
  streams, frame interpolation and easing, inheritance/mesh chains, composite
  stencils, clamp/eye/mouth controllers, physics equations and the type-7/anchor
  passes. The Rust code in this repository is written for this project, but its
  algorithms and constants are adapted from eluna, and the module headers say
  so. eluna is pinned at commit `9a00029` for the facts cited here.
- [`FreeMote`](https://github.com/UlyssesWu/FreeMote) and its WebGL runtime
  (**CC BY-NC-SA 4.0**) — PSB/atlas format facts and a visual oracle. No
  FreeMote code is incorporated; it is **not** used for commercial licensing.
- [`emote-psb`](https://github.com/storycraft/emote-psb-rs) by storycraft
  (**MIT**) — the PSB/MDF binary parser dependency.
- [`amadeus-emote-loader`](https://github.com/Mieluoxxx/amadeus-emote-loader)
  (**Apache-2.0**) — model normalization and wasm ABI concepts.

The project as a whole is distributed under **MIT OR Apache-2.0**. The eluna
project is MPL-2.0; if you redistribute a modified version of a module that
contains adapted eluna logic, keep the MPL-2.0 notice for that module and make
its source available. See `NOTICE` for per-project attribution and the full
list of referenced files.

The official `YSEmote.dll` / `emotedriver.dll` are used only as an
interoperability oracle; no official code or shader source is copied.

## Related

- YU-RIS kernel: <https://github.com/NotFaceGUI/yuris-kernel>
- E-mote audit/evidence ledger (in the kernel repo): `docs/emote-parity.md`
- Task book: `docs/tasks/task-emote-runtime.md`
