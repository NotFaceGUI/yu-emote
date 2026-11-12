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
    // Inside the mask: red. Outside: transparent. The projection reflects X
    // (see `soft::PROJECTION_MIRROR_X`): model column `x` lands on screen
    // column `width - 1 - x`, so the mask over model (0,0)-(2,2) covers screen
    // columns 6..8 while the unmasked quad covers 4..8.
    let screen_x = |x: usize| {
        if soft::PROJECTION_MIRROR_X {
            8 - 1 - x
        } else {
            x
        }
    };
    assert_eq!(pixel(&rgba, screen_x(1), 1), [255, 0, 0, 255]);
    assert_eq!(pixel(&rgba, screen_x(3), 3), [0, 0, 0, 0]);
    assert_eq!(pixel(&cpu.rgba8, screen_x(1), 1), [255, 0, 0, 255]);
    assert_eq!(pixel(&cpu.rgba8, screen_x(3), 3), [0, 0, 0, 0]);

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

#[test]
fn gpu_applies_presentation_rotation() {
    let plan = RenderPlan {
        width: 8,
        height: 8,
        items: vec![item(
            0,
            EmoteDrawPass::Normal,
            Vec::new(),
            (2.0, 2.0, 6.0, 6.0),
        )],
        mask_groups: Vec::new(),
        skipped: BTreeMap::new(),
    };
    let mut textures = BTreeMap::new();
    textures.insert(0, solid([255, 0, 0, 255]));
    // Rotate the 4x4 square 45 degrees around its center (4,4).
    let view = ViewTransform::player([4.0, 4.0], [4.0, 4.0], 1.0, std::f32::consts::FRAC_PI_4);

    let (cpu, _) = soft::render_with_textures(&plan, &textures, view).expect("cpu render");
    let pixel = |data: &[u8], x: usize, y: usize| -> [u8; 4] {
        let index = (y * 8 + x) * 4;
        [
            data[index],
            data[index + 1],
            data[index + 2],
            data[index + 3],
        ]
    };
    // Center stays covered; the axis-aligned corner of the original square is
    // outside the rotated diamond.
    assert_eq!(pixel(&cpu.rgba8, 4, 4), [255, 0, 0, 255]);
    assert_eq!(pixel(&cpu.rgba8, 2, 2), [0, 0, 0, 0]);

    let Ok((mut gpu, _adapter)) = GpuRenderer::headless() else {
        eprintln!("SKIP: no wgpu adapter");
        return;
    };
    let frame = gpu.render(&plan, &textures, view).expect("gpu render");
    let rgba = gpu.read_frame(&frame).expect("gpu readback");
    assert_eq!(pixel(&rgba, 4, 4), [255, 0, 0, 255]);
    assert_eq!(pixel(&rgba, 2, 2), [0, 0, 0, 0]);
}

#[test]
fn rotated_clip_uses_model_space_rectangle() {
    let mut draw = item(0, EmoteDrawPass::Normal, Vec::new(), (0.0, 0.0, 8.0, 8.0));
    draw.clip_rect = Some([2.0, 2.0, 6.0, 6.0]);
    let plan = RenderPlan {
        width: 8,
        height: 8,
        items: vec![draw],
        mask_groups: Vec::new(),
        skipped: BTreeMap::new(),
    };
    let mut textures = BTreeMap::new();
    textures.insert(0, solid([255, 0, 0, 255]));
    let view = ViewTransform::player([4.0, 4.0], [4.0, 4.0], 1.0, std::f32::consts::FRAC_PI_4);
    let pixel = |data: &[u8], x: usize, y: usize| -> [u8; 4] {
        let index = (y * 8 + x) * 4;
        [
            data[index],
            data[index + 1],
            data[index + 2],
            data[index + 3],
        ]
    };

    let (cpu, _) = soft::render_with_textures(&plan, &textures, view).expect("cpu render");
    // The clip is the model-space square 2..6; after rotation it is a diamond.
    // (2,2) is inside the screen-space AABB but outside the diamond, so the
    // CPU path must not paint it.
    assert_eq!(pixel(&cpu.rgba8, 4, 4), [255, 0, 0, 255]);
    assert_eq!(pixel(&cpu.rgba8, 2, 2), [0, 0, 0, 0]);

    let Ok((mut gpu, _adapter)) = GpuRenderer::headless() else {
        eprintln!("SKIP: no wgpu adapter");
        return;
    };
    let frame = gpu.render(&plan, &textures, view).expect("gpu render");
    let rgba = gpu.read_frame(&frame).expect("gpu readback");
    assert_eq!(pixel(&rgba, 4, 4), [255, 0, 0, 255]);
    assert_eq!(pixel(&rgba, 2, 2), [0, 0, 0, 0]);
}
