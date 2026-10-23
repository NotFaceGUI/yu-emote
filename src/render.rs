//! Render-side contracts for M3: WGSL variants and mesh generation.
//!
//! ## Shader evidence
//!
//! The official `emotedriver.dll` embeds 58 unique D3D9 programs. They were
//! extracted from the pointer table at `0x100866c0..0x100867ac` and
//! disassembled with `fxc /dumpbin`:
//!
//! | program | count | uniforms |
//! |---|---:|---|
//! | `ps_2_0` | 27 | 13 x `cTex`; 13 x `cTex + cAlphaMaskTex`; 1 x `cTex + cPixelateTex` |
//! | `ps_3_0` | 27 | same split, masked/pixelate add `float2 cScMul` |
//! | `vs_2_0` | 3 | `cMat`; `cMat + cScOffset` (two projection forms) |
//! | `vs_3_0` | 1 | `cMat` |
//!
//! The 27 pixel programs are not arbitrary: they are combinations of
//! texture/vertex-color sourcing, a 1x/2x scale, a `1 - c` / `1 - 2c` invert,
//! RGB premultiplication, and a mask channel read from `cAlphaMaskTex.g`.
//! [`PIXEL_PROGRAMS`] records those features in the same order as the official
//! pointer table. The committed WGSL sources live in the repository's
//! `shaders/` directory; [`pixel_wgsl`] and [`vertex_wgsl`] return those files.
//! [`generated_pixel_wgsl`] / [`generated_vertex_wgsl`] are the canonical
//! generator used by `cargo run --bin gen-shaders` and by the parity test.
//!
//! `ps_3_0` masked and pixelate programs do not use the vertex-provided
//! screen UV: they compute `(vPos + 0.5) * cScMul` in the fragment shader.
//! The WGSL therefore reads `@builtin(position)` and applies `cScMul`.
//!
//! ## Mesh evidence status
//!
//! Official content stores a 4x4 control grid (`mesh.bp`, 32 floats). The
//! native evaluator has not been reduced to a self-contained formula yet.
//! Public tooling documents it as a cubic Bezier patch with the identity grid
//! `(col/3, row/3)`; [`MeshPatch`] implements that basis so geometry can be
//! generated and tested, but it is marked **unverified** until M4 pixel parity
//! confirms it. `meshDivision` controls the tessellation density per axis.

/// Number of official pixel programs per shader model.
pub const PIXEL_PROGRAMS_PER_MODEL: usize = 27;

/// Number of official vertex programs.
pub const VERTEX_PROGRAMS: usize = 4;

/// D3D9 shader model the program came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderModel {
    /// Pixel shader 2.0.
    Ps2,
    /// Pixel shader 3.0 (fragment-space mask UV and `cScMul`).
    Ps3,
}

impl ShaderModel {
    /// Directory name used by `shaders/`.
    pub const fn directory(self) -> &'static str {
        match self {
            Self::Ps2 => "ps_2_0",
            Self::Ps3 => "ps_3_0",
        }
    }
}

/// How the fragment shader derives RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RgbSource {
    /// `texture.rgb * vertex_color.rgb`.
    TextureAndVertex,
    /// `vertex_color.rgb` only (texture contributes alpha only).
    Vertex,
}

/// RGB inversion applied after scaling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invert {
    /// No inversion.
    None,
    /// `rgb = 1 - rgb`.
    OneMinus,
    /// `rgb = 1 - 2 * rgb` (input was already scaled by 2).
    OneMinusScaled,
}

/// One pixel program's feature set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PixelProgram {
    /// Index in the official 27-entry table.
    pub index: usize,
    /// Uses `cAlphaMaskTex` and multiplies alpha by its green channel.
    pub masked: bool,
    /// Uses `cPixelateTex` (screen-space alpha modifier).
    pub pixelate: bool,
    /// RGB source.
    pub rgb: RgbSource,
    /// RGB scale (1 or 2).
    pub scale: f32,
    /// RGB inversion.
    pub invert: Invert,
    /// `rgb *= alpha` after alpha is known.
    pub premultiply: bool,
    /// Alpha multiplies `cTex.a`.
    pub alpha_texture: bool,
    /// Alpha multiplies `vertex_color.a`.
    pub alpha_vertex: bool,
    /// Copies the final alpha into `g` (mask render target program).
    pub green_from_alpha: bool,
    /// Multiplies `g` by the mask channel (mask render target program).
    pub green_masked: bool,
}

const fn plain(
    index: usize,
    rgb: RgbSource,
    scale: f32,
    invert: Invert,
    premultiply: bool,
) -> PixelProgram {
    PixelProgram {
        index,
        masked: false,
        pixelate: false,
        rgb,
        scale,
        invert,
        premultiply,
        alpha_texture: true,
        alpha_vertex: true,
        green_from_alpha: false,
        green_masked: false,
    }
}

const fn masked(
    index: usize,
    rgb: RgbSource,
    scale: f32,
    invert: Invert,
    premultiply: bool,
) -> PixelProgram {
    PixelProgram {
        index,
        masked: true,
        pixelate: false,
        rgb,
        scale,
        invert,
        premultiply,
        alpha_texture: true,
        alpha_vertex: true,
        green_from_alpha: false,
        green_masked: false,
    }
}

/// The 27 official pixel programs, in pointer-table order.
///
/// Plain: indices 0,1,2,6,7,8,12,13,14,18,19,20,24.
/// Masked: 3,4,5,9,10,11,15,16,17,21,22,23,25.
/// Pixelate: 26.
pub const PIXEL_PROGRAMS: [PixelProgram; PIXEL_PROGRAMS_PER_MODEL] = [
    plain(0, RgbSource::TextureAndVertex, 1.0, Invert::None, false),
    plain(1, RgbSource::TextureAndVertex, 1.0, Invert::OneMinus, false),
    plain(2, RgbSource::TextureAndVertex, 1.0, Invert::None, true),
    masked(3, RgbSource::TextureAndVertex, 1.0, Invert::None, false),
    masked(4, RgbSource::TextureAndVertex, 1.0, Invert::OneMinus, false),
    masked(5, RgbSource::TextureAndVertex, 1.0, Invert::None, true),
    plain(6, RgbSource::TextureAndVertex, 2.0, Invert::None, false),
    plain(
        7,
        RgbSource::TextureAndVertex,
        2.0,
        Invert::OneMinusScaled,
        false,
    ),
    plain(8, RgbSource::TextureAndVertex, 2.0, Invert::None, true),
    masked(9, RgbSource::TextureAndVertex, 2.0, Invert::None, false),
    masked(
        10,
        RgbSource::TextureAndVertex,
        2.0,
        Invert::OneMinusScaled,
        false,
    ),
    masked(11, RgbSource::TextureAndVertex, 2.0, Invert::None, true),
    plain(12, RgbSource::Vertex, 1.0, Invert::None, false),
    plain(13, RgbSource::Vertex, 1.0, Invert::OneMinus, false),
    plain(14, RgbSource::Vertex, 1.0, Invert::None, true),
    masked(15, RgbSource::Vertex, 1.0, Invert::None, false),
    masked(16, RgbSource::Vertex, 1.0, Invert::OneMinus, false),
    masked(17, RgbSource::Vertex, 1.0, Invert::None, true),
    plain(18, RgbSource::Vertex, 2.0, Invert::None, false),
    plain(19, RgbSource::Vertex, 2.0, Invert::OneMinusScaled, false),
    plain(20, RgbSource::Vertex, 2.0, Invert::None, true),
    masked(21, RgbSource::Vertex, 2.0, Invert::None, false),
    masked(22, RgbSource::Vertex, 2.0, Invert::OneMinusScaled, false),
    masked(23, RgbSource::Vertex, 2.0, Invert::None, true),
    PixelProgram {
        index: 24,
        masked: false,
        pixelate: false,
        rgb: RgbSource::TextureAndVertex,
        scale: 1.0,
        invert: Invert::None,
        premultiply: false,
        alpha_texture: true,
        alpha_vertex: false,
        green_from_alpha: true,
        green_masked: false,
    },
    PixelProgram {
        index: 25,
        masked: true,
        pixelate: false,
        rgb: RgbSource::TextureAndVertex,
        scale: 1.0,
        invert: Invert::None,
        premultiply: false,
        alpha_texture: true,
        alpha_vertex: false,
        green_from_alpha: false,
        green_masked: true,
    },
    PixelProgram {
        index: 26,
        masked: false,
        pixelate: true,
        rgb: RgbSource::TextureAndVertex,
        scale: 1.0,
        invert: Invert::None,
        premultiply: false,
        alpha_texture: true,
        alpha_vertex: false,
        green_from_alpha: false,
        green_masked: false,
    },
];

/// Vertex programs, in official pointer-table order (`vs_2_0[0..3]`,
/// `vs_3_0[0]`). `vs_3_0` is equivalent to `vs_2_0` variant 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexProgram {
    /// `cMat` only, passthrough color/uv.
    Basic,
    /// `cMat`, screen-space UV (`OT1`) from NDC and `cScOffset`.
    ScreenOffset,
    /// `cMat`, screen-space UV with an additional scale.
    ScreenOffsetScaled,
}

/// The four official vertex programs.
pub const VERTEX_PROGRAM_LIST: [VertexProgram; VERTEX_PROGRAMS] = [
    VertexProgram::Basic,
    VertexProgram::ScreenOffset,
    VertexProgram::ScreenOffsetScaled,
    VertexProgram::Basic,
];

/// WGSL common prelude.
const PRELUDE: &str = r#"struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) mask_uv: vec2<f32>,
};

struct FragOut {
    @location(0) color: vec4<f32>,
};

@group(0) @binding(0) var cTex: texture_2d<f32>;
@group(0) @binding(1) var cSampler: sampler;
"#;

fn pixel_header(model: ShaderModel, program: &PixelProgram) -> String {
    let group = if program.pixelate {
        "pixelate"
    } else if program.masked {
        "alpha-mask"
    } else {
        "plain"
    };
    format!(
        "// E-mote official {}[{}] - {} program.\n\
         // Uniforms: cTex{}{}.\n\
         // Generated by `cargo run --bin gen-shaders`; edit the generator, not this file.\n",
        model.directory(),
        program.index,
        group,
        if program.masked {
            ", cAlphaMaskTex"
        } else {
            ""
        },
        if program.pixelate {
            ", cPixelateTex"
        } else {
            ""
        },
    )
}

/// Generate the WGSL for one official pixel program.
///
/// This is the canonical generator; the committed files under `shaders/` are
/// produced from it and checked by `shader_files_match_generator`.
pub fn generated_pixel_wgsl(model: ShaderModel, program: &PixelProgram) -> String {
    let mut source = pixel_header(model, program);
    source.push_str(PRELUDE);
    if program.masked {
        source.push_str("@group(0) @binding(2) var cAlphaMaskTex: texture_2d<f32>;\n");
    }
    if program.pixelate {
        source.push_str("@group(0) @binding(2) var cPixelateTex: texture_2d<f32>;\n");
    }
    if model == ShaderModel::Ps3 && (program.masked || program.pixelate) {
        source.push_str("@group(0) @binding(3) var<uniform> cScMul: vec2<f32>;\n");
    }
    source.push_str("\n@fragment\nfn fs_main(in: VsOut) -> FragOut {\n");
    source.push_str("    let tex = textureSample(cTex, cSampler, in.uv);\n");

    if program.pixelate {
        match model {
            ShaderModel::Ps2 => {
                source.push_str("    let uv = in.mask_uv;\n");
            }
            ShaderModel::Ps3 => {
                source.push_str("    let uv = (in.position.xy + vec2<f32>(0.5)) * cScMul;\n");
            }
        }
        source.push_str("    let pix = textureSample(cPixelateTex, cSampler, uv);\n");
        source.push_str("    return FragOut(vec4<f32>(pix.rgb, tex.a * pix.a));\n}\n");
        return source;
    }

    let mut rgb = match program.rgb {
        RgbSource::TextureAndVertex => "tex.rgb * in.color.rgb".to_string(),
        RgbSource::Vertex => "in.color.rgb".to_string(),
    };
    if program.scale != 1.0 {
        rgb = format!("({rgb}) * {:.1}", program.scale);
    }
    rgb = match program.invert {
        Invert::None => rgb,
        Invert::OneMinus | Invert::OneMinusScaled => {
            format!("vec3<f32>(1.0) - ({rgb})")
        }
    };
    source.push_str(&format!("    var rgb = {rgb};\n"));
    source.push_str("    var alpha = 1.0;\n");
    if program.alpha_texture {
        source.push_str("    alpha *= tex.a;\n");
    }
    if program.alpha_vertex {
        source.push_str("    alpha *= in.color.a;\n");
    }
    if program.masked {
        let mask_uv = match model {
            ShaderModel::Ps2 => "in.mask_uv",
            ShaderModel::Ps3 => "(in.position.xy + vec2<f32>(0.5)) * cScMul",
        };
        source.push_str(&format!(
            "    let mask = textureSample(cAlphaMaskTex, cSampler, {mask_uv});\n\
             \x20   alpha *= mask.g;\n"
        ));
    }
    if program.premultiply {
        source.push_str("    rgb *= alpha;\n");
    }
    if program.green_from_alpha {
        source.push_str("    rgb.g = alpha;\n");
    }
    if program.green_masked {
        source.push_str("    rgb.g *= mask.g;\n");
    }
    source.push_str("    return FragOut(vec4<f32>(rgb, alpha));\n}\n");
    source
}

fn vertex_header(name: &str, program: VertexProgram) -> String {
    let kind = match program {
        VertexProgram::Basic => "basic (cMat)",
        VertexProgram::ScreenOffset => "screen UV (cMat + cScOffset)",
        VertexProgram::ScreenOffsetScaled => "screen UV scaled (cMat + cScOffset)",
    };
    format!(
        "// E-mote official {name} - {kind}.\n\
         // Generated by `cargo run --bin gen-shaders`; edit the generator, not this file.\n"
    )
}

/// Generate the WGSL for one official vertex program.
///
/// `name` is only used in the header (`vs_2_0` or `vs_3_0`).
pub fn generated_vertex_wgsl(name: &str, program: VertexProgram) -> String {
    let mut source = vertex_header(name, program);
    source.push_str(
        r#"struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) uv: vec2<f32>,
};

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) mask_uv: vec2<f32>,
};

@group(0) @binding(0) var<uniform> cMat: mat4x4<f32>;
"#,
    );
    if program != VertexProgram::Basic {
        source.push_str("@group(0) @binding(1) var<uniform> cScOffset: vec4<f32>;\n");
    }
    source.push_str(
        r#"
@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    let clip = cMat * vec4<f32>(in.position, 1.0);
    out.position = clip;
    out.color = in.color;
    out.uv = in.uv;
"#,
    );
    match program {
        VertexProgram::Basic => {
            source.push_str("    out.mask_uv = vec2<f32>(0.0);\n");
        }
        VertexProgram::ScreenOffset => {
            source.push_str(
                r#"    let ndc = clip.xy / clip.w;
    out.mask_uv = ndc * vec2<f32>(0.5, -0.5) + cScOffset.xy;
"#,
            );
        }
        VertexProgram::ScreenOffsetScaled => {
            source.push_str(
                r#"    let ndc = clip.xy / clip.w;
    out.mask_uv = ndc * vec2<f32>(0.5, -0.5) * cScOffset.wz + cScOffset.xy;
"#,
            );
        }
    }
    source.push_str("    return out;\n}\n");
    source
}

// The committed shader files are included at compile time by `shader_index.rs`
// (generated by `build.rs`). Keeping them as files makes the WGSL reviewable
// without running the generator while staying wasm-friendly (no runtime I/O).
include!(concat!(env!("OUT_DIR"), "/shader_index.rs"));

/// WGSL source for one official pixel program.
///
/// Returns `None` when `index` is outside `0..27`.
pub fn pixel_wgsl(model: ShaderModel, index: usize) -> Option<&'static str> {
    pixel_shader_source(model, index)
}

/// WGSL source for one of the three distinct vertex programs.
///
/// `vs_3_0` uses the same basic program as `vs_2_0`; the duplicate file in
/// `shaders/vs_3_0/` documents the official table.
pub fn vertex_wgsl(program: VertexProgram) -> &'static str {
    vertex_shader_source(program)
}

/// A 4x4 cubic Bezier patch in normalized local sprite space.
///
/// `identity` uses `(col/3, row/3)`, matching the neutral mesh.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct MeshPatch {
    /// Control points, row-major.
    pub control_points: [[f32; 2]; 16],
    /// Tessellation density carried by the patch producer.
    ///
    /// A parent proxy can tessellate a much finer grid than the textured
    /// sprite that inherits the patch; `combined_with` keeps the maximum.
    pub division: u32,
    /// Optional mesh-sync domain `[left, top, width, height]` in local pixels.
    ///
    /// Frames whose `icon` is `width:height:originX:originY` define this from
    /// the icon box; it is used only by `meshSyncChildMask` coordinate/angle/
    /// scale sync, not by sprite tessellation.
    pub domain: Option<[f32; 4]>,
}

impl MeshPatch {
    /// Neutral patch covering the unit square.
    pub fn identity() -> Self {
        let mut control_points = [[0.0; 2]; 16];
        for row in 0..4 {
            for col in 0..4 {
                control_points[row * 4 + col] = [col as f32 / 3.0, row as f32 / 3.0];
            }
        }
        Self {
            control_points,
            division: 1,
            domain: None,
        }
    }

    /// Build a patch from the flat `mesh.bp` list (32 floats).
    ///
    /// `None` or a short list yields the identity patch; this mirrors the
    /// neutral frame behavior (`mesh.bp = null`).
    pub fn from_bp(values: Option<&[f64]>) -> Self {
        let Some(values) = values else {
            return Self::identity();
        };
        if values.len() < 32 {
            return Self::identity();
        }
        let mut patch = Self::identity();
        for (index, point) in patch.control_points.iter_mut().enumerate() {
            *point = [values[index * 2] as f32, values[index * 2 + 1] as f32];
        }
        patch
    }

    /// Set the tessellation density used when this patch deforms a sprite.
    pub fn with_division(mut self, division: u32) -> Self {
        self.division = self.division.max(division.max(1));
        self
    }

    /// Warp one point through the patch in its authored domain.
    ///
    /// The input and output are in the owning layer's local sprite pixels;
    /// points outside `[0,1]` are evaluated without clamping, matching native
    /// `meshSyncChild` finite differences at patch edges.
    pub fn warp_local(&self, point: [f32; 2]) -> Option<[f32; 2]> {
        let [left, top, width, height] = self.domain?;
        if !width.is_finite()
            || !height.is_finite()
            || width.abs() <= f32::EPSILON
            || height.abs() <= f32::EPSILON
        {
            return None;
        }
        let u = (point[0] - left) / width;
        let v = (point[1] - top) / height;
        let mapped = self.sample(u, v);
        Some([left + mapped[0] * width, top + mapped[1] * height])
    }

    /// Attach the mesh-sync domain when the patch does not carry one.
    pub fn with_domain(mut self, domain: Option<[f32; 4]>) -> Self {
        if self.domain.is_none() {
            self.domain = domain;
        }
        self
    }

    /// Combine an inherited patch with the current layer's patch.
    ///
    /// The native `meshCombine` path (`sub_1038B340`) does not compose the
    /// two Bezier mappings. It adds each source control-point displacement
    /// from the canonical identity mesh to the destination point-by-point:
    ///
    /// ```text
    /// out[i] = next[i] + (self[i] - identity[i])
    /// ```
    ///
    /// `self` is the accumulated ancestor patch and `next` is the layer's own
    /// patch (eluna's `EmoteMeshPatch::combined_with`, evidenced against the
    /// native function). All patches are normalized to the same sprite box,
    /// so the displacement addition is well defined.
    pub fn combined_with(&self, next: &MeshPatch) -> MeshPatch {
        let identity = MeshPatch::identity();
        let mut out = *next;
        out.division = self.division.max(next.division);
        out.domain = self.domain.or(next.domain);
        for index in 0..16 {
            out.control_points[index][0] +=
                self.control_points[index][0] - identity.control_points[index][0];
            out.control_points[index][1] +=
                self.control_points[index][1] - identity.control_points[index][1];
        }
        out
    }

    /// Interpolate two patches control-point by control-point.
    pub fn interpolate(a: &MeshPatch, b: &MeshPatch, t: f32) -> MeshPatch {
        let t = t.clamp(0.0, 1.0);
        let mut control_points = a.control_points;
        for (index, point) in control_points.iter_mut().enumerate() {
            point[0] = a.control_points[index][0]
                + (b.control_points[index][0] - a.control_points[index][0]) * t;
            point[1] = a.control_points[index][1]
                + (b.control_points[index][1] - a.control_points[index][1]) * t;
        }
        MeshPatch {
            control_points,
            division: a.division.max(b.division),
            domain: a.domain.or(b.domain),
        }
    }

    /// Whether the patch is the neutral identity mapping within `epsilon`.
    pub fn is_identity(&self, epsilon: f32) -> bool {
        let identity = MeshPatch::identity();
        self.control_points
            .iter()
            .zip(identity.control_points.iter())
            .all(|(point, neutral)| {
                (point[0] - neutral[0]).abs() <= epsilon && (point[1] - neutral[1]).abs() <= epsilon
            })
    }

    /// Evaluate the bicubic Bezier patch at `(u, v)`.
    pub fn sample(&self, u: f32, v: f32) -> [f32; 2] {
        let bu = basis(u);
        let bv = basis(v);
        let mut out = [0.0f32; 2];
        for (row, bv_row) in bv.iter().enumerate() {
            for (col, bu_col) in bu.iter().enumerate() {
                let weight = bv_row * bu_col;
                let point = self.control_points[row * 4 + col];
                out[0] += point[0] * weight;
                out[1] += point[1] * weight;
            }
        }
        out
    }

    /// Scale the normalized patch into a pixel-space rectangle.
    pub fn to_rect(&self, u: f32, v: f32, width: f32, height: f32) -> [f32; 2] {
        let point = self.sample(u, v);
        [point[0] * width, point[1] * height]
    }
}

fn basis(t: f32) -> [f32; 4] {
    let s = 1.0 - t;
    [s * s * s, 3.0 * t * s * s, 3.0 * t * t * s, t * t * t]
}

/// One tessellated mesh vertex.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeshVertex {
    /// Local X in pixels.
    pub x: f32,
    /// Local Y in pixels.
    pub y: f32,
    /// Normalized U.
    pub u: f32,
    /// Normalized V.
    pub v: f32,
}

/// A tessellated quad: vertices plus triangle indices.
#[derive(Debug, Clone, PartialEq)]
pub struct MeshGrid {
    /// `(division_x+1) * (division_y+1)` vertices.
    pub vertices: Vec<MeshVertex>,
    /// Two triangles per cell.
    pub indices: Vec<u32>,
    /// Number of cells on X.
    pub division_x: u32,
    /// Number of cells on Y.
    pub division_y: u32,
}

impl MeshGrid {
    /// Tessellate a patch over a pixel-space rectangle.
    ///
    /// `division` is `meshDivision` from the layer (observed 20). Zero is
    /// treated as 1 because a mesh with no cells cannot draw.
    pub fn tessellate(patch: &MeshPatch, width: f32, height: f32, division: u32) -> Self {
        let division = division.max(1);
        let mut vertices = Vec::with_capacity(((division + 1) * (division + 1)) as usize);
        for row in 0..=division {
            let v = row as f32 / division as f32;
            for col in 0..=division {
                let u = col as f32 / division as f32;
                let [x, y] = patch.to_rect(u, v, width, height);
                vertices.push(MeshVertex { x, y, u, v });
            }
        }
        let stride = division + 1;
        let mut indices = Vec::with_capacity((division * division * 6) as usize);
        for row in 0..division {
            for col in 0..division {
                let i0 = row * stride + col;
                let i1 = i0 + 1;
                let i2 = i0 + stride;
                let i3 = i2 + 1;
                indices.extend_from_slice(&[i0, i1, i2, i1, i3, i2]);
            }
        }
        Self {
            vertices,
            indices,
            division_x: division,
            division_y: division,
        }
    }
}

/// Marker for facts that still need official pixel parity.
pub const MESH_BASIS_EVIDENCE: &str =
    "cubic Bezier documented by public tooling; pending official parity";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_variant_counts() {
        assert_eq!(PIXEL_PROGRAMS.len(), 27);
        assert_eq!(PIXEL_PROGRAMS.iter().filter(|p| p.masked).count(), 13);
        assert_eq!(PIXEL_PROGRAMS.iter().filter(|p| p.pixelate).count(), 1);
        assert_eq!(
            PIXEL_PROGRAMS
                .iter()
                .filter(|p| !p.masked && !p.pixelate)
                .count(),
            13
        );
        assert_eq!(VERTEX_PROGRAM_LIST.len(), 4);
    }

    #[test]
    fn mesh_patch_combines_by_displacement() {
        let identity = MeshPatch::identity();
        assert!(identity.is_identity(0.0));
        let mut patch = MeshPatch::identity();
        patch.control_points[5] = [0.5, 0.1];
        let combined = identity.combined_with(&patch);
        assert_eq!(combined.control_points[5], [0.5, 0.1]);
        // Combining a patch with itself accumulates its displacement twice.
        let doubled = patch.combined_with(&patch);
        assert!((doubled.control_points[5][0] - 2.0 / 3.0).abs() < 1e-6);
        assert!((doubled.control_points[5][1] - (0.2 - 1.0 / 3.0)).abs() < 1e-6);
    }

    #[test]
    fn wgsl_contains_mask_only_when_needed() {
        assert!(
            !pixel_wgsl(ShaderModel::Ps2, 0)
                .unwrap()
                .contains("cAlphaMaskTex")
        );
        assert!(
            pixel_wgsl(ShaderModel::Ps2, 3)
                .unwrap()
                .contains("cAlphaMaskTex")
        );
        assert!(
            pixel_wgsl(ShaderModel::Ps2, 26)
                .unwrap()
                .contains("cPixelateTex")
        );
        assert!(pixel_wgsl(ShaderModel::Ps3, 26).unwrap().contains("cScMul"));
    }

    #[test]
    fn all_official_shader_files_are_present() {
        for model in [ShaderModel::Ps2, ShaderModel::Ps3] {
            for index in 0..PIXEL_PROGRAMS_PER_MODEL {
                assert!(
                    pixel_wgsl(model, index).is_some(),
                    "missing {}[{index}]",
                    model.directory()
                );
            }
        }
        assert!(pixel_wgsl(ShaderModel::Ps2, 27).is_none());
        for program in VERTEX_PROGRAM_LIST {
            assert!(!vertex_wgsl(program).is_empty());
        }
    }

    #[test]
    fn shader_files_match_generator() {
        for model in [ShaderModel::Ps2, ShaderModel::Ps3] {
            for program in PIXEL_PROGRAMS {
                let file = pixel_wgsl(model, program.index).unwrap();
                assert_eq!(
                    file,
                    generated_pixel_wgsl(model, &program),
                    "{}[{}]",
                    model.directory(),
                    program.index
                );
            }
        }
        assert_eq!(
            vertex_wgsl(VertexProgram::Basic),
            generated_vertex_wgsl("vs_2_0", VertexProgram::Basic)
        );
        assert_eq!(
            vertex_wgsl(VertexProgram::ScreenOffset),
            generated_vertex_wgsl("vs_2_0", VertexProgram::ScreenOffset)
        );
        assert_eq!(
            vertex_wgsl(VertexProgram::ScreenOffsetScaled),
            generated_vertex_wgsl("vs_2_0", VertexProgram::ScreenOffsetScaled)
        );
    }

    #[test]
    fn identity_patch_matches_unit_square() {
        let patch = MeshPatch::identity();
        assert_eq!(patch.sample(0.0, 0.0), [0.0, 0.0]);
        assert_eq!(patch.sample(1.0, 0.0), [1.0, 0.0]);
        assert_eq!(patch.sample(0.0, 1.0), [0.0, 1.0]);
        assert_eq!(patch.sample(1.0, 1.0), [1.0, 1.0]);
        let center = patch.sample(0.5, 0.5);
        assert!((center[0] - 0.5).abs() < 1e-6);
        assert!((center[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn grid_has_expected_topology() {
        let grid = MeshGrid::tessellate(&MeshPatch::identity(), 100.0, 200.0, 2);
        assert_eq!(grid.vertices.len(), 9);
        assert_eq!(grid.indices.len(), 24);
        assert_eq!(grid.vertices[0].x, 0.0);
        assert_eq!(grid.vertices[8].x, 100.0);
        assert_eq!(grid.vertices[8].y, 200.0);
    }
}
