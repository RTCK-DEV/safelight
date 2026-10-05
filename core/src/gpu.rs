//! GPU development pipeline (wgpu/WGSL compute).
//! Mirrors the math in develop.rs: demosaic -> chroma NR -> sharpen/clarity ->
//! straighten/flip/fit-resize -> tone/color adjust -> grain/vignette -> gamma.
//! Intermediates are vec4<f32> storage buffers; the last pass writes packed
//! rgba8. The CPU path remains as fallback (ARA_DISABLE_GPU or no adapter).
use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};

use crate::decode::Mosaic;
use crate::develop::{build_params, Params, RgbaImage};
use crate::recipe::{Recipe, WbMode};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uni {
    black: [f32; 4],
    norm: [f32; 4],
    wb: [f32; 4],
    m0: [f32; 4],
    m1: [f32; 4],
    m2: [f32; 4],
    /// exposure_mul, contrast, highlights, shadows
    a1: [f32; 4],
    /// whites, blacks, saturation, vibrance
    a2: [f32; 4],
    /// sharpen, noise_luma, clarity, vignette
    a3: [f32; 4],
    /// grain, rot_sin, rot_cos, unused
    a4: [f32; 4],
    /// raw_w, raw_h, left, top
    g0: [u32; 4],
    /// vis_w, vis_h, stride, flip_mode
    g1: [u32; 4],
    /// src_w, src_h (demosaiced), dst_w, dst_h
    g2: [u32; 4],
    /// cfa_w, cfa_h, samp_step, fw (post-flip frame width)
    g3: [u32; 4],
    /// fh (post-flip frame height), sensor visible w, sensor visible h
    g4: [u32; 4],
}

const WGSL: &str = r#"
struct Uni {
    black: vec4<f32>,
    norm: vec4<f32>,
    wb: vec4<f32>,
    m0: vec4<f32>,
    m1: vec4<f32>,
    m2: vec4<f32>,
    a1: vec4<f32>,
    a2: vec4<f32>,
    a3: vec4<f32>,
    a4: vec4<f32>,
    g0: vec4<u32>,
    g1: vec4<u32>,
    g2: vec4<u32>,
    g3: vec4<u32>,
    g4: vec4<u32>,
}
@group(0) @binding(0) var<uniform> u: Uni;
@group(0) @binding(1) var<storage, read> rawbuf: array<u32>;
@group(0) @binding(2) var<storage, read> cfa: array<u32>;
@group(0) @binding(3) var<storage, read_write> io_a: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> io_b: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> outb: array<u32>;
@group(0) @binding(6) var<storage, read> lut: array<f32>;
@group(0) @binding(7) var<storage, read_write> stats: array<atomic<u32>>;

fn cfa_col(sx: u32, sy: u32) -> u32 {
    return cfa[(sy % u.g3.y) * u.g3.x + (sx % u.g3.x)];
}

fn demosaic(vx: u32, vy: u32) -> vec3<f32> {
    let x0 = u.g0.z + vx * u.g1.z;
    let y0 = u.g0.w + vy * u.g1.z;
    let x_end = u.g0.z + u.g4.y;
    let y_end = u.g0.w + u.g4.z;
    var sums = vec3<f32>(0.0);
    var cnts = vec3<u32>(0u);
    var pad = 0u;
    for (var it = 0u; it < 6u; it = it + 1u) {
        sums = vec3<f32>(0.0);
        cnts = vec3<u32>(0u);
        var ys = y0;
        if (ys >= pad) { ys = ys - pad; } else { ys = 0u; }
        ys = max(ys, u.g0.w);
        var xs = x0;
        if (xs >= pad) { xs = xs - pad; } else { xs = 0u; }
        xs = max(xs, u.g0.z);
        let ye = min(y0 + u.g1.z + pad, y_end);
        let xe = min(x0 + u.g1.z + pad, x_end);
        for (var sy = ys; sy < ye; sy = sy + 1u) {
            let row = sy * u.g0.x;
            for (var sx = xs; sx < xe; sx = sx + 1u) {
                let col = cfa_col(sx, sy);
                let ci = min(col, 3u);
                let cc = select(ci, 1u, col == 3u);
                let v = (f32(rawbuf[row + sx]) - u.black[ci]) * u.norm[ci];
                sums[cc] = sums[cc] + v;
                cnts[cc] = cnts[cc] + 1u;
            }
        }
        if (cnts.x > 0u && cnts.y > 0u && cnts.z > 0u) { break; }
        pad = pad + 2u;
    }
    return sums / vec3<f32>(max(cnts, vec3<u32>(1u)));
}

fn wb_matrix(cam: vec3<f32>) -> vec3<f32> {
    let cw = clamp(cam * u.wb.xyz, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec3<f32>(
        dot(u.m0.xyz, cw),
        dot(u.m1.xyz, cw),
        dot(u.m2.xyz, cw),
    );
}

// x-dispatch is capped at 65535 workgroups; gid.y carries the overflow rows.
fn flat_index(gid: vec3<u32>, num: vec3<u32>) -> u32 {
    return gid.y * (num.x * 256u) + gid.x;
}

// sparse gray-world statistics for auto WB (sums * 1e6 as u32 + count)
@compute @workgroup_size(256)
fn stats_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) num: vec3<u32>) {
    let i = flat_index(gid, num);
    let step = u.g3.z;
    let nx = (u.g1.x + step - 1u) / step;
    let ny = (u.g1.y + step - 1u) / step;
    if (i >= nx * ny) { return; }
    let c = demosaic((i % nx) * step, (i / nx) * step);
    // u32 accumulators: scale x1000, capped at ~64K samples => sum <= ~2.6e8
    atomicAdd(&stats[0], u32(clamp(c.x, 0.0, 4.0) * 1000.0));
    atomicAdd(&stats[1], u32(clamp(c.y, 0.0, 4.0) * 1000.0));
    atomicAdd(&stats[2], u32(clamp(c.z, 0.0, 4.0) * 1000.0));
    atomicAdd(&stats[3], 1u);
}

@compute @workgroup_size(256)
fn demosaic_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) num: vec3<u32>) {
    let i = flat_index(gid, num);
    if (i >= u.g1.x * u.g1.y) { return; }
    let cam = demosaic(i % u.g1.x, i / u.g1.x);
    io_a[i] = vec4<f32>(wb_matrix(cam), 0.0);
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// chroma smoothing: blur R/B residuals vs luma
@compute @workgroup_size(256)
fn nr_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) num: vec3<u32>) {
    let i = flat_index(gid, num);
    let w = u.g2.x;
    let h = u.g2.y;
    if (i >= w * h) { return; }
    let x = i % w;
    let y = i / w;
    let c = io_a[i].xyz;
    let l = luma(c);
    let cr0 = c.x - l;
    let cb0 = c.z - l;
    var sr = 0.0;
    var sb = 0.0;
    var n = 0.0;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let nx = i32(x) + dx;
            let ny = i32(y) + dy;
            if (nx >= 0 && ny >= 0 && nx < i32(w) && ny < i32(h)) {
                let cc = io_a[u32(ny) * w + u32(nx)].xyz;
                let ll = luma(cc);
                sr = sr + cc.x - ll;
                sb = sb + cc.z - ll;
                n = n + 1.0;
            }
        }
    }
    let nr = mix(cr0, sr / n, u.a3.y);
    let nb = mix(cb0, sb / n, u.a3.y);
    io_b[i] = vec4<f32>(
        l + nr,
        l - 0.2126 / 0.7152 * nr - 0.0722 / 0.7152 * nb,
        l + nb,
        0.0,
    );
}

fn box_at(x: u32, y: u32, rad: i32) -> vec3<f32> {
    let w = u.g2.x;
    let h = u.g2.y;
    var s = vec3<f32>(0.0);
    var n = 0.0;
    for (var dy = -rad; dy <= rad; dy = dy + 1) {
        for (var dx = -rad; dx <= rad; dx = dx + 1) {
            let nx = i32(x) + dx;
            let ny = i32(y) + dy;
            if (nx >= 0 && ny >= 0 && nx < i32(w) && ny < i32(h)) {
                s = s + io_a[u32(ny) * w + u32(nx)].xyz;
                n = n + 1.0;
            }
        }
    }
    return s / n;
}

// unsharp (3x3) + clarity (5x5 midtone-weighted local contrast)
@compute @workgroup_size(256)
fn sharpen_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) num: vec3<u32>) {
    let i = flat_index(gid, num);
    let w = u.g2.x;
    let h = u.g2.y;
    if (i >= w * h) { return; }
    let src = io_a[i].xyz;
    var out = src;
    if (u.a3.x != 0.0) {
        out = src + u.a3.x * 0.8 * (src - box_at(i % w, i / w, 1));
    }
    if (u.a3.z != 0.0) {
        let lum = clamp(luma(src), 0.0, 1.0);
        let mid = 4.0 * lum * (1.0 - lum);
        out = out + u.a3.z * 0.6 * mid * (src - box_at(i % w, i / w, 2));
    }
    io_b[i] = vec4<f32>(out, 0.0);
}

fn sstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clamp((x - e0) / (e1 - e0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn srgb_encode(v: f32) -> f32 {
    let c = clamp(v, 0.0, 1.0);
    if (c <= 0.0031308) {
        return c * 12.92;
    }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

fn adjust(px: vec3<f32>) -> vec3<f32> {
    var x = px * u.a1.x;
    if (u.a2.y > 0.0) {
        x = x + u.a2.y * 0.15 * (vec3<f32>(1.0) - x);
    } else {
        x = x * (1.0 + u.a2.y * 0.20);
    }
    x = x * (1.0 + u.a2.x * 0.20);
    x = (x - vec3<f32>(0.18)) * (1.0 + u.a1.y * 0.9) + vec3<f32>(0.18);
    if (u.a1.w != 0.0) {
        let w = (vec3<f32>(1.0) - vec3<f32>(
            sstep(0.0, 0.45, x.x), sstep(0.0, 0.45, x.y), sstep(0.0, 0.45, x.z),
        )) * vec3<f32>(
            sstep(0.0, 0.06, x.x), sstep(0.0, 0.06, x.y), sstep(0.0, 0.06, x.z),
        );
        x = x + u.a1.w * w * 0.30;
    }
    if (u.a1.z != 0.0) {
        let w = vec3<f32>(
            sstep(0.45, 1.2, x.x), sstep(0.45, 1.2, x.y), sstep(0.45, 1.2, x.z),
        );
        x = x - u.a1.z * w * 0.5 * x;
    }
    if (u.a2.z != 0.0 || u.a2.w != 0.0) {
        let luma2 = luma(x);
        let mx = max(x.x, max(x.y, x.z));
        let mn = min(x.x, min(x.y, x.z));
        var sat_now = 0.0;
        if (mx > 1e-5) { sat_now = (mx - mn) / mx; }
        let s = max(1.0 + u.a2.z + u.a2.w * (1.0 - sat_now), 0.0);
        x = vec3<f32>(luma2) + (x - vec3<f32>(luma2)) * s;
    }
    x = vec3<f32>(
        lut[u32(clamp(x.x, 0.0, 1.0) * 255.0)],
        lut[u32(clamp(x.y, 0.0, 1.0) * 255.0)],
        lut[u32(clamp(x.z, 0.0, 1.0) * 255.0)],
    );
    return x;
}

fn hash(p: vec2<f32>, seed: f32) -> f32 {
    return fract(sin(dot(p, vec2<f32>(12.9898 + seed, 78.233 + seed * 1.7))) * 43758.5453);
}

// finish: fit-resize + straighten + flip undo + adjust + grain + vignette + gamma
@compute @workgroup_size(256)
fn finish_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) num: vec3<u32>) {
    let i = flat_index(gid, num);
    let dw = u.g2.z;
    let dh = u.g2.w;
    if (i >= dw * dh) { return; }
    let dx = i % dw;
    let dy = i / dw;
    let fw = u.g3.w;
    let fh = u.g4.x;
    // dst -> post-flip frame coords
    var fx = (f32(dx) + 0.5) * f32(fw) / f32(dw) - 0.5;
    var fy = (f32(dy) + 0.5) * f32(fh) / f32(dh) - 0.5;
    // undo straighten rotation about frame centre
    let cx = (f32(fw) - 1.0) * 0.5;
    let cy = (f32(fh) - 1.0) * 0.5;
    let px = fx - cx;
    let py = fy - cy;
    fx = cx + px * u.a4.z + py * u.a4.y;
    fy = cy - px * u.a4.y + py * u.a4.z;
    // undo flip -> src coords (src_w x src_h)
    var sx = fx;
    var sy = fy;
    let sw = u.g2.x;
    let sh = u.g2.y;
    switch u.g1.w {
        case 3u: {
            sx = f32(sw) - 1.0 - fx;
            sy = f32(sh) - 1.0 - fy;
        }
        case 6u: {
            sx = fy;
            sy = f32(sh) - 1.0 - fx;
        }
        case 5u: {
            sx = f32(sw) - 1.0 - fy;
            sy = fx;
        }
        default: {}
    }
    // bilinear sample (black outside)
    var col = vec3<f32>(0.0);
    let x0 = i32(floor(sx));
    let y0 = i32(floor(sy));
    if (x0 >= 0 && y0 >= 0 && x0 + 1 < i32(sw) && y0 + 1 < i32(sh)) {
        let tx = sx - floor(sx);
        let ty = sy - floor(sy);
        let i00 = u32(y0) * sw + u32(x0);
        let c00 = io_a[i00].xyz;
        let c10 = io_a[i00 + 1u].xyz;
        let c01 = io_a[i00 + sw].xyz;
        let c11 = io_a[i00 + sw + 1u].xyz;
        col = mix(mix(c00, c10, tx), mix(c01, c11, tx), ty);
    } else if (x0 >= 0 && y0 >= 0 && x0 < i32(sw) && y0 < i32(sh)) {
        col = io_a[u32(y0) * sw + u32(x0)].xyz;
    }
    var adj = adjust(col);
    // film grain (pre-gamma, linear domain)
    if (u.a4.x > 0.0) {
        let p = vec2<f32>(f32(dx), f32(dy));
        adj = adj + vec3<f32>(
            hash(p, 0.0) - 0.5,
            hash(p, 17.0) - 0.5,
            hash(p, 43.0) - 0.5,
        ) * u.a4.x * 0.12;
    }
    // vignette (post-adjust, pre-gamma; positive darkens corners)
    if (u.a3.w != 0.0) {
        let nx = (f32(dx) + 0.5) / f32(dw) * 2.0 - 1.0;
        let ny = (f32(dy) + 0.5) / f32(dh) * 2.0 - 1.0;
        let d = length(vec2<f32>(nx, ny)) * 0.7071;
        adj = adj * (1.0 - u.a3.w * sstep(0.35, 1.05, d) * 0.9);
    }
    let enc = srgb_encode(adj.x) * 255.0 + 0.5;
    let enc2 = srgb_encode(adj.y) * 255.0 + 0.5;
    let enc3 = srgb_encode(adj.z) * 255.0 + 0.5;
    outb[i] = min(u32(enc), 255u)
        | (min(u32(enc2), 255u) << 8u)
        | (min(u32(enc3), 255u) << 16u)
        | (255u << 24u);
}
"#;

struct Pipe {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    demosaic: Pipe,
    stats: Pipe,
    nr: Pipe,
    sharpen: Pipe,
    finish: Pipe,
}

fn norm_factors(m: &Mosaic) -> [f32; 4] {
    let mut n = [0.0f32; 4];
    for c in 0..4 {
        n[c] = 1.0 / (m.maximum as f32 - m.black[c]).max(1.0);
    }
    n
}

/// post-flip (oriented) frame dims
fn flipped_dims(w: u32, h: u32, flip: i32) -> (u32, u32) {
    match flip {
        5 | 6 => (h, w),
        _ => (w, h),
    }
}

fn fit(w: u32, h: u32, max_px: u32) -> (u32, u32) {
    if max_px == 0 || w.max(h) <= max_px {
        return (w.max(1), h.max(1));
    }
    let s = max_px as f32 / w.max(h) as f32;
    (
        ((w as f32 * s).round() as u32).max(1),
        ((h as f32 * s).round() as u32).max(1),
    )
}

impl Gpu {
    pub fn try_new() -> Option<Gpu> {
        if std::env::var_os("ARA_DISABLE_GPU").is_some() {
            log::info!("GPU disabled via ARA_DISABLE_GPU");
            return None;
        }
        match Self::init() {
            Ok(g) => Some(g),
            Err(e) => {
                log::warn!("GPU init failed, falling back to CPU: {e:#}");
                None
            }
        }
    }

    fn init() -> Result<Gpu> {
        let inst = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });
        let adapter = pollster::block_on(inst.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .context("no wgpu adapter")?;
        // full-res frames are large (vec4f per px); request the adapter's own
        // buffer limits rather than the 128MB default
        let mut limits = wgpu::Limits::default();
        let al = adapter.limits();
        limits.max_storage_buffer_binding_size = al.max_storage_buffer_binding_size;
        limits.max_buffer_size = al.max_buffer_size;
        limits.max_compute_invocations_per_workgroup = al.max_compute_invocations_per_workgroup;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("araware"),
            required_limits: limits,
            ..Default::default()
        }))
        .context("wgpu device request")?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("develop"),
            source: wgpu::ShaderSource::Wgsl(WGSL.into()),
        });
        let mk = |entry: &str| -> Pipe {
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(entry),
                entries: &(0..8)
                    .map(|i| wgpu::BindGroupLayoutEntry {
                        binding: i,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: match i {
                                0 => wgpu::BufferBindingType::Uniform,
                                3 | 4 | 5 | 7 => {
                                    wgpu::BufferBindingType::Storage { read_only: false }
                                }
                                _ => wgpu::BufferBindingType::Storage { read_only: true },
                            },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    })
                    .collect::<Vec<_>>(),
            });
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(entry),
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pl),
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            });
            Pipe { pipeline, layout }
        };
        Ok(Gpu {
            demosaic: mk("demosaic_main"),
            stats: mk("stats_main"),
            nr: mk("nr_main"),
            sharpen: mk("sharpen_main"),
            finish: mk("finish_main"),
            device,
            queue,
        })
    }

    /// mosaic -> developed rgba8 (fit inside max_px, 0 = full res)
    pub fn develop(&self, m: &Mosaic, r: &Recipe, max_px: u32) -> Result<RgbaImage> {
        use wgpu::BufferUsages as U;
        let dev = &self.device;
        let period = m.cfa.h.max(1) as u32;
        let mut stride = 1u32;
        if max_px > 0 {
            let ratio = (m.w.max(m.h) as f32 / max_px as f32).max(1.0);
            stride = ((ratio / period as f32).floor() as u32).max(1) * period;
        }
        let vw = (m.w as u32 / stride).max(1);
        let vh = (m.h as u32 / stride).max(1);
        let n_px = (vw * vh) as u64;

        // uploads
        let raw32: Vec<u32> = m.data.iter().map(|&v| v as u32).collect();
        let cfa32: Vec<u32> = m.cfa.cells.iter().map(|&v| v as u32).collect();
        let mk_buf = |label: &str, bytes: &[u8], usage: wgpu::BufferUsages| {
            let b = dev.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: bytes.len() as u64,
                usage,
                mapped_at_creation: false,
            });
            self.queue.write_buffer(&b, 0, bytes);
            b
        };
        let storage_in = U::STORAGE | U::COPY_DST;
        let raw_b = mk_buf("raw", bytemuck::cast_slice(&raw32), storage_in);
        let cfa_b = mk_buf("cfa", bytemuck::cast_slice(&cfa32), storage_in);
        let io_a = dev.create_buffer(&wgpu::BufferDescriptor {
            label: Some("io_a"),
            size: n_px * 16,
            usage: U::STORAGE | U::COPY_SRC | U::COPY_DST,
            mapped_at_creation: false,
        });
        // io_b is the scratch target for spatial passes; copied back into io_a after each.
        let io_b = dev.create_buffer(&wgpu::BufferDescriptor {
            label: Some("io_b"),
            size: n_px * 16,
            usage: U::STORAGE | U::COPY_SRC,
            mapped_at_creation: false,
        });
        let (fw, fh) = flipped_dims(vw, vh, m.info.flip);
        let (dw, dh) = fit(fw, fh, max_px);
        let out_b = dev.create_buffer(&wgpu::BufferDescriptor {
            label: Some("out"),
            size: (dw * dh) as u64 * 4,
            usage: U::STORAGE | U::COPY_SRC,
            mapped_at_creation: false,
        });
        let stats_b = dev.create_buffer(&wgpu::BufferDescriptor {
            label: Some("stats"),
            size: 16,
            usage: U::STORAGE | U::COPY_DST | U::COPY_SRC,
            mapped_at_creation: false,
        });
        let lut_b = dev.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lut"),
            size: 1024,
            usage: storage_in,
            mapped_at_creation: false,
        });
        let uni_b = dev.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uni"),
            size: std::mem::size_of::<Uni>() as u64,
            usage: U::UNIFORM | U::COPY_DST,
            mapped_at_creation: false,
        });

        let mk_uni = |p: &Params, samp_step: u32| Uni {
            black: m.black,
            norm: norm_factors(m),
            wb: [p.wb[0], p.wb[1], p.wb[2], 0.0],
            m0: [p.m[0][0], p.m[0][1], p.m[0][2], 0.0],
            m1: [p.m[1][0], p.m[1][1], p.m[1][2], 0.0],
            m2: [p.m[2][0], p.m[2][1], p.m[2][2], 0.0],
            a1: [p.exposure_mul, p.contrast, p.highlights, p.shadows],
            a2: [p.whites, p.blacks, p.saturation, p.vibrance],
            a3: [p.sharpen, p.noise_luma, p.clarity, p.vignette],
            a4: [
                p.grain,
                p.rotation_deg.to_radians().sin(),
                p.rotation_deg.to_radians().cos(),
                0.0,
            ],
            g0: [m.raw_w as u32, m.raw_h as u32, m.left as u32, m.top as u32],
            g1: [vw, vh, stride, m.info.flip as u32],
            g2: [vw, vh, dw, dh],
            g3: [m.cfa.w as u32, m.cfa.h as u32, samp_step, fw],
            g4: [fh, m.w as u32, m.h as u32, 0],
        };

        let bind = |pipe: &Pipe| {
            let entries: Vec<wgpu::BindGroupEntry> = [0, 1, 2, 3, 4, 5, 6, 7]
                .iter()
                .map(|&i| wgpu::BindGroupEntry {
                    binding: i,
                    resource: match i {
                        0 => uni_b.as_entire_binding(),
                        1 => raw_b.as_entire_binding(),
                        2 => cfa_b.as_entire_binding(),
                        3 => io_a.as_entire_binding(),
                        4 => io_b.as_entire_binding(),
                        5 => out_b.as_entire_binding(),
                        6 => lut_b.as_entire_binding(),
                        _ => stats_b.as_entire_binding(),
                    },
                })
                .collect();
            dev.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &pipe.layout,
                entries: &entries,
            })
        };
        let run = |enc: &mut wgpu::CommandEncoder, pipe: &Pipe, n: u64| {
            let bg = bind(pipe);
            let mut cp = enc.begin_compute_pass(&Default::default());
            cp.set_pipeline(&pipe.pipeline);
            cp.set_bind_group(0, &bg, &[]);
            let groups = n.div_ceil(256) as u32;
            let gx = groups.min(65535);
            cp.dispatch_workgroups(gx, groups.div_ceil(gx), 1);
        };

        // auto WB: sparse stats dispatch + readback
        let auto_means = if r.wb_mode == WbMode::Auto {
            // keep total samples under ~64K so the u32 accumulators can't overflow
            let step = ((n_px as f64 / 65536.0).sqrt().ceil() as u32).max(2);
            self.queue.write_buffer(&stats_b, 0, &[0u8; 16]);
            self.queue
                .write_buffer(&uni_b, 0, bytemuck::bytes_of(&mk_uni(&build_params(m, r, None), step)));
            let mut enc = dev.create_command_encoder(&Default::default());
            run(&mut enc, &self.stats, (vw.div_ceil(step) * vh.div_ceil(step)) as u64);
            let stg = dev.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stg_stats"),
                size: 16,
                usage: U::COPY_DST | U::MAP_READ,
                mapped_at_creation: false,
            });
            enc.copy_buffer_to_buffer(&stats_b, 0, &stg, 0, 16);
            self.queue.submit([enc.finish()]);
            let s = readback(dev, &stg, 16)?;
            let sums: &[u32] = bytemuck::cast_slice(&s);
            let cnt = sums[3].max(1) as f64;
            Some([
                (sums[0] as f64 / 1e3 / cnt) as f32,
                (sums[1] as f64 / 1e3 / cnt) as f32,
                (sums[2] as f64 / 1e3 / cnt) as f32,
            ])
        } else {
            None
        };

        let p = build_params(m, r, auto_means);
        self.queue.write_buffer(&lut_b, 0, bytemuck::cast_slice(&p.lut));
        self.queue
            .write_buffer(&uni_b, 0, bytemuck::bytes_of(&mk_uni(&p, 0)));

        let mut enc = dev.create_command_encoder(&Default::default());
        run(&mut enc, &self.demosaic, n_px);
        // Each spatial stage reads io_a and writes io_b; copy back between stages.
        if p.noise_luma > 0.0 {
            run(&mut enc, &self.nr, n_px);
            enc.copy_buffer_to_buffer(&io_b, 0, &io_a, 0, n_px * 16);
        }
        if p.sharpen > 0.0 || p.clarity != 0.0 {
            run(&mut enc, &self.sharpen, n_px);
            enc.copy_buffer_to_buffer(&io_b, 0, &io_a, 0, n_px * 16);
        }
        run(&mut enc, &self.finish, (dw * dh) as u64);
        let out_bytes = (dw * dh * 4) as u64;
        let stg = dev.create_buffer(&wgpu::BufferDescriptor {
            label: Some("stg_out"),
            size: out_bytes,
            usage: U::COPY_DST | U::MAP_READ,
            mapped_at_creation: false,
        });
        enc.copy_buffer_to_buffer(&out_b, 0, &stg, 0, out_bytes);
        self.queue.submit([enc.finish()]);
        let data = readback(dev, &stg, out_bytes as usize)?;
        Ok(RgbaImage {
            width: dw,
            height: dh,
            data,
        })
    }
}

fn readback(dev: &wgpu::Device, buf: &wgpu::Buffer, size: usize) -> Result<Vec<u8>> {
    let slice = buf.slice(..size as u64);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = dev.poll(wgpu::PollType::Wait);
    let data = slice.get_mapped_range().to_vec();
    buf.unmap();
    Ok(data)
}
