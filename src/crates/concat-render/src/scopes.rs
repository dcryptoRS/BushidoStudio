// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors
// SPDX-FileCopyrightText: 2026 Royalty Studio

//! The video scopes (Royalty Studio fork): a waveform, an RGB parade, a
//! vectorscope and a histogram of a frame, worked out on the GPU from the
//! monitor's own texture and drawn into textures the window shows as they
//! are. Nothing is read back.
//!
//! One pass counts: every pixel of the frame adds itself to the cells of
//! the scopes asked for - its Rec. 709 luma (Y' = 0.2126 R' + 0.7152 G' +
//! 0.0722 B') at its column for the waveform, each channel at its column
//! for the parade, its Cb and Cr for the vectorscope, and its levels for
//! the histogram. Then one pass a scope turns the counts into a picture:
//! a trace as bright as the log of how many pixels landed there, and for
//! the histogram the bins against the tallest one. The values measured are
//! the frame's as shown, display-encoded, so 0 and 1 are 0 and 100 IRE.
//!
//! The graticules - the levels, the colour targets, the skin-tone line -
//! are the window's to draw over these, in its own type and theme.

/// The waveform: luma by column.
pub const WAVEFORM: u32 = 1;
/// The parade: red, green and blue side by side, each by column.
pub const PARADE: u32 = 2;
/// The vectorscope: Cb across, Cr up.
pub const VECTORSCOPE: u32 = 4;
/// The histogram: how many pixels at each level, red, green, blue and luma.
pub const HISTOGRAM: u32 = 8;
/// Every scope.
pub const ALL: u32 = WAVEFORM | PARADE | VECTORSCOPE | HISTOGRAM;

/// The waveform's columns, and every scope's levels: 8 bits' worth.
pub const WAVE_COLUMNS: u32 = 512;
/// One channel's columns in the parade.
pub const PARADE_COLUMNS: u32 = 256;
/// The levels a trace is counted at, bottom to top.
pub const LEVELS: u32 = 256;
/// The vectorscope's side, in cells.
pub const VECTOR_SIDE: u32 = 320;
/// How far out a Cb or Cr of 0.5 lands, as a fraction of the side: 0.45,
/// so a fully saturated primary sits just inside the edge.
pub const VECTOR_REACH: f32 = 0.9;
/// The histogram's picture.
pub const HISTOGRAM_SIZE: (u32, u32) = (512, 200);

const WAVE_AT: u32 = 0;
const PARADE_AT: u32 = WAVE_AT + WAVE_COLUMNS * LEVELS;
const VECTOR_AT: u32 = PARADE_AT + 3 * PARADE_COLUMNS * LEVELS;
const HIST_AT: u32 = VECTOR_AT + VECTOR_SIDE * VECTOR_SIDE;
const PEAK_AT: u32 = HIST_AT + 4 * LEVELS;
const COUNTS: u32 = PEAK_AT + 4;

const SHADER: &str = r#"
struct Info { width: u32, height: u32, mask: u32, pad: u32 }

@group(0) @binding(0) var frame: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> counts: array<atomic<u32>>;
@group(0) @binding(2) var<uniform> info: Info;
@group(0) @binding(3) var wave_out: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(4) var parade_out: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(5) var vector_out: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(6) var hist_out: texture_storage_2d<rgba8unorm, write>;

const WAVE_COLUMNS: u32 = __WAVE_COLUMNS__u;
const PARADE_COLUMNS: u32 = __PARADE_COLUMNS__u;
const LEVELS: u32 = __LEVELS__u;
const VECTOR_SIDE: u32 = __VECTOR_SIDE__u;
const VECTOR_REACH: f32 = __VECTOR_REACH__;
const HIST_W: u32 = __HIST_W__u;
const HIST_H: u32 = __HIST_H__u;
const WAVE_AT: u32 = __WAVE_AT__u;
const PARADE_AT: u32 = __PARADE_AT__u;
const VECTOR_AT: u32 = __VECTOR_AT__u;
const HIST_AT: u32 = __HIST_AT__u;
const PEAK_AT: u32 = __PEAK_AT__u;

fn level(v: f32) -> u32 {
    return min(u32(round(clamp(v, 0.0, 1.0) * f32(LEVELS - 1u))), LEVELS - 1u);
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// Where a colour lands on the vectorscope: Cb across, Cr up, the middle
// grey, as cells.
fn vector_cell(c: vec3<f32>) -> vec2<u32> {
    let y = luma(c);
    let cb = (c.b - y) / 1.8556;
    let cr = (c.r - y) / 1.5748;
    let u = 0.5 + cb * VECTOR_REACH;
    let v = 0.5 - cr * VECTOR_REACH;
    let side = f32(VECTOR_SIDE);
    return vec2<u32>(
        min(u32(clamp(u, 0.0, 1.0) * side), VECTOR_SIDE - 1u),
        min(u32(clamp(v, 0.0, 1.0) * side), VECTOR_SIDE - 1u),
    );
}

@compute @workgroup_size(16, 16)
fn accumulate(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= info.width || id.y >= info.height) {
        return;
    }
    let c = clamp(textureLoad(frame, vec2<i32>(id.xy), 0).rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    let y = luma(c);
    if ((info.mask & 1u) != 0u) {
        let column = min(id.x * WAVE_COLUMNS / info.width, WAVE_COLUMNS - 1u);
        atomicAdd(&counts[WAVE_AT + level(y) * WAVE_COLUMNS + column], 1u);
    }
    if ((info.mask & 2u) != 0u) {
        let column = min(id.x * PARADE_COLUMNS / info.width, PARADE_COLUMNS - 1u);
        let row = 3u * PARADE_COLUMNS;
        atomicAdd(&counts[PARADE_AT + level(c.r) * row + column], 1u);
        atomicAdd(&counts[PARADE_AT + level(c.g) * row + PARADE_COLUMNS + column], 1u);
        atomicAdd(&counts[PARADE_AT + level(c.b) * row + 2u * PARADE_COLUMNS + column], 1u);
    }
    if ((info.mask & 4u) != 0u) {
        let cell = vector_cell(c);
        atomicAdd(&counts[VECTOR_AT + cell.y * VECTOR_SIDE + cell.x], 1u);
    }
    if ((info.mask & 8u) != 0u) {
        atomicAdd(&counts[HIST_AT + level(c.r)], 1u);
        atomicAdd(&counts[HIST_AT + LEVELS + level(c.g)], 1u);
        atomicAdd(&counts[HIST_AT + 2u * LEVELS + level(c.b)], 1u);
        atomicAdd(&counts[HIST_AT + 3u * LEVELS + level(y)], 1u);
    }
}

// The histogram's tallest bin of each channel, the two ends left out: a
// clipped white or a crushed black is a spike that would flatten the rest.
@compute @workgroup_size(256)
fn peak(@builtin(local_invocation_id) id: vec3<u32>) {
    let bin = id.x;
    if (bin == 0u || bin >= LEVELS - 1u) {
        return;
    }
    for (var channel = 0u; channel < 4u; channel = channel + 1u) {
        atomicMax(&counts[PEAK_AT + channel], atomicLoad(&counts[HIST_AT + channel * LEVELS + bin]));
    }
}

// How bright a cell with `count` pixels is: by its log, against what a
// cell holding `reference` would be.
fn glow(count: u32, reference: f32) -> f32 {
    return clamp(log2(1.0 + f32(count)) / log2(1.0 + max(reference, 1.0)), 0.0, 1.0);
}

// A level at the very ends is one the picture lost: drawn in red.
fn trace(level: u32, count: u32, tint: vec3<f32>, reference: f32) -> vec4<f32> {
    let lit = glow(count, reference);
    if (count > 0u && (level == 0u || level == LEVELS - 1u)) {
        return vec4<f32>(1.0, 0.27, 0.23, max(lit, 0.85));
    }
    return vec4<f32>(tint, lit);
}

@compute @workgroup_size(16, 16)
fn resolve_waveform(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= WAVE_COLUMNS || id.y >= LEVELS) {
        return;
    }
    let level = LEVELS - 1u - id.y;
    let count = atomicLoad(&counts[WAVE_AT + level * WAVE_COLUMNS + id.x]);
    let per_column = f32(info.width) * f32(info.height) / f32(WAVE_COLUMNS);
    textureStore(wave_out, id.xy, trace(level, count, vec3<f32>(0.80, 1.0, 0.86), per_column / 8.0));
}

@compute @workgroup_size(16, 16)
fn resolve_parade(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= 3u * PARADE_COLUMNS || id.y >= LEVELS) {
        return;
    }
    let level = LEVELS - 1u - id.y;
    let count = atomicLoad(&counts[PARADE_AT + level * 3u * PARADE_COLUMNS + id.x]);
    let channel = id.x / PARADE_COLUMNS;
    var tint = vec3<f32>(1.0, 0.36, 0.33);
    if (channel == 1u) {
        tint = vec3<f32>(0.38, 1.0, 0.45);
    } else if (channel == 2u) {
        tint = vec3<f32>(0.40, 0.62, 1.0);
    }
    let per_column = f32(info.width) * f32(info.height) / f32(PARADE_COLUMNS);
    textureStore(parade_out, id.xy, trace(level, count, tint, per_column / 8.0));
}

// Each cell in the colour it stands for, as bright as how much of the
// picture is there.
@compute @workgroup_size(16, 16)
fn resolve_vector(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= VECTOR_SIDE || id.y >= VECTOR_SIDE) {
        return;
    }
    let count = atomicLoad(&counts[VECTOR_AT + id.y * VECTOR_SIDE + id.x]);
    let side = f32(VECTOR_SIDE);
    let cb = ((f32(id.x) + 0.5) / side - 0.5) / VECTOR_REACH;
    let cr = (0.5 - (f32(id.y) + 0.5) / side) / VECTOR_REACH;
    let y = 0.62;
    let hue = clamp(vec3<f32>(y + 1.5748 * cr, y - 0.1873 * cb - 0.4681 * cr, y + 1.8556 * cb), vec3<f32>(0.0), vec3<f32>(1.0));
    let tint = mix(vec3<f32>(0.92), hue, 0.85);
    let total = f32(info.width) * f32(info.height);
    textureStore(vector_out, id.xy, vec4<f32>(tint, glow(count, total / 3000.0)));
}

// The three channels laid over each other, each a filled shape against
// its tallest bin, and luma as a white edge on top.
@compute @workgroup_size(16, 16)
fn resolve_histogram(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= HIST_W || id.y >= HIST_H) {
        return;
    }
    let bin = min(id.x * LEVELS / HIST_W, LEVELS - 1u);
    let height = 1.0 - (f32(id.y) + 0.5) / f32(HIST_H);
    var colour = vec3<f32>(0.0);
    var alpha = 0.0;
    var tints = array<vec3<f32>, 3>(vec3<f32>(1.0, 0.25, 0.22), vec3<f32>(0.25, 0.95, 0.35), vec3<f32>(0.28, 0.5, 1.0));
    for (var channel = 0u; channel < 3u; channel = channel + 1u) {
        let peak = max(f32(atomicLoad(&counts[PEAK_AT + channel])), 1.0);
        let reach = min(f32(atomicLoad(&counts[HIST_AT + channel * LEVELS + bin])) / peak, 1.0);
        if (height <= reach) {
            colour = colour + tints[channel];
            alpha = alpha + 0.34;
        }
    }
    let luma_peak = max(f32(atomicLoad(&counts[PEAK_AT + 3u])), 1.0);
    let luma_reach = min(f32(atomicLoad(&counts[HIST_AT + 3u * LEVELS + bin])) / luma_peak, 1.0);
    if (abs(height - luma_reach) < 1.5 / f32(HIST_H)) {
        colour = vec3<f32>(1.0);
        alpha = 1.0;
    }
    textureStore(hist_out, id.xy, vec4<f32>(clamp(colour, vec3<f32>(0.0), vec3<f32>(1.0)), clamp(alpha, 0.0, 1.0)));
}
"#;

/// The scopes' pictures from one frame: a texture for each scope asked
/// for, `Rgba8Unorm` with straight alpha, to be drawn over a dark ground.
#[derive(Default)]
pub struct ScopeImages {
    /// Luma by column, [`WAVE_COLUMNS`] × [`LEVELS`], 100 IRE at the top.
    pub waveform: Option<wgpu::Texture>,
    /// Red, green and blue, each [`PARADE_COLUMNS`] wide, side by side.
    pub parade: Option<wgpu::Texture>,
    /// [`VECTOR_SIDE`] square: Cb across, Cr up, grey in the middle.
    pub vectorscope: Option<wgpu::Texture>,
    /// [`HISTOGRAM_SIZE`]: levels across, black on the left.
    pub histogram: Option<wgpu::Texture>,
}

/// The four scopes' textures, as one set.
struct Outputs {
    waveform: wgpu::Texture,
    parade: wgpu::Texture,
    vectorscope: wgpu::Texture,
    histogram: wgpu::Texture,
}

/// What draws the scopes, on the device the frames are on.
pub struct Scopes {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    accumulate: wgpu::ComputePipeline,
    peak: wgpu::ComputePipeline,
    resolve: [wgpu::ComputePipeline; 4],
    counts: wgpu::Buffer,
    info: wgpu::Buffer,
    /// Two sets drawn into in turn: the window may still be showing the
    /// last set when the next is drawn, and a texture of its own each time
    /// is what tells it the picture changed.
    outputs: [Outputs; 2],
    turn: usize,
}

impl Scopes {
    /// The pipelines and buffers, on `device`. Built once; drawing takes
    /// only the frame.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Scopes {
        let source = SHADER
            .replace("__WAVE_COLUMNS__", &WAVE_COLUMNS.to_string())
            .replace("__PARADE_COLUMNS__", &PARADE_COLUMNS.to_string())
            .replace("__LEVELS__", &LEVELS.to_string())
            .replace("__VECTOR_SIDE__", &VECTOR_SIDE.to_string())
            .replace("__VECTOR_REACH__", &format!("{VECTOR_REACH:?}"))
            .replace("__HIST_W__", &HISTOGRAM_SIZE.0.to_string())
            .replace("__HIST_H__", &HISTOGRAM_SIZE.1.to_string())
            .replace("__WAVE_AT__", &WAVE_AT.to_string())
            .replace("__PARADE_AT__", &PARADE_AT.to_string())
            .replace("__VECTOR_AT__", &VECTOR_AT.to_string())
            .replace("__HIST_AT__", &HIST_AT.to_string())
            .replace("__PEAK_AT__", &PEAK_AT.to_string());
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("concat scopes"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format: wgpu::TextureFormat::Rgba8Unorm,
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("concat scopes"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                storage(3),
                storage(4),
                storage(5),
                storage(6),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("concat scopes"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some(entry),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            })
        };
        let counts = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("concat scopes counts"),
            size: u64::from(COUNTS) * 4,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let info = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("concat scopes info"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let outputs = [Outputs::new(device), Outputs::new(device)];
        Scopes {
            accumulate: pipeline("accumulate"),
            peak: pipeline("peak"),
            resolve: [
                pipeline("resolve_waveform"),
                pipeline("resolve_parade"),
                pipeline("resolve_vector"),
                pipeline("resolve_histogram"),
            ],
            device: device.clone(),
            queue: queue.clone(),
            layout,
            counts,
            info,
            outputs,
            turn: 0,
        }
    }

    /// The scopes in `wanted` (a mask of [`WAVEFORM`], [`PARADE`],
    /// [`VECTORSCOPE`] and [`HISTOGRAM`]) of `frame`, an `Rgba8Unorm`
    /// texture on this device that can be bound. Submitted, not waited on:
    /// whatever draws the textures next is ordered after this on the queue.
    ///
    /// Call it from the thread that submits to the device's queue, as the
    /// compositor's own drawing is.
    pub fn draw(&mut self, frame: &wgpu::Texture, wanted: u32) -> ScopeImages {
        let wanted = wanted & ALL;
        if wanted == 0 {
            return ScopeImages::default();
        }
        self.turn = 1 - self.turn;
        let outputs = &self.outputs[self.turn];
        let (width, height) = (frame.width(), frame.height());
        let info: Vec<u8> = [width, height, wanted, 0]
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        self.queue.write_buffer(&self.info, 0, &info);
        let view =
            |texture: &wgpu::Texture| texture.create_view(&wgpu::TextureViewDescriptor::default());
        let (frame_view, wave, parade, vector, histogram) = (
            view(frame),
            view(&outputs.waveform),
            view(&outputs.parade),
            view(&outputs.vectorscope),
            view(&outputs.histogram),
        );
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("concat scopes"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&frame_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.counts.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.info.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&wave),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&parade),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&vector),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(&histogram),
                },
            ],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("concat scopes"),
            });
        encoder.clear_buffer(&self.counts, 0, None);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("concat scopes"),
                timestamp_writes: None,
            });
            pass.set_bind_group(0, &group, &[]);
            pass.set_pipeline(&self.accumulate);
            pass.dispatch_workgroups(width.div_ceil(16), height.div_ceil(16), 1);
            if wanted & HISTOGRAM != 0 {
                pass.set_pipeline(&self.peak);
                pass.dispatch_workgroups(1, 1, 1);
            }
            let sizes = [
                (WAVEFORM, (WAVE_COLUMNS, LEVELS)),
                (PARADE, (3 * PARADE_COLUMNS, LEVELS)),
                (VECTORSCOPE, (VECTOR_SIDE, VECTOR_SIDE)),
                (HISTOGRAM, HISTOGRAM_SIZE),
            ];
            for ((scope, (w, h)), pipeline) in sizes.iter().zip(&self.resolve) {
                if wanted & scope != 0 {
                    pass.set_pipeline(pipeline);
                    pass.dispatch_workgroups(w.div_ceil(16), h.div_ceil(16), 1);
                }
            }
        }
        self.queue.submit([encoder.finish()]);
        let pick =
            |scope: u32, texture: &wgpu::Texture| (wanted & scope != 0).then(|| texture.clone());
        ScopeImages {
            waveform: pick(WAVEFORM, &outputs.waveform),
            parade: pick(PARADE, &outputs.parade),
            vectorscope: pick(VECTORSCOPE, &outputs.vectorscope),
            histogram: pick(HISTOGRAM, &outputs.histogram),
        }
    }

    /// The counts the last [`Scopes::draw`] made, read back: for the tests.
    #[cfg(test)]
    fn counts(&self) -> Vec<u32> {
        let size = u64::from(COUNTS) * 4;
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("concat scopes readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_buffer_to_buffer(&self.counts, 0, &staging, 0, size);
        self.queue.submit([encoder.finish()]);
        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_secs(10)),
            })
            .expect("the device finishes");
        let data = slice.get_mapped_range().expect("maps");
        data.chunks_exact(4)
            .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .collect()
    }
}

impl Outputs {
    fn new(device: &wgpu::Device) -> Outputs {
        let texture = |label: &str, (width, height): (u32, u32)| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                // Render attachment too: the window takes a texture to show
                // only with both, though nothing is ever drawn into this.
                usage: wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        Outputs {
            waveform: texture("concat waveform", (WAVE_COLUMNS, LEVELS)),
            parade: texture("concat parade", (3 * PARADE_COLUMNS, LEVELS)),
            vectorscope: texture("concat vectorscope", (VECTOR_SIDE, VECTOR_SIDE)),
            histogram: texture("concat histogram", HISTOGRAM_SIZE),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device of the machine's, or none - and then the test says so and
    /// passes, as the compositor's suite does.
    fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let gpu = crate::WgpuCompositor::new();
        if gpu.is_none() {
            assert!(
                std::env::var_os("CONCAT_REQUIRE_GPU").is_none(),
                "CONCAT_REQUIRE_GPU is set and no GPU adapter is usable"
            );
            eprintln!("no usable GPU adapter; skipping");
        }
        gpu.map(|gpu| (gpu.device().clone(), gpu.queue().clone()))
    }

    /// A frame of `width` × `height` pixels, each `pixel(x, y)`.
    fn frame(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        pixel: impl Fn(u32, u32) -> [u8; 4],
    ) -> wgpu::Texture {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scopes test frame"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&pixel(x, y));
            }
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        texture
    }

    #[test]
    fn a_grey_frame_is_one_line_on_the_waveform_and_the_middle_of_the_vectorscope() {
        let Some((device, queue)) = device() else {
            return;
        };
        let (width, height) = (64, 32);
        let grey = frame(&device, &queue, width, height, |_, _| [128, 128, 128, 255]);
        let mut scopes = Scopes::new(&device, &queue);
        let images = scopes.draw(&grey, ALL);
        assert!(images.waveform.is_some() && images.histogram.is_some());
        let counts = scopes.counts();
        let pixels = width * height;
        // Every pixel at level 128 on the waveform, spread over its columns.
        let wave: u32 = (0..WAVE_COLUMNS)
            .map(|column| counts[(WAVE_AT + 128 * WAVE_COLUMNS + column) as usize])
            .sum();
        assert_eq!(wave, pixels);
        // All in the vectorscope's middle cell: grey has no chroma.
        let middle = VECTOR_AT + (VECTOR_SIDE / 2) * VECTOR_SIDE + VECTOR_SIDE / 2;
        assert_eq!(counts[middle as usize], pixels);
        // The histogram: every channel and luma in bin 128.
        for channel in 0..4 {
            assert_eq!(counts[(HIST_AT + channel * LEVELS + 128) as usize], pixels);
            assert_eq!(counts[(PEAK_AT + channel) as usize], pixels);
        }
    }

    #[test]
    fn red_lands_on_the_red_side_and_the_parade_splits_the_channels() {
        let Some((device, queue)) = device() else {
            return;
        };
        let (width, height) = (32, 16);
        let red = frame(&device, &queue, width, height, |_, _| [255, 0, 0, 255]);
        let mut scopes = Scopes::new(&device, &queue);
        scopes.draw(&red, VECTORSCOPE | PARADE);
        let counts = scopes.counts();
        let pixels = width * height;
        // The parade: red at the top level, green and blue at the bottom.
        let row = 3 * PARADE_COLUMNS;
        let sum = |level: u32, channel: u32| -> u32 {
            (0..PARADE_COLUMNS)
                .map(|column| {
                    counts[(PARADE_AT + level * row + channel * PARADE_COLUMNS + column) as usize]
                })
                .sum()
        };
        assert_eq!(sum(255, 0), pixels);
        assert_eq!(sum(0, 1), pixels);
        assert_eq!(sum(0, 2), pixels);
        // The vectorscope: Cr is high and Cb a little low, so red sits up
        // and to the left of the middle.
        let cell = (VECTOR_AT..HIST_AT)
            .find(|&at| counts[at as usize] == pixels)
            .expect("one cell holds the frame")
            - VECTOR_AT;
        let (x, y) = (cell % VECTOR_SIDE, cell / VECTOR_SIDE);
        assert!(x < VECTOR_SIDE / 2, "cb is negative: {x}");
        assert!(y < VECTOR_SIDE / 4, "cr is high: {y}");
        // Nothing asked for, nothing counted.
        let wave: u32 = counts[WAVE_AT as usize..PARADE_AT as usize].iter().sum();
        assert_eq!(wave, 0);
    }
}
