struct Params {
    hue_hue: array<vec4<f32>, 8>,
    hue_sat: array<vec4<f32>, 8>,
    hue_lum: array<vec4<f32>, 8>,
}

// Hue curves on the display level (Royalty Studio fork), as a grading
// suite has them: across, the colour wheel from red round to red again;
// up and down, how far the colours of that hue are turned round the wheel
// (half a turn either way), how much saturation they keep (none to twice),
// and how bright they are (about a stop of light either way: half a stop
// of the display's level, which its gamma makes 1.2 of light). Flat at the middle, the
// picture is as shot. A grey has no hue and takes no change; a colour
// takes it as far as it has colour, so a near-grey moves only a little. A
// turned hue keeps the colour's value and saturation, as a colour picker
// does; saturation turns about the colour's luma.

// A curve round the wheel at `hue`: through its points, the last running
// on into the first, each left at the slope between its neighbours -
// concat-effects' `hue_curve_value`, which the inspector draws.
fn hue_curve_at(curve: array<vec4<f32>, 8>, hue: f32) -> f32 {
    var p = curve;
    let n = clamp(i32(p[0].w), 2, 8);
    var i = n - 1;
    for (var k = 0; k < 7; k++) {
        if (k < n - 1 && hue >= p[k].x && hue < p[k + 1].x) {
            i = k;
        }
    }
    let at = select(hue, hue + 1.0, hue < p[0].x);
    let ib = (i + n - 1) % n;
    let ia = (i + 1) % n;
    let iaa = (i + 2) % n;
    let before = vec2<f32>(p[ib].x - select(0.0, 1.0, i == 0), p[ib].y);
    let a = p[i].xy;
    let b = vec2<f32>(p[ia].x + select(0.0, 1.0, i + 1 >= n), p[ia].y);
    let after = vec2<f32>(p[iaa].x + select(0.0, 1.0, i + 2 >= n), p[iaa].y);
    let slope_a = (b.y - before.y) / max(b.x - before.x, 1e-6);
    let slope_b = (after.y - a.y) / max(after.x - a.x, 1e-6);
    let h = max(b.x - a.x, 1e-6);
    let s = clamp((at - a.x) / h, 0.0, 1.0);
    let s2 = s * s;
    let s3 = s2 * s;
    let y = (2.0 * s3 - 3.0 * s2 + 1.0) * a.y + (s3 - 2.0 * s2 + s) * h * slope_a
        + (3.0 * s2 - 2.0 * s3) * b.y + (s3 - s2) * h * slope_b;
    return clamp(y, 0.0, 1.0);
}

// A colour from its hue (0 to 1), its saturation and its value: HSV's,
// the inverse of the library's `hue_of` with the extremes it came with.
fn from_hsv(hue: f32, saturation: f32, value: f32) -> vec3<f32> {
    let k = vec3<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0);
    let p = abs(fract(vec3<f32>(hue) + k) * 6.0 - vec3<f32>(3.0));
    return value * mix(vec3<f32>(1.0), clamp(p - vec3<f32>(1.0), vec3<f32>(0.0), vec3<f32>(1.0)), saturation);
}

// Whether a curve is flat at the middle: nothing to do.
fn resting(curve: array<vec4<f32>, 8>) -> bool {
    var p = curve;
    let n = clamp(i32(p[0].w), 2, 8);
    var flat = true;
    for (var k = 0; k < 8; k++) {
        if (k < n && abs(p[k].y - 0.5) > 1e-5) {
            flat = false;
        }
    }
    return flat;
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample(uv);
    if (resting(params.hue_hue) && resting(params.hue_sat) && resting(params.hue_lum)) {
        return c;
    }
    let rgb = c.rgb;
    // Round the wheel from red, 0 to 1: HSV's, the order the curve's
    // ground is painted in.
    let hue = hue_of(rgb) / 360.0;
    // How much colour there is to change: none at grey, all of it by a
    // chroma of 0.08.
    let chroma = max(rgb.r, max(rgb.g, rgb.b)) - min(rgb.r, min(rgb.g, rgb.b));
    let weight = smoothstep(0.0, 0.08, chroma);
    let y = luma(rgb);

    // The hue turned round the wheel, its value and saturation kept, so a
    // colour inside the gamut stays inside it.
    let hi = max(rgb.r, max(rgb.g, rgb.b));
    let turn = (hue_curve_at(params.hue_hue, hue) - 0.5) * weight;
    var graded = rgb;
    if (abs(turn) > 1e-6 && hi > 1e-6) {
        graded = from_hsv(fract(hue + turn + 1.0), chroma / hi, hi);
    }

    // Its saturation: none to twice.
    let keep = mix(1.0, 2.0 * hue_curve_at(params.hue_sat, hue), weight);
    graded = vec3<f32>(y) + (graded - vec3<f32>(y)) * keep;

    // Its brightness: about a stop of light either way.
    let light = exp2((hue_curve_at(params.hue_lum, hue) - 0.5) * weight);
    return held(vec4<f32>(graded * light, c.a));
}
