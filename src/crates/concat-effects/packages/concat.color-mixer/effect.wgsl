struct Params {
    red_hue: f32,
    red_sat: f32,
    red_lum: f32,
    orange_hue: f32,
    orange_sat: f32,
    orange_lum: f32,
    yellow_hue: f32,
    yellow_sat: f32,
    yellow_lum: f32,
    green_hue: f32,
    green_sat: f32,
    green_lum: f32,
    aqua_hue: f32,
    aqua_sat: f32,
    aqua_lum: f32,
    blue_hue: f32,
    blue_sat: f32,
    blue_lum: f32,
    purple_hue: f32,
    purple_sat: f32,
    purple_lum: f32,
    magenta_hue: f32,
    magenta_sat: f32,
    magenta_lum: f32,
}

// A colour mixer on the display level (Royalty Studio fork): eight bands
// centred at 0, 30, 60, 120, 180, 240, 270 and 300 degrees. A pixel's hue
// falls between two neighbouring centres and takes each one's turn,
// saturation and luminance in proportion to how near it is, eased so the
// bands meet without a seam; how far from grey it is weighs it all, so a
// grey is never touched.

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample(uv);
    let rgb = c.rgb;
    var centres = array<f32, 9>(0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 270.0, 300.0, 360.0);
    var turns = array<f32, 8>(params.red_hue, params.orange_hue, params.yellow_hue, params.green_hue, params.aqua_hue, params.blue_hue, params.purple_hue, params.magenta_hue);
    var sats = array<f32, 8>(params.red_sat, params.orange_sat, params.yellow_sat, params.green_sat, params.aqua_sat, params.blue_sat, params.purple_sat, params.magenta_sat);
    var lums = array<f32, 8>(params.red_lum, params.orange_lum, params.yellow_lum, params.green_lum, params.aqua_lum, params.blue_lum, params.purple_lum, params.magenta_lum);
    let hue = hue_of(rgb);
    var band = 7u;
    for (var i = 0u; i < 8u; i = i + 1u) {
        if (hue >= centres[i] && hue < centres[i + 1u]) {
            band = i;
        }
    }
    let next = (band + 1u) % 8u;
    let t = smoothstep(0.0, 1.0, (hue - centres[band]) / (centres[band + 1u] - centres[band]));
    let weight = smoothstep(0.0, 0.2, chroma_of(rgb));
    let turn = mix(turns[band], turns[next], t) * weight;
    let sat = mix(sats[band], sats[next], t) * weight / 100.0;
    let lum = mix(lums[band], lums[next], t) * weight / 100.0;
    var out = rgb;
    if (abs(turn) > 1e-4) {
        out = hue_rotate(out, turn);
    }
    out = saturation(out, 1.0 + sat);
    out = out * (1.0 + 0.6 * lum);
    return vec4<f32>(out, c.a);
}
