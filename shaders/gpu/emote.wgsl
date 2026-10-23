struct View {
    scale: vec2<f32>,
    offset: vec2<f32>,
    size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> view: View;

@group(1) @binding(0) var sprite_tex: texture_2d<f32>;
@group(1) @binding(1) var sprite_sampler: sampler;
@group(1) @binding(2) var mask_tex: texture_2d<f32>;

struct VsIn {
    @location(0) position: vec2<f32>,
    @location(1) texcoord: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) blend_mode: u32,
    @location(4) clip_rect: vec4<f32>,
    @location(5) wipe: vec3<f32>,
};

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) texcoord: vec2<f32>,
    @location(1) color: vec4<f32>,
    @interpolate(flat)
    @location(2) blend_mode: u32,
    @location(3) model_position: vec2<f32>,
    @location(4) clip_rect: vec4<f32>,
    @location(5) wipe: vec3<f32>,
};

@vertex
fn vs_main(input: VsIn) -> VsOut {
    let screen = input.position * view.scale + view.offset;
    let ndc = vec2<f32>(
        screen.x / view.size.x * 2.0 - 1.0,
        1.0 - screen.y / view.size.y * 2.0,
    );
    var out: VsOut;
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    out.texcoord = input.texcoord;
    out.color = input.color;
    out.blend_mode = input.blend_mode;
    out.model_position = input.position;
    out.clip_rect = input.clip_rect;
    out.wipe = input.wipe;
    return out;
}

// Native MMotionDevice::SetBlendMode texture stage (`sub_1041A190`):
// high nibble 0x10 = D3DTOP_MODULATE2X; modes 3/4 premultiply RGB by alpha;
// mode 5 selects the complement of the current color.
fn native_texture_stage(c: vec4<f32>, blend_mode: u32) -> vec4<f32> {
    var rgb = c.rgb;
    let alpha = c.a;
    if ((blend_mode & 0xF0u) == 0x10u) {
        rgb = clamp(rgb * 2.0, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    let low = blend_mode & 0xFF0Fu;
    if (low == 3u || low == 4u) {
        rgb = rgb * alpha;
    } else if (low == 5u) {
        rgb = vec3<f32>(1.0) - rgb;
    }
    return vec4<f32>(rgb, alpha);
}

fn clipped(input: VsOut) -> bool {
    return input.model_position.x < input.clip_rect.x
        || input.model_position.y < input.clip_rect.y
        || input.model_position.x > input.clip_rect.z
        || input.model_position.y > input.clip_rect.w;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    if (clipped(input)) {
        discard;
    }
    var c = textureSample(sprite_tex, sprite_sampler, input.texcoord) * input.color;
    if (input.wipe.z > 0.5) {
        c = vec4<f32>(c.rgb, clamp(c.a * input.wipe.x + input.wipe.y, 0.0, 1.0));
    }
    c = native_texture_stage(c, input.blend_mode);
    let mask = textureSample(mask_tex, sprite_sampler, input.position.xy / view.size).r;
    c = vec4<f32>(c.rgb, c.a * mask);
    if (c.a <= 0.003) {
        discard;
    }
    return c;
}

// Mask-generation pass: write texture alpha times vertex alpha. The mask
// target is Rgba8Unorm with MAX blending, so overlapping sources union.
@fragment
fn fs_mask(input: VsOut) -> @location(0) vec4<f32> {
    if (clipped(input)) {
        discard;
    }
    var c = textureSample(sprite_tex, sprite_sampler, input.texcoord) * input.color;
    if (input.wipe.z > 0.5) {
        c = vec4<f32>(c.rgb, clamp(c.a * input.wipe.x + input.wipe.y, 0.0, 1.0));
    }
    return vec4<f32>(c.a, 0.0, 0.0, 1.0);
}
