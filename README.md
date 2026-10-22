# yu-emote

Clean-room E-mote (EMT/PSB) runtime for YU-RIS, written in Rust and designed for
native + `wasm32` use.

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
| Deterministic variable/timeline state (`Animator`: PLAY/DPLAY/STOP, fades, looping) | ✅ M2 core (differential fade curve pending) |
| Layer/frame evaluation, nested motions, motion-local parameter tables, motion-level `parameterize`, `priority` emission, resolved-Z draw order (`psb-pose`) | ✅ M2 core (parameterized deformer-mesh inheritance pending) |
| Render plan (`psb-plan`) + deterministic CPU rasterizer (`psb-render`, PNG) | ✅ M3 baseline |
| Composite alpha masks (`stencilType & 4` + `stencilCompositeMaskLayerList`) in the CPU path | ✅ M3 core |
| WGSL for all 58 official shader variants in [`shaders/`](shaders/) + cubic Bezier mesh tessellation | ✅ M3 core (wgpu pipeline pending) |
| Pixel parity vs. official `YSEmote.dll` | ⏳ M4 |

The renderer is usable today for real game data: the YU-RIS test model and
four Nekopara maid models produce complete characters with correct
rear-hair/face/front-hair layering and variable-driven expressions.
[`demo/make_demo.py`](demo/make_demo.py) builds a self-contained character +
expression + idle-loop demo page (`target/emote-demo/index.html`) from those
models. Every pose and animation frame uses one fixed camera, so tilting or
breathing never changes the character's apparent size.

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
  named source layers (resolved in the same motion scope). The CPU renderer
  rasterizes those sources into a coverage buffer; `add_mask` with opacity 0 is
  not a source (matching the native visible-sprite filter).
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

## Attribution and licensing

Dual-licensed under **MIT OR Apache-2.0**.

- PSB/MDF binary parsing: [`emote-psb`](https://github.com/storycraft/emote-psb-rs)
  by storycraft, MIT.
- PSB body stream cipher schedule and single-DWORD key search: documented by
  FreeMote and eluna; this repository contains an independent implementation
  (no code copied).
- Model normalization / part-table / wasm ABI concepts:
  [`amadeus-emote-loader`](https://github.com/Mieluoxxx/amadeus-emote-loader),
  Apache-2.0 (reference; no code copied in this milestone).
- Behavior/oracle references only (no code): [`eluna`](https://github.com/xmoezzz/eluna)
  (no license), [`FreeMote`](https://github.com/UlyssesWu/FreeMote) and its WebGL
  runtime (CC BY-NC-SA 4.0).

See `NOTICE` for details.

## Related

- YU-RIS kernel: <https://github.com/NotFaceGUI/yuris-kernel>
- E-mote audit/evidence ledger (in the kernel repo): `docs/emote-parity.md`
- Task book: `docs/tasks/task-emote-runtime.md`
