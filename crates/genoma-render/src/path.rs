//! Vector paths drawn on the GPU (docs/svg-plan.md): an outline tessellated
//! into triangles once, kept by key, and drawn in whatever colour, alpha,
//! clip and depth each frame asks, with no rasterising.
//!
//! Everything is a fill: a stroke reaches here already turned into the
//! outline it covers. Antialiasing is a fringe: the filled region's
//! boundary (the edges that belong to one triangle only, after the fill
//! rule is resolved) is moved half a pixel in, and a strip one pixel wide
//! is laid along it whose coverage runs from 1 inside to 0 outside. The
//! two never overlap, so a translucent fill stays even, and the edge's
//! coverage falls across one pixel as a box filter's does.

use std::{collections::HashMap, sync::Arc};

use bytemuck::{Pod, Zeroable};
use lyon_tessellation::{BuffersBuilder, FillOptions, FillRule, FillTessellator, FillVertex, VertexBuffers, path::Path as Outline};

pub use lyon_tessellation::path;

/// A mesh's vertex: physical pixels from the item's origin, and how much
/// of the pixel it covers.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct PathVertex {
    pub position: [f32; 2],
    pub coverage: f32,
}

/// A tessellated outline, physical pixels.
#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub vertices: Vec<PathVertex>,
    pub indices: Vec<u32>,
    /// x0, y0, x1, y1 of what it covers, fringe included.
    pub bounds: [f32; 4],
}

/// A filled outline in the draw list.
#[derive(Clone)]
pub struct Path {
    /// The same key is the same mesh: uploaded once, kept while drawn.
    pub key: u64,
    pub mesh: Arc<Mesh>,
    /// Where the mesh's origin lands, physical pixels.
    pub origin: [f32; 2],
    /// Straight sRGB-encoded RGBA, as everything else.
    pub color: [f32; 4],
    pub alpha: f32,
    /// Drawn only inside this rectangle, physical pixels: x0, y0, x1, y1.
    pub clip: [f32; 4],
    /// Faded toward a clip's foot, as a rect's (see [`crate::Rect::fade`]).
    pub fade: [f32; 4],
    /// Depth, as a rect's (see [`crate::Rect::transform`]).
    pub transform: [[f32; 4]; 4],
}

/// `outline` filled by `rule` and tessellated within `tolerance` physical
/// pixels, with its antialiasing fringe. The outline is in physical pixels.
pub fn fill(outline: &Outline, rule: FillRule, tolerance: f32) -> Mesh {
    let mut buffers: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
    let options = FillOptions::tolerance(tolerance.max(0.01)).with_fill_rule(rule);
    let built = FillTessellator::new().tessellate_path(outline, &options, &mut BuffersBuilder::new(&mut buffers, |v: FillVertex| v.position().to_array()));
    if let Err(e) = built {
        log::debug!("path: {e:?}");
        return Mesh::default();
    }
    fringed(&buffers.vertices, &buffers.indices)
}

fn sub(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

fn dot(a: [f32; 2], b: [f32; 2]) -> f32 {
    a[0] * b[0] + a[1] * b[1]
}

fn normalised(v: [f32; 2]) -> [f32; 2] {
    let l = dot(v, v).sqrt();
    if l > 1e-6 { [v[0] / l, v[1] / l] } else { [0.0, 0.0] }
}

/// Triangles (shared corners merged) with the fringe along their boundary.
fn fringed(positions: &[[f32; 2]], indices: &[u32]) -> Mesh {
    // Corners at the same place are one corner: the boundary is found by
    // edges, which must name the same ends.
    let mut merged: HashMap<(u32, u32), u32> = HashMap::new();
    let mut points: Vec<[f32; 2]> = Vec::new();
    let remap: Vec<u32> = positions
        .iter()
        .map(|p| {
            *merged.entry((p[0].to_bits(), p[1].to_bits())).or_insert_with(|| {
                points.push(*p);
                points.len() as u32 - 1
            })
        })
        .collect();
    let mut triangles: Vec<[u32; 3]> = Vec::with_capacity(indices.len() / 3);
    for t in indices.chunks_exact(3) {
        let [a, b, c] = [remap[t[0] as usize], remap[t[1] as usize], remap[t[2] as usize]];
        let (pa, pb, pc) = (points[a as usize], points[b as usize], points[c as usize]);
        let area = (pb[0] - pa[0]) * (pc[1] - pa[1]) - (pc[0] - pa[0]) * (pb[1] - pa[1]);
        if a != b && b != c && a != c && area.abs() > 1e-7 {
            triangles.push([a, b, c]);
        }
    }
    // Each edge's triangles: one is the boundary, with the third corner on
    // its inner side.
    let mut edges: HashMap<(u32, u32), (u32, u32)> = HashMap::new();
    for t in &triangles {
        for (a, b, c) in [(t[0], t[1], t[2]), (t[1], t[2], t[0]), (t[2], t[0], t[1])] {
            let e = edges.entry((a.min(b), a.max(b))).or_insert((0, c));
            e.0 += 1;
            e.1 = c;
        }
    }
    // An edge with one triangle is the boundary only if just outside it
    // nothing is filled: where a vertex splits the edge on one side only
    // (a T-junction, as a stroke's inner loops leave), it lies inside.
    let grid = Grid::new(&points, &triangles);
    let mut boundary: Vec<(u32, u32, [f32; 2])> = Vec::new();
    let mut normals: HashMap<u32, Vec<[f32; 2]>> = HashMap::new();
    for (&(a, b), &(count, c)) in &edges {
        if count != 1 {
            continue;
        }
        let (pa, pb, pc) = (points[a as usize], points[b as usize], points[c as usize]);
        let d = sub(pb, pa);
        let mut n = normalised([-d[1], d[0]]);
        if dot(n, sub(pc, pa)) > 0.0 {
            n = [-n[0], -n[1]];
        }
        let probe = [(pa[0] + pb[0]) * 0.5 + n[0] * 1e-3, (pa[1] + pb[1]) * 0.5 + n[1] * 1e-3];
        if grid.covers(&points, &triangles, probe) {
            continue;
        }
        boundary.push((a, b, n));
        normals.entry(a).or_default().push(n);
        normals.entry(b).or_default().push(n);
    }
    // A corner's offset: along the mean of its edges' normals, long enough
    // that both edges move half a pixel (a miter), within a limit.
    let offset: HashMap<u32, [f32; 2]> = normals
        .iter()
        .map(|(&v, ns)| {
            let sum = ns.iter().fold([0.0, 0.0], |s, n| [s[0] + n[0], s[1] + n[1]]);
            let m = normalised(sum);
            let m = if m == [0.0, 0.0] { ns[0] } else { m };
            let cos = ns.iter().map(|n| dot(*n, m)).fold(1.0f32, f32::min).max(0.25);
            (v, [m[0] / cos * 0.5, m[1] / cos * 0.5])
        })
        .collect();
    let mut vertices: Vec<PathVertex> = points
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let o = offset.get(&(i as u32)).copied().unwrap_or([0.0, 0.0]);
            PathVertex { position: [p[0] - o[0], p[1] - o[1]], coverage: 1.0 }
        })
        .collect();
    let mut indices: Vec<u32> = triangles.iter().flatten().copied().collect();
    // Each boundary corner once more, outside, uncovered.
    let mut outer: HashMap<u32, u32> = HashMap::new();
    for (&v, o) in &offset {
        let p = points[v as usize];
        vertices.push(PathVertex { position: [p[0] + o[0], p[1] + o[1]], coverage: 0.0 });
        outer.insert(v, vertices.len() as u32 - 1);
    }
    for (a, b, _) in &boundary {
        let (ao, bo) = (outer[a], outer[b]);
        indices.extend_from_slice(&[*a, *b, bo, *a, bo, ao]);
    }
    let bounds = vertices.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, v| {
        [b[0].min(v.position[0]), b[1].min(v.position[1]), b[2].max(v.position[0]), b[3].max(v.position[1])]
    });
    Mesh { vertices, indices, bounds: if bounds[0] > bounds[2] { [0.0; 4] } else { bounds } }
}

/// Triangles by the cells of a grid over them, for a point to find the
/// few it may lie in.
struct Grid {
    origin: [f32; 2],
    cell: f32,
    size: [usize; 2],
    cells: Vec<Vec<u32>>,
}

impl Grid {
    fn new(points: &[[f32; 2]], triangles: &[[u32; 3]]) -> Grid {
        let b = points.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, p| [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])]);
        if triangles.is_empty() || b[0] > b[2] {
            return Grid { origin: [0.0; 2], cell: 1.0, size: [0, 0], cells: Vec::new() };
        }
        let side = ((triangles.len() as f32).sqrt().ceil() as usize).clamp(1, 128);
        let cell = ((b[2] - b[0]).max(b[3] - b[1]) / side as f32).max(1e-3);
        let size = [((b[2] - b[0]) / cell) as usize + 1, ((b[3] - b[1]) / cell) as usize + 1];
        let mut cells = vec![Vec::new(); size[0] * size[1]];
        for (i, t) in triangles.iter().enumerate() {
            let ps = t.map(|v| points[v as usize]);
            let lo = [ps.iter().map(|p| p[0]).fold(f32::MAX, f32::min), ps.iter().map(|p| p[1]).fold(f32::MAX, f32::min)];
            let hi = [ps.iter().map(|p| p[0]).fold(f32::MIN, f32::max), ps.iter().map(|p| p[1]).fold(f32::MIN, f32::max)];
            let (x0, y0) = (((lo[0] - b[0]) / cell) as usize, ((lo[1] - b[1]) / cell) as usize);
            let (x1, y1) = ((((hi[0] - b[0]) / cell) as usize).min(size[0] - 1), (((hi[1] - b[1]) / cell) as usize).min(size[1] - 1));
            for y in y0..=y1 {
                for x in x0..=x1 {
                    cells[y * size[0] + x].push(i as u32);
                }
            }
        }
        Grid { origin: [b[0], b[1]], cell, size, cells }
    }

    /// Whether `p` lies strictly inside one of the triangles.
    fn covers(&self, points: &[[f32; 2]], triangles: &[[u32; 3]], p: [f32; 2]) -> bool {
        let (fx, fy) = ((p[0] - self.origin[0]) / self.cell, (p[1] - self.origin[1]) / self.cell);
        if fx < 0.0 || fy < 0.0 || fx as usize >= self.size[0] || fy as usize >= self.size[1] {
            return false;
        }
        self.cells[fy as usize * self.size[0] + fx as usize].iter().any(|&i| {
            let [a, b, c] = triangles[i as usize].map(|v| points[v as usize]);
            let side = |u: [f32; 2], v: [f32; 2]| (v[0] - u[0]) * (p[1] - u[1]) - (v[1] - u[1]) * (p[0] - u[0]);
            let (s1, s2, s3) = (side(a, b), side(b, c), side(c, a));
            (s1 > 0.0 && s2 > 0.0 && s3 > 0.0) || (s1 < 0.0 && s2 < 0.0 && s3 < 0.0)
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Instance {
    /// Origin, then alpha and the fade's foot and span.
    origin: [f32; 4],
    fade: [f32; 4],
    color: [f32; 4],
    clip: [f32; 4],
    m: [[f32; 4]; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    viewport: [f32; 2],
    _pad: [f32; 2],
}

pub(crate) const SHADER: &str = r#"
struct Globals { viewport: vec2<f32>, _pad: vec2<f32> };
@group(0) @binding(0) var<uniform> globals: Globals;

struct Varyings {
    @builtin(position) position: vec4<f32>,
    @location(0) coverage: f32,
    @location(1) color: vec4<f32>,
    @location(2) clip: vec4<f32>,
    @location(3) fade: vec3<f32>,
    // The pixel before the matrix, where the clip and the fade are measured.
    @location(4) unturned: vec2<f32>,
};

@vertex
fn vs(@location(0) position: vec2<f32>, @location(1) coverage: f32,
      @location(2) origin: vec4<f32>, @location(3) fade: vec4<f32>, @location(4) color: vec4<f32>, @location(5) clip: vec4<f32>,
      @location(6) m0: vec4<f32>, @location(7) m1: vec4<f32>, @location(8) m2: vec4<f32>, @location(9) m3: vec4<f32>) -> Varyings {
    let pixel = origin.xy + position;
    let p = vec4<f32>(pixel, 0.0, 1.0);
    let q = vec4<f32>(dot(m0, p), dot(m1, p), dot(m2, p), dot(m3, p));
    var out: Varyings;
    out.position = vec4<f32>((q.xy / q.w) / globals.viewport * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0) * vec4<f32>(q.w, q.w, 1.0, q.w);
    out.coverage = coverage;
    out.color = color;
    out.clip = clip;
    out.fade = vec3<f32>(origin.z, fade.x, fade.y);
    out.unturned = pixel;
    return out;
}

@fragment
fn fs(in: Varyings) -> @location(0) vec4<f32> {
    let p = in.unturned;
    if (p.x < in.clip.x || p.y < in.clip.y || p.x >= in.clip.z || p.y >= in.clip.w) {
        discard;
    }
    var cover = clamp(in.coverage, 0.0, 1.0) * in.fade.x;
    if (in.fade.z > 0.0) {
        cover *= clamp((in.fade.y - p.y) / in.fade.z, 0.0, 1.0);
    }
    let a = in.color.a * cover;
    return vec4<f32>(in.color.rgb * a, a);
}
"#;

struct GpuMesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
    /// The upload that last drew it.
    used: u64,
}

pub(crate) struct Pipeline {
    pipeline: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    globals_group: wgpu::BindGroup,
    last_viewport: [f32; 2],
    instances: wgpu::Buffer,
    capacity: usize,
    meshes: HashMap<u64, GpuMesh>,
    /// The meshes of the list uploaded last, in order.
    keys: Vec<u64>,
    uploads: u64,
}

/// Meshes kept past this many go once no list has drawn them for a while.
const MESHES_KEPT: usize = 512;
const UNUSED_UPLOADS: u64 = 600;

impl Pipeline {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Pipeline {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("path"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("path globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("path globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("path globals"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("path"),
            bind_group_layouts: &[Some(&globals_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("path"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[
                    Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<PathVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32],
                    }),
                    Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Instance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![2 => Float32x4, 3 => Float32x4, 4 => Float32x4, 5 => Float32x4,
                            6 => Float32x4, 7 => Float32x4, 8 => Float32x4, 9 => Float32x4],
                    }),
                ],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, ..Default::default() },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        Pipeline {
            pipeline,
            globals,
            globals_group,
            last_viewport: [0.0; 2],
            instances: Self::instance_buffer(device, 16),
            capacity: 16,
            meshes: HashMap::new(),
            keys: Vec::new(),
            uploads: 0,
        }
    }

    fn instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("path instances"),
            size: (capacity * std::mem::size_of::<Instance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    pub(crate) fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, viewport: [f32; 2], paths: &[Path]) {
        use wgpu::util::DeviceExt;
        self.uploads += 1;
        self.keys.clear();
        if paths.is_empty() {
            return;
        }
        if viewport != self.last_viewport {
            queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&Globals { viewport, _pad: [0.0; 2] }));
            self.last_viewport = viewport;
        }
        if paths.len() > self.capacity {
            self.capacity = paths.len().next_power_of_two();
            self.instances = Self::instance_buffer(device, self.capacity);
        }
        let mut instances = Vec::with_capacity(paths.len());
        for p in paths {
            let mesh = self.meshes.entry(p.key).or_insert_with(|| {
                // An empty mesh still gets buffers: drawing it draws nothing.
                let vertices: &[PathVertex] = if p.mesh.vertices.is_empty() { &[PathVertex { position: [0.0; 2], coverage: 0.0 }] } else { &p.mesh.vertices };
                let indices: &[u32] = if p.mesh.indices.is_empty() { &[0, 0, 0] } else { &p.mesh.indices };
                GpuMesh {
                    vertices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("path vertices"),
                        contents: bytemuck::cast_slice(vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    }),
                    indices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("path indices"),
                        contents: bytemuck::cast_slice(indices),
                        usage: wgpu::BufferUsages::INDEX,
                    }),
                    count: if p.mesh.indices.is_empty() { 0 } else { p.mesh.indices.len() as u32 },
                    used: 0,
                }
            });
            mesh.used = self.uploads;
            self.keys.push(p.key);
            instances.push(Instance {
                origin: [p.origin[0], p.origin[1], p.alpha, 0.0],
                fade: [p.fade[0], p.fade[1], 0.0, 0.0],
                color: p.color,
                clip: p.clip,
                m: p.transform,
            });
        }
        queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&instances));
        if self.meshes.len() > MESHES_KEPT {
            let now = self.uploads;
            self.meshes.retain(|_, m| now - m.used < UNUSED_UPLOADS);
        }
    }

    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, only: std::ops::Range<usize>) {
        if only.is_empty() || self.keys.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.globals_group, &[]);
        pass.set_vertex_buffer(1, self.instances.slice(..));
        for i in only {
            let Some(mesh) = self.keys.get(i).and_then(|k| self.meshes.get(k)) else { continue };
            if mesh.count == 0 {
                continue;
            }
            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
            pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..mesh.count, 0, i as u32..i as u32 + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lyon_tessellation::math::point;

    fn square(x: f32, y: f32, s: f32) -> Outline {
        let mut b = Outline::builder();
        b.begin(point(x, y));
        b.line_to(point(x + s, y));
        b.line_to(point(x + s, y + s));
        b.line_to(point(x, y + s));
        b.end(true);
        b.build()
    }

    /// Coverage at a point: the covered triangle's interpolation, as the GPU
    /// does it.
    fn coverage_at(mesh: &Mesh, p: [f32; 2]) -> f32 {
        let mut total = 0.0f32;
        for t in mesh.indices.chunks_exact(3) {
            let [a, b, c] = [mesh.vertices[t[0] as usize], mesh.vertices[t[1] as usize], mesh.vertices[t[2] as usize]];
            let (pa, pb, pc) = (a.position, b.position, c.position);
            let det = (pb[1] - pc[1]) * (pa[0] - pc[0]) + (pc[0] - pb[0]) * (pa[1] - pc[1]);
            if det.abs() < 1e-9 {
                continue;
            }
            let l1 = ((pb[1] - pc[1]) * (p[0] - pc[0]) + (pc[0] - pb[0]) * (p[1] - pc[1])) / det;
            let l2 = ((pc[1] - pa[1]) * (p[0] - pc[0]) + (pa[0] - pc[0]) * (p[1] - pc[1])) / det;
            let l3 = 1.0 - l1 - l2;
            if l1 >= -1e-5 && l2 >= -1e-5 && l3 >= -1e-5 {
                total = total.max(l1 * a.coverage + l2 * b.coverage + l3 * c.coverage);
            }
        }
        total
    }

    #[test]
    fn a_square_is_covered_inside_and_fades_across_its_edge() {
        let mesh = fill(&square(10.0, 10.0, 20.0), FillRule::NonZero, 0.1);
        assert_eq!(coverage_at(&mesh, [20.0, 20.0]), 1.0);
        assert!((coverage_at(&mesh, [10.0, 20.0]) - 0.5).abs() < 1e-4, "half on the edge");
        assert!((coverage_at(&mesh, [9.75, 20.0]) - 0.25).abs() < 1e-4);
        assert_eq!(coverage_at(&mesh, [9.4, 20.0]), 0.0, "nothing past half a pixel out");
        assert_eq!(mesh.bounds, [9.5, 9.5, 30.5, 30.5]);
    }

    #[test]
    fn even_odd_leaves_a_hole() {
        let mut b = Outline::builder();
        for (x, y, s) in [(0.0, 0.0, 30.0), (10.0, 10.0, 10.0)] {
            b.begin(point(x, y));
            b.line_to(point(x + s, y));
            b.line_to(point(x + s, y + s));
            b.line_to(point(x, y + s));
            b.end(true);
        }
        let outline = b.build();
        let odd = fill(&outline, FillRule::EvenOdd, 0.1);
        assert_eq!(coverage_at(&odd, [15.0, 15.0]), 0.0);
        assert!((coverage_at(&odd, [10.0, 15.0]) - 0.5).abs() < 1e-4, "the hole's edge is antialiased too");
        assert_eq!(coverage_at(&odd, [5.0, 5.0]), 1.0);
        let nonzero = fill(&outline, FillRule::NonZero, 0.1);
        assert_eq!(coverage_at(&nonzero, [15.0, 15.0]), 1.0, "same winding: no hole");
    }

    #[test]
    fn overlapping_contours_have_no_inner_fringe() {
        let mut b = Outline::builder();
        for (x, y) in [(0.0, 0.0), (10.0, 0.0)] {
            b.begin(point(x, y));
            b.line_to(point(x + 20.0, y));
            b.line_to(point(x + 20.0, y + 20.0));
            b.line_to(point(x, y + 20.0));
            b.end(true);
        }
        let mesh = fill(&b.build(), FillRule::NonZero, 0.1);
        assert_eq!(coverage_at(&mesh, [10.0, 10.0]), 1.0, "where one square's edge lies inside the other");
        assert_eq!(coverage_at(&mesh, [20.0, 10.0]), 1.0);
    }
}
