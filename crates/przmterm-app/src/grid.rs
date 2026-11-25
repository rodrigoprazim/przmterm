//! The terminal grid drawn with wgpu: cell backgrounds, underlines and
//! glyphs from the [`Atlas`] as instanced quads, one draw call per pane,
//! instead of laying text out with egui every frame.
//!
//! Characters the atlas leaves to egui (colour emoji, characters only a
//! system font has) are still painted by egui, over the grid. With no wgpu
//! (or `PRZMTERM_RENDERER=egui`) the whole grid is painted by egui as
//! before.

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32};
use eframe::egui_wgpu::{self, wgpu};

use crate::atlas::{Atlas, Slot};

static ON: AtomicBool = AtomicBool::new(false);
static ATLAS: LazyLock<Mutex<Atlas>> = LazyLock::new(Mutex::default);

/// The glyph atlas every pane shares.
pub fn atlas() -> MutexGuard<'static, Atlas> {
    ATLAS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether panes draw their grid with wgpu.
pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

/// What is drawn: a quad with a solid colour, or a glyph from the atlas.
pub const GLYPH: u32 = 1;
/// The top leans right, as egui draws italics.
pub const ITALIC: u32 = 2;

/// One quad, in physical pixels from the window's top left.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Quad {
    pub pos: [f32; 2],
    pub size: [f32; 2],
    /// The glyph's top left in the atlas, in pixels.
    pub uv: [f32; 2],
    /// Premultiplied RGBA, red in the low byte.
    pub color: u32,
    pub flags: u32,
}

impl Quad {
    pub fn solid(pos: [f32; 2], size: [f32; 2], color: Color32) -> Self {
        Self {
            pos,
            size,
            uv: [0.0; 2],
            color: u32::from_le_bytes(color.to_array()),
            flags: 0,
        }
    }

    pub fn glyph(pos: [f32; 2], slot: Slot, color: Color32, italic: bool) -> Self {
        Self {
            pos,
            size: [slot.w as f32, slot.h as f32],
            uv: [slot.x as f32, slot.y as f32],
            color: u32::from_le_bytes(color.to_array()),
            flags: GLYPH | if italic { ITALIC } else { 0 },
        }
    }
}

/// Sets up the renderer for the windows eframe opens, unless
/// `PRZMTERM_RENDERER=egui` asks for egui's painting.
pub fn install(state: &egui_wgpu::RenderState) {
    if std::env::var("PRZMTERM_RENDERER").is_ok_and(|v| v.eq_ignore_ascii_case("egui")) {
        tracing::info!("terminal grid painted by egui (PRZMTERM_RENDERER=egui)");
        return;
    }
    let renderer = Renderer::new(&state.device, state.target_format);
    state.renderer.write().callback_resources.insert(renderer);
    ON.store(true, Ordering::Relaxed);
}

/// The shape that draws `quads` for the pane `id` inside `rect`.
pub fn paint(rect: egui::Rect, id: u64, quads: Vec<Quad>) -> egui::Shape {
    egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(
        rect,
        Paint { id, quads },
    ))
}

struct Paint {
    id: u64,
    quads: Vec<Quad>,
}

/// The screen size, for turning pixels into clip space.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Locals {
    screen: [f32; 2],
    /// The target converts to sRGB itself: give it linear colours.
    linear: u32,
    pad: u32,
}

struct AtlasTexture {
    bind: wgpu::BindGroup,
    texture: wgpu::Texture,
    side: u32,
    generation: u64,
}

struct PaneBuffers {
    quads: wgpu::Buffer,
    capacity: usize,
    count: u32,
    locals: wgpu::Buffer,
    bind: wgpu::BindGroup,
    used: Instant,
}

struct Renderer {
    pipeline: wgpu::RenderPipeline,
    atlas_layout: wgpu::BindGroupLayout,
    locals_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    linear: bool,
    atlas: Option<AtlasTexture>,
    panes: HashMap<u64, PaneBuffers>,
}

/// Buffers of panes not drawn for this long are dropped.
const UNUSED: Duration = Duration::from_secs(30);

const SHADER: &str = r#"
struct Locals {
    screen: vec2<f32>,
    linear: u32,
    pad: u32,
};

@group(0) @binding(0) var atlas: texture_2d<f32>;
@group(0) @binding(1) var atlas_sampler: sampler;
@group(1) @binding(0) var<uniform> locals: Locals;

struct Quad {
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: u32,
    @location(4) flags: u32,
};

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) flags: u32,
};

@vertex
fn vs_main(@builtin(vertex_index) corner: u32, quad: Quad) -> Out {
    let at = vec2<f32>(f32(corner & 1u), f32(corner >> 1u));
    var p = quad.pos + at * quad.size;
    if (quad.flags & 2u) != 0u {
        p.x += (1.0 - at.y) * quad.size.y * 0.25;
    }
    var out: Out;
    out.position = vec4<f32>(
        p.x / locals.screen.x * 2.0 - 1.0,
        1.0 - p.y / locals.screen.y * 2.0,
        0.0,
        1.0,
    );
    out.uv = (quad.uv + at * quad.size) / vec2<f32>(textureDimensions(atlas));
    out.color = unpack4x8unorm(quad.color);
    out.flags = quad.flags;
    return out;
}

fn linear_from_gamma(srgb: vec3<f32>) -> vec3<f32> {
    let cutoff = srgb < vec3<f32>(0.04045);
    let lower = srgb / vec3<f32>(12.92);
    let higher = pow((srgb + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(higher, lower, cutoff);
}

@fragment
fn fs_main(in: Out) -> @location(0) vec4<f32> {
    var coverage = 1.0;
    if (in.flags & 1u) != 0u {
        coverage = textureSampleLevel(atlas, atlas_sampler, in.uv, 0.0).r;
    }
    var color = in.color * coverage;
    if locals.linear == 1u {
        // As egui does for an sRGB target.
        color = vec4<f32>(linear_from_gamma(color.rgb), color.a);
    }
    return color;
}
"#;

impl Renderer {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("przmterm_grid"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("przmterm_grid_atlas"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let locals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("przmterm_grid_locals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(std::mem::size_of::<Locals>() as u64),
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("przmterm_grid"),
            bind_group_layouts: &[Some(&atlas_layout), Some(&locals_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("przmterm_grid"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Quad>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Uint32, 4 => Uint32
                    ],
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            // As egui's own pipeline: no depth, no multisampling.
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::OneMinusDstAlpha,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("przmterm_grid_atlas"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline,
            atlas_layout,
            locals_layout,
            sampler,
            linear: format.is_srgb(),
            atlas: None,
            panes: HashMap::new(),
        }
    }

    /// Copies what changed in the atlas to the texture (all of it when the
    /// atlas was resized or cleared).
    fn sync_atlas(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let mut atlas = atlas();
        let upload = atlas.upload();
        let stale = self
            .atlas
            .as_ref()
            .is_none_or(|a| a.side != upload.side || a.generation != upload.generation);
        let rows = if stale {
            let size = wgpu::Extent3d {
                width: upload.side,
                height: upload.side,
                depth_or_array_layers: 1,
            };
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("przmterm_grid_atlas"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("przmterm_grid_atlas"),
                layout: &self.atlas_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.atlas = Some(AtlasTexture {
                bind,
                texture,
                side: upload.side,
                generation: upload.generation,
            });
            Some((0, upload.side))
        } else {
            upload.rows
        };
        let (Some((from, to)), Some(target)) = (rows, &self.atlas) else {
            return;
        };
        let side = upload.side;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &target.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: from,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &upload.pixels[(from * side) as usize..(to * side) as usize],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(side),
                rows_per_image: Some(to - from),
            },
            wgpu::Extent3d {
                width: side,
                height: to - from,
                depth_or_array_layers: 1,
            },
        );
    }

    fn pane_buffers(&mut self, device: &wgpu::Device, id: u64, quads: usize) -> &mut PaneBuffers {
        let fits = self.panes.get(&id).is_some_and(|p| p.capacity >= quads);
        if !fits {
            let capacity = quads.next_power_of_two().max(1024);
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("przmterm_grid_quads"),
                size: (capacity * std::mem::size_of::<Quad>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            match self.panes.get_mut(&id) {
                Some(pane) => {
                    pane.quads = buffer;
                    pane.capacity = capacity;
                }
                None => {
                    let locals = device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("przmterm_grid_locals"),
                        size: std::mem::size_of::<Locals>() as u64,
                        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    });
                    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("przmterm_grid_locals"),
                        layout: &self.locals_layout,
                        entries: &[wgpu::BindGroupEntry {
                            binding: 0,
                            resource: locals.as_entire_binding(),
                        }],
                    });
                    self.panes.insert(
                        id,
                        PaneBuffers {
                            quads: buffer,
                            capacity,
                            count: 0,
                            locals,
                            bind,
                            used: Instant::now(),
                        },
                    );
                }
            }
        }
        self.panes.get_mut(&id).expect("just made")
    }
}

impl egui_wgpu::CallbackTrait for Paint {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(renderer) = resources.get_mut::<Renderer>() else {
            return Vec::new();
        };
        renderer.sync_atlas(device, queue);
        renderer.panes.retain(|_, p| p.used.elapsed() < UNUSED);
        let linear = u32::from(renderer.linear);
        let pane = renderer.pane_buffers(device, self.id, self.quads.len());
        queue.write_buffer(&pane.quads, 0, bytemuck::cast_slice(&self.quads));
        let [w, h] = screen.size_in_pixels;
        let locals = Locals {
            screen: [w as f32, h as f32],
            linear,
            pad: 0,
        };
        queue.write_buffer(&pane.locals, 0, bytemuck::bytes_of(&locals));
        pane.count = self.quads.len() as u32;
        pane.used = Instant::now();
        Vec::new()
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let Some(renderer) = resources.get::<Renderer>() else {
            return;
        };
        let (Some(atlas), Some(pane)) = (&renderer.atlas, renderer.panes.get(&self.id)) else {
            return;
        };
        if pane.count == 0 {
            return;
        }
        // Quads are placed on the whole window, not the callback's rect.
        let [w, h] = info.screen_size_px;
        pass.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
        pass.set_pipeline(&renderer.pipeline);
        pass.set_bind_group(0, &atlas.bind, &[]);
        pass.set_bind_group(1, &pane.bind, &[]);
        pass.set_vertex_buffer(0, pane.quads.slice(..));
        pass.draw(0..4, 0..pane.count);
    }
}
