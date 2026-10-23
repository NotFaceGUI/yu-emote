//! GPU renderer tests. Skips explicitly when no adapter is available.
//!
//! The synthetic plan exercises the inner-mask path, which the current v4
//! fixtures do not use: a 2x2 mask source clips a 4x4 color quad.

use std::collections::BTreeMap;

use yu_emote::EmoteDrawPass;
use yu_emote::gpu::GpuRenderer;
use yu_emote::plan::{DrawItem, MaskGroup, PlanVertex, RenderPlan};
use yu_emote::soft::{self, ViewTransform};
use yu_emote::texture::DecodedTexture;

fn quad(x0: f32, y0: f32, x1: f32, y1: f32) -> (Vec<PlanVertex>, Vec<u32>) {
    let color = [1.0, 1.0, 1.0, 1.0];
    let vertex = |x: f32, y: f32, u: f32, v: f32| PlanVertex {
        position: [x, y],
        color,
        uv: [u, v],
    };
    (
        vec![
            vertex(x0, y0, 0.0, 0.0),
            vertex(x0, y1, 0.0, 1.0),
            vertex(x1, y0, 1.0, 0.0),
            vertex(x1, y0, 1.0, 0.0),
            vertex(x0, y1, 0.0, 1.0),
            vertex(x1, y1, 1.0, 1.0),
        ],
        vec![0, 1, 2, 3, 4, 5],
    )
}

fn item(
    resource: u32,
    pass: EmoteDrawPass,
    mask_groups: Vec<usize>,
    rect: (f32, f32, f32, f32),
) -> DrawItem {
    let (vertices, indices) = quad(rect.0, rect.1, rect.2, rect.3);
    let bounds = Some(([rect.0, rect.1], [rect.2, rect.3]));
    DrawItem {
        label: String::new(),
        path: String::new(),
        page: format!("tex#{resource}"),
        resource: Some(resource),
        page_width: 2,
        page_height: 2,
        blend_mode: 0,
        pass,
        mesh: false,
        mesh_division: 0,
        stencil_type: 0,
        clip_rect: None,
        mask_groups,
        wipe: [0.0; 3],
        vertices,
        indices,
        bounds,
    }
}

fn solid(color: [u8; 4]) -> DecodedTexture {
    DecodedTexture {
        width: 2,
        height: 2,
        rgba8: color.repeat(4),
    }
}

#[test]
fn gpu_matches_cpu_and_applies_inner_mask() {
    let mut plan = RenderPlan {
        width: 8,
        height: 8,
        items: vec![
            item(
                0,
                EmoteDrawPass::MaskGeneration,
                Vec::new(),
                (0.0, 0.0, 2.0, 2.0),
            ),
            item(1, EmoteDrawPass::Normal, vec![0], (0.0, 0.0, 4.0, 4.0)),
        ],
        mask_groups: vec![MaskGroup {
            sources: vec![0],
            owner: Vec::new(),
        }],
        skipped: BTreeMap::new(),
    };
    // The mask source is drawn by the mask pass, not as color.
    plan.items[0].mask_groups.clear();
    let mut textures = BTreeMap::new();
    textures.insert(0, solid([0, 255, 0, 255]));
    textures.insert(1, solid([255, 0, 0, 255]));

    let (cpu, _) = soft::render_with_textures(&plan, &textures, ViewTransform::identity())
        .expect("cpu render");

    let Ok((mut gpu, _adapter)) = GpuRenderer::headless() else {
        eprintln!("SKIP: no wgpu adapter");
        return;
    };
    let frame = gpu
        .render(&plan, &textures, ViewTransform::identity())
        .expect("gpu render");
    let rgba = gpu.read_frame(&frame).expect("gpu readback");

    let pixel = |data: &[u8], x: usize, y: usize| -> [u8; 4] {
        let index = (y * 8 + x) * 4;
        [
            data[index],
            data[index + 1],
            data[index + 2],
            data[index + 3],
        ]
    };
    // Inside the mask: red. Outside: transparent.
    assert_eq!(pixel(&rgba, 1, 1), [255, 0, 0, 255]);
    assert_eq!(pixel(&rgba, 3, 3), [0, 0, 0, 0]);
    assert_eq!(pixel(&cpu.rgba8, 1, 1), [255, 0, 0, 255]);
    assert_eq!(pixel(&cpu.rgba8, 3, 3), [0, 0, 0, 0]);

    // Interior pixels should agree exactly; edges may differ by filtering.
    let mut diff = 0u64;
    for (a, b) in cpu.rgba8.chunks_exact(4).zip(rgba.chunks_exact(4)) {
        if a == [255, 0, 0, 255] && b == [255, 0, 0, 255] {
            continue;
        }
        for channel in 0..4 {
            diff += u64::from((a[channel] as i32 - b[channel] as i32).unsigned_abs());
        }
    }
    assert!(diff < 4000, "cpu/gpu differ too much: {diff}");
}
