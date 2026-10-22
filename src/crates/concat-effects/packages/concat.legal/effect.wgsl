struct Params { ceiling: f32, floor: f32, softness: f32, chroma: f32 }

// Legal levels on the display level (Royalty Studio fork), where 0 and 1
// are 0 and 100 IRE. A colour richer than the limit - its chroma, the
// distance from its luma, against full - is brought back towards its luma;
// a luma over the knee (the ceiling less the softness) is eased up to the
// ceiling and the colour scaled with it, so a highlight keeps its hue; then
// no channel is let past the ceiling or under the floor.

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample(uv);
    var rgb = c.rgb;
    let top = params.ceiling / 100.0;
    let bottom = params.floor / 100.0;
    let limit = params.chroma / 100.0;
    let y = luma(rgb);
    let chroma = chroma_of(rgb);
    if (chroma > limit) {
        rgb = vec3<f32>(y) + (rgb - vec3<f32>(y)) * (limit / chroma);
    }
    let soft = params.softness / 100.0;
    let knee = top - soft;
    if (y > knee && y > 1e-4) {
        var eased = top;
        if (soft > 1e-4) {
            eased = knee + soft * (1.0 - exp(-(y - knee) / soft));
        }
        rgb = rgb * (min(eased, y) / y);
    }
    rgb = clamp(rgb, vec3<f32>(bottom), vec3<f32>(top));
    return vec4<f32>(rgb, c.a);
}
