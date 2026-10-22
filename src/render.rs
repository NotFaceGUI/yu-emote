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
//! | `ps_2_0` | 27 | 13 × `cTex`; 13 × `cTex + cAlphaMaskTex`; 1 × `cTex + cPixelateTex` |
//! | `ps_3_0` | 27 | same split, masked group adds `float2 cScMul` |
//! | `vs_2_0` | 3 | `cMat`; `cMat + cScOffset` (two projection forms) |
//! | `vs_3_0` | 1 | `cMat` |
//!
//! The 27 pixel programs are not arbitrary: they are combinations of
//! texture/vertex-color sourcing, a 1×/2× scale, a `1 - c` / `1 - 2c` invert,
//! RGB premultiplication, and a mask channel read from `cAlphaMaskTex.g`.
//! [`PIXEL_PROGRAMS`] records those features in the same order as the official
//! pointer table; [`pixel_wgsl`] regenerates an equivalent fragment shader.
//!
//! ## Mesh evidence status
//!
//! Official content stores a 4×4 control grid (`mesh.bp`, 32 floats). The
//! native evaluator has not been reduced to a self-contained formula yet.
//! Public tooling documents it as a cubic Bezier patch with the identity grid
//! `(col/3, row/3)`; [`MeshPatch`] implements that basis so geometry can be
//! generated and tested, but it is marked **unverified** until M4 pixel parity
//! confirms it. `meshDivision` controls the tessellation density per axis.

/// Number of official pixel programs per shader model.
pub const PIXEL_PROGRAMS_PER_MODEL: usize = 27;

/// Number of official vertex programs.
pub const VERTEX_PROGRAMS: usize = 4;

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

/// Generate WGSL for one pixel program.
pub fn pixel_wgsl(program: &PixelProgram) -> String {
    let mut source = String::from(PRELUDE);
    if program.masked {
        source.push_str("@group(0) @binding(2) var cAlphaMaskTex: texture_2d<f32>;\n");
    }
    if program.pixelate {
        source.push_str("@group(0) @binding(2) var cPixelateTex: texture_2d<f32>;\n");
    }
    source.push_str(
        r#"
@fragment
fn fs_main(in: VsOut) -> FragOut {
    var tex = textureSample(cTex, cSampler, in.uv);
"#,
    );
    let mut rgb = match program.rgb {
        RgbSource::TextureAndVertex => "tex.rgb * in.color.rgb".to_string(),
        RgbSource::Vertex => "in.color.rgb".to_string(),
    };
    if program.scale != 1.0 {
        rgb = format!("({rgb}) * {:.1}", program.scale);
    }
    rgb = match program.invert {
        Invert::None => rgb,
        Invert::OneMinus => format!("vec3<f32>(1.0) - ({rgb})"),
        Invert::OneMinusScaled => format!("vec3<f32>(1.0) - ({rgb})"),
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
        source.push_str("    alpha *= textureSample(cAlphaMaskTex, cSampler, in.mask_uv).g;\n");
    }
    if program.pixelate {
        source.push_str("    alpha *= textureSample(cPixelateTex, cSampler, in.mask_uv).a;\n");
    }
    if program.premultiply {
        source.push_str("    rgb *= alpha;\n");
    }
    if program.green_from_alpha {
        source.push_str("    rgb.g = alpha;\n");
    }
    if program.green_masked {
        source.push_str("    rgb.g *= textureSample(cAlphaMaskTex, cSampler, in.mask_uv).g;\n");
    }
    source.push_str(
        r#"    return FragOut(vec4<f32>(rgb, alpha));
}
"#,
    );
    source
}

/// Generate WGSL for one vertex program.
///
/// Inputs are `@location(0) position: vec3<f32>`, `@location(1) color`,
/// `@location(2) uv`. `cMat` is a column-major 4×4 matrix in the same layout
/// the D3D9 programs use (`dp4 oPos.x, r0, c0` etc.).
pub fn vertex_wgsl(program: VertexProgram) -> String {
    let mut source = String::from(
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
    source.push_str(
        r#"    return out;
}
"#,
    );
    source
}

/// A 4×4 cubic Bezier patch in normalized local sprite space.
///
/// `identity` uses `(col/3, row/3)`, matching the neutral mesh.
#[derive(Debug, Clone, PartialEq)]
pub struct MeshPatch {
    /// Control points, row-major.
    pub control_points: [[f32; 2]; 16],
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
        Self { control_points }
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
    fn wgsl_contains_mask_only_when_needed() {
        assert!(!pixel_wgsl(&PIXEL_PROGRAMS[0]).contains("cAlphaMaskTex"));
        assert!(pixel_wgsl(&PIXEL_PROGRAMS[3]).contains("cAlphaMaskTex"));
        assert!(pixel_wgsl(&PIXEL_PROGRAMS[26]).contains("cPixelateTex"));
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
