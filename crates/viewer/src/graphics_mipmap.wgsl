// Source loads decode sRGB into linear light. The sRGB attachment encodes the
// result back into the UNORM storage that egui samples as gamma-encoded values.
@group(0) @binding(0) var source: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(positions[index], 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let source_size = textureDimensions(source, 0);
    let target_size = max(source_size / vec2<u32>(2u), vec2<u32>(1u));
    let footprint = vec2<f32>(source_size) / vec2<f32>(target_size);
    let start = floor(position.xy) * footprint;
    let end = min(start + footprint, vec2<f32>(source_size));
    let first = vec2<u32>(floor(start));
    var sum = vec4<f32>(0.0);

    // Each axis reduces N to max(floor(N / 2), 1). At most three source texels
    // overlap a destination texel, including odd sizes and one-pixel axes.
    // Area weights retain the final row/column instead of cropping odd edges.
    for (var y = 0u; y < 3u; y += 1u) {
        for (var x = 0u; x < 3u; x += 1u) {
            let texel = first + vec2<u32>(x, y);
            let low = max(start, vec2<f32>(texel));
            let high = min(end, vec2<f32>(texel) + vec2<f32>(1.0));
            let weight = max(high - low, vec2<f32>(0.0));
            if (all(texel < source_size) && all(weight > vec2<f32>(0.0))) {
                sum += textureLoad(source, vec2<i32>(texel), 0) * weight.x * weight.y;
            }
        }
    }
    return sum / (footprint.x * footprint.y);
}
