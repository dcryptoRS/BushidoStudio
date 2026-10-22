struct Params {
    hue: f32,
    width: f32,
    sat_low: f32,
    sat_high: f32,
    luma_low: f32,
    luma_high: f32,
    softness: f32,
    invert: f32,
    matte: f32,
    turn: f32,
    saturation: f32,
    brightness: f32,
}

// A secondary on the display level, as a grading suite's HSL qualifier
// (Royalty Studio fork): the matte is how near a pixel's hue is to `hue`,
// within half of `width` and fading over the softness past it, times how
// far its saturation (chroma over its brightest channel) and its luma sit
// inside their ranges, each fading at its ends. Inside the matte the
// colour is turned, saturated and brightened; outside it, it is as shot.

// Whether `v` lies in `low..high`, fading over `soft` past either end;
// never a hard edge, which smoothstep cannot draw.
fn inside(v: f32, low: f32, high: f32, soft: f32) -> f32 {
    let s = max(soft, 1e-3);
    return smoothstep(low - s, low, v) * (1.0 - smoothstep(high, high + s, v));
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample(uv);
    let rgb = c.rgb;
    let soft = params.softness / 100.0;
    let apart = abs(fract((hue_of(rgb) - params.hue) / 360.0 + 0.5) * 360.0 - 180.0);
    let reach = params.width * 0.5;
    let by_hue = 1.0 - smoothstep(reach, reach + max(soft * 90.0, 0.5), apart);
    let brightest = max(max(rgb.r, rgb.g), rgb.b);
    let sat = chroma_of(rgb) / max(brightest, 1e-4);
    let by_sat = inside(sat, params.sat_low / 100.0, params.sat_high / 100.0, soft * 0.5);
    let by_luma = inside(luma(rgb), params.luma_low / 100.0, params.luma_high / 100.0, soft * 0.5);
    var matte = clamp(by_hue * by_sat * by_luma, 0.0, 1.0);
    if (params.invert > 0.5) {
        matte = 1.0 - matte;
    }
    if (params.matte > 0.5) {
        return vec4<f32>(vec3<f32>(matte), c.a);
    }
    var out = rgb;
    if (abs(params.turn) > 1e-4) {
        out = hue_rotate(out, params.turn);
    }
    out = saturation(out, params.saturation / 100.0);
    out = out * exp2(params.brightness / 100.0);
    return vec4<f32>(mix(rgb, out, matte), c.a);
}
