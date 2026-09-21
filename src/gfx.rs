//! The wgpu renderer.
//!
//! Three pipelines share one "globals" bind group:
//!
//! * voxels -- greedy-meshed quads, one draw call per model instance;
//! * lines  -- the ground grid, bounding box and axis gizmo;
//! * HUD    -- screen-space text quads.
//!
//! Colours reach the voxel shader as palette *indices*, looked up in a
//! 256-entry uniform. See `README.md` for why that beats baking colours into
//! vertices.

use crate::camera::OrbitCamera;
use crate::hud::{self, Anchor, HudLine, HudVertex};
use crate::loader::VoxScene;
use crate::mesh::{Mesh, Vertex};
use crate::overlay::{self, LineVertex, Overlays};
use crate::palette::srgb_to_linear;
use crate::{font, scene::Bounds};
use anyhow::{Context, Result, anyhow};
use glam::{Mat4, Vec3};
use std::num::NonZeroU64;
use std::path::Path;
use std::sync::Arc;
use wgpu::util::DeviceExt;

/// Which background the `T` key is currently showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Background {
    #[default]
    Dark,
    Light,
}

impl Background {
    pub fn toggled(self) -> Background {
        match self {
            Background::Dark => Background::Light,
            Background::Light => Background::Dark,
        }
    }

    /// Clear colour. Swapchain formats are sRGB, and wgpu takes clear values
    /// in linear light, so the sRGB values are converted here.
    ///
    /// `alpha` is zero for screenshots, which is what makes their background
    /// transparent.
    fn clear(self, alpha: f64) -> wgpu::Color {
        let srgb = match self {
            Background::Dark => [0.11f32, 0.115, 0.13],
            Background::Light => [0.82, 0.83, 0.86],
        };
        wgpu::Color {
            r: srgb_to_linear(srgb[0]) as f64,
            g: srgb_to_linear(srgb[1]) as f64,
            b: srgb_to_linear(srgb[2]) as f64,
            a: alpha,
        }
    }
}

/// Everything the renderer needs to know about one frame.
pub struct FrameParams<'a> {
    pub camera: &'a OrbitCamera,
    pub show_grid: bool,
    pub show_bbox: bool,
    pub show_axes: bool,
    pub ambient_occlusion: bool,
    pub background: Background,
    pub hud: &'a [HudLine],
    /// The file menu, anchored to the opposite corner. Empty when closed.
    pub menu: &'a [HudLine],
    /// Integer pixel scale for HUD text.
    pub hud_scale: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    light_dir: [f32; 4],
    params: [f32; 4],
    viewport: [f32; 4],
}

/// Direction the key light travels, in world space (Z up).
const LIGHT_DIR: Vec3 = Vec3::new(0.45, 0.75, -0.9);
const AMBIENT: f32 = 0.38;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// A buffer that reallocates when the data outgrows it.
struct GrowBuffer {
    buffer: wgpu::Buffer,
    capacity: u64,
    usage: wgpu::BufferUsages,
    label: &'static str,
}

impl GrowBuffer {
    fn new(device: &wgpu::Device, label: &'static str, usage: wgpu::BufferUsages) -> GrowBuffer {
        let capacity = 4096;
        GrowBuffer {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: capacity,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            capacity,
            usage: usage | wgpu::BufferUsages::COPY_DST,
            label,
        }
    }

    fn write(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        // Buffer sizes and write lengths both have to be multiples of four.
        let needed = data.len().next_multiple_of(4) as u64;
        if needed > self.capacity {
            self.capacity = needed.next_power_of_two();
            self.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(self.label),
                size: self.capacity,
                usage: self.usage,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&self.buffer, 0, data);
    }
}

/// Buffers for one model's mesh.
struct GpuMesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
}

/// One instance to draw: which model, and where its matrix sits in the
/// per-model uniform buffer.
struct GpuDraw {
    mesh: usize,
    uniform_offset: u32,
}

struct GpuScene {
    meshes: Vec<GpuMesh>,
    draws: Vec<GpuDraw>,
    model_bind_group: wgpu::BindGroup,
    /// Kept so the camera can re-frame without re-reading the file.
    bounds: Bounds,
    triangle_count: usize,
}

struct GpuOverlays {
    buffer: wgpu::Buffer,
    ranges: Overlays,
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    depth_view: wgpu::TextureView,

    globals_buffer: wgpu::Buffer,
    palette_buffer: wgpu::Buffer,
    globals_bind_group: wgpu::BindGroup,
    model_layout: wgpu::BindGroupLayout,
    font_bind_group: wgpu::BindGroup,

    voxel_pipeline: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    hud_pipeline: wgpu::RenderPipeline,

    scene: Option<GpuScene>,
    overlays: Option<GpuOverlays>,
    hud_vertices: GrowBuffer,
    hud_indices: GrowBuffer,
    hud_index_count: u32,

    /// Offset stride for the per-model dynamic uniform.
    model_stride: u32,
    pub adapter_name: String,
}

impl Renderer {
    /// Bring up a device and all three pipelines for `window`.
    pub fn new(window: Arc<winit::window::Window>) -> Result<Renderer> {
        let size = window.inner_size();
        let (width, height) = (size.width.max(1), size.height.max(1));

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            // PRIMARY is Vulkan, DX12 and Metal: Vulkan on Linux for both
            // Wayland and X11, and DX12 or Vulkan on Windows.
            backends: wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let surface = instance
            .create_surface(window)
            .context("could not create a rendering surface for the window")?;

        let adapter =
            pollster::block_on(
                instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::from_env()
                        .unwrap_or(wgpu::PowerPreference::HighPerformance),
                    compatible_surface: Some(&surface),
                    force_fallback_adapter: false,
                    ..Default::default()
                }),
            )
            .context("no graphics adapter could drive this window")?;
        let adapter_name = adapter.get_info().name;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("voxview device"),
            required_features: wgpu::Features::empty(),
            // Ask for exactly what this adapter offers: nothing here needs more
            // than the downlevel defaults, which integrated GPUs all satisfy.
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .context("could not open a graphics device")?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or_else(|| caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width,
            height,
            // Fifo is the one present mode every backend guarantees, and it
            // caps the loop at the display's refresh rate.
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);
        let depth_view = create_depth(&device, width, height);

        let globals_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let palette_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("palette"),
            size: 256 * 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals layout"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT, false, None),
                uniform_entry(1, wgpu::ShaderStages::VERTEX, false, None),
            ],
        });
        let globals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: &globals_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: globals_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: palette_buffer.as_entire_binding(),
                },
            ],
        });

        let model_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("model layout"),
            entries: &[uniform_entry(
                0,
                wgpu::ShaderStages::VERTEX,
                true,
                NonZeroU64::new(64),
            )],
        });

        let font_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("font layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });
        let font_bind_group = create_font(&device, &queue, &font_layout);

        let voxel_pipeline = create_voxel_pipeline(&device, format, &globals_layout, &model_layout);
        let line_pipeline = create_line_pipeline(&device, format, &globals_layout);
        let hud_pipeline = create_hud_pipeline(&device, format, &globals_layout, &font_layout);

        let model_stride = device.limits().min_uniform_buffer_offset_alignment.max(64);

        let hud_vertices = GrowBuffer::new(&device, "hud vertices", wgpu::BufferUsages::VERTEX);
        let hud_indices = GrowBuffer::new(&device, "hud indices", wgpu::BufferUsages::INDEX);

        Ok(Renderer {
            surface,
            device,
            queue,
            config,
            depth_view,
            globals_buffer,
            palette_buffer,
            globals_bind_group,
            model_layout,
            font_bind_group,
            voxel_pipeline,
            line_pipeline,
            hud_pipeline,
            scene: None,
            overlays: None,
            hud_vertices,
            hud_indices,
            hud_index_count: 0,
            model_stride,
            adapter_name,
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn aspect(&self) -> f32 {
        self.config.width as f32 / self.config.height.max(1) as f32
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.depth_view = create_depth(&self.device, width, height);
    }

    /// Reconfigure after a surface error; cheap enough to do on the spot.
    pub fn reconfigure(&mut self) {
        self.surface.configure(&self.device, &self.config);
    }

    /// Triangles currently uploaded, for the HUD.
    pub fn triangle_count(&self) -> usize {
        self.scene.as_ref().map_or(0, |s| s.triangle_count)
    }

    /// Replace the geometry on the GPU. Leaves the camera alone -- hot reload
    /// depends on that.
    pub fn set_scene(&mut self, scene: &VoxScene, meshes: &[Mesh]) {
        self.queue.write_buffer(
            &self.palette_buffer,
            0,
            bytemuck::cast_slice(&scene.palette.to_linear_rgba()),
        );

        let gpu_meshes: Vec<GpuMesh> = meshes
            .iter()
            .enumerate()
            .map(|(i, mesh)| GpuMesh {
                vertices: self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some(&format!("model {i} vertices")),
                        // An empty model still needs a non-empty buffer.
                        contents: non_empty(bytemuck::cast_slice(&mesh.vertices)),
                        usage: wgpu::BufferUsages::VERTEX,
                    }),
                indices: self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some(&format!("model {i} indices")),
                        contents: non_empty(bytemuck::cast_slice(&mesh.indices)),
                        usage: wgpu::BufferUsages::INDEX,
                    }),
                index_count: mesh.indices.len() as u32,
            })
            .collect();

        let stride = self.model_stride as usize;
        let mut uniform_bytes = vec![0u8; (scene.instances.len().max(1)) * stride];
        let mut draws = Vec::with_capacity(scene.instances.len());
        let mut triangle_count = 0;
        for (i, inst) in scene.instances.iter().enumerate() {
            let size = scene.models[inst.model_index].size().as_ivec3();
            let matrix: Mat4 = inst.transform.model_matrix(size);
            let offset = i * stride;
            uniform_bytes[offset..offset + 64]
                .copy_from_slice(bytemuck::cast_slice(&matrix.to_cols_array()));
            draws.push(GpuDraw {
                mesh: inst.model_index,
                uniform_offset: offset as u32,
            });
            triangle_count += meshes
                .get(inst.model_index)
                .map_or(0, |m| m.triangle_count());
        }

        let model_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("model transforms"),
                contents: &uniform_bytes,
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let model_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("model transforms"),
            layout: &self.model_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &model_buffer,
                    offset: 0,
                    size: NonZeroU64::new(64),
                }),
            }],
        });

        let ranges = overlay::build(&scene.bounds);
        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("overlay lines"),
                contents: non_empty(bytemuck::cast_slice(&ranges.vertices)),
                usage: wgpu::BufferUsages::VERTEX,
            });
        self.overlays = Some(GpuOverlays { buffer, ranges });

        self.scene = Some(GpuScene {
            meshes: gpu_meshes,
            draws,
            model_bind_group,
            bounds: scene.bounds,
            triangle_count,
        });
    }

    /// Draw one frame to the window.
    ///
    /// Surfaces go stale on their own -- a resize, a monitor change, a
    /// compositor restart -- so the recoverable cases are handled here rather
    /// than pushed onto the caller.
    pub fn render(&mut self, params: &FrameParams) {
        use wgpu::CurrentSurfaceTexture as Acquired;
        let frame = match self.surface.get_current_texture() {
            Acquired::Success(frame) => frame,
            // Still drawable this frame; reconfigure so the next one is clean.
            Acquired::Suboptimal(frame) => {
                self.reconfigure();
                frame
            }
            Acquired::Outdated | Acquired::Lost => {
                self.reconfigure();
                return;
            }
            // Nothing to do but skip the frame.
            Acquired::Timeout | Acquired::Occluded => return,
            Acquired::Validation => {
                eprintln!("voxview: the surface rejected a frame");
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        self.prepare(params);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        self.encode(&mut encoder, &view, params, true);
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
    }

    /// Render one frame into a PNG next to the model, with a transparent
    /// background and without the HUD or the debug overlays.
    pub fn screenshot(&mut self, params: &FrameParams, path: &Path) -> Result<()> {
        let (width, height) = (self.config.width, self.config.height);
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("screenshot"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Same format as the swapchain, so the pipelines can be reused.
            format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let clean = FrameParams {
            camera: params.camera,
            show_grid: false,
            show_bbox: false,
            show_axes: false,
            ambient_occlusion: params.ambient_occlusion,
            background: params.background,
            hud: &[],
            menu: &[],
            hud_scale: params.hud_scale,
        };
        self.prepare(&clean);

        // Copy destinations need rows padded to 256 bytes.
        let unpadded = width as usize * 4;
        let padded = unpadded.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize);
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("screenshot readback"),
            size: (padded * height as usize) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("screenshot"),
            });
        self.encode(&mut encoder, &view, &clean, false);
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded as u32),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));

        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| anyhow!("waiting for the GPU failed: {e}"))?;
        rx.recv()
            .map_err(|_| anyhow!("the screenshot readback was dropped"))?
            .map_err(|e| anyhow!("could not read the screenshot back: {e}"))?;

        let mut pixels = Vec::with_capacity(unpadded * height as usize);
        {
            let data = slice
                .get_mapped_range()
                .map_err(|e| anyhow!("could not map the screenshot buffer: {e}"))?;
            let swap_rb = matches!(
                self.config.format,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
            );
            for row in data.chunks_exact(padded).take(height as usize) {
                for px in row[..unpadded].as_chunks::<4>().0 {
                    if swap_rb {
                        pixels.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                    } else {
                        pixels.extend_from_slice(px);
                    }
                }
            }
        }
        readback.unmap();

        let image = image::RgbaImage::from_raw(width, height, pixels)
            .ok_or_else(|| anyhow!("screenshot pixel data was the wrong length"))?;
        image
            .save(path)
            .with_context(|| format!("could not write {}", path.display()))?;
        Ok(())
    }

    /// Upload the per-frame uniforms and HUD geometry.
    fn prepare(&mut self, params: &FrameParams) {
        let globals = Globals {
            view_proj: params
                .camera
                .view_projection(self.aspect())
                .to_cols_array_2d(),
            light_dir: LIGHT_DIR.normalize().extend(0.0).to_array(),
            params: [
                AMBIENT,
                if params.ambient_occlusion { 1.0 } else { 0.0 },
                0.0,
                0.0,
            ],
            viewport: [
                self.config.width as f32,
                self.config.height as f32,
                0.0,
                0.0,
            ],
        };
        self.queue
            .write_buffer(&self.globals_buffer, 0, bytemuck::bytes_of(&globals));

        let viewport = (self.config.width as f32, self.config.height as f32);
        let mut mesh = hud::layout(params.hud, params.hud_scale, Anchor::TopLeft, viewport);
        mesh.append(&hud::layout(
            params.menu,
            params.hud_scale,
            Anchor::TopRight,
            viewport,
        ));
        self.hud_index_count = mesh.indices.len() as u32;
        if !mesh.is_empty() {
            self.hud_vertices.write(
                &self.device,
                &self.queue,
                bytemuck::cast_slice(&mesh.vertices),
            );
            self.hud_indices.write(
                &self.device,
                &self.queue,
                bytemuck::cast_slice(&mesh.indices),
            );
        }
    }

    fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        color: &wgpu::TextureView,
        params: &FrameParams,
        opaque_background: bool,
    ) {
        let clear = params
            .background
            .clear(if opaque_background { 1.0 } else { 0.0 });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scene"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: color,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(clear),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        if let Some(scene) = &self.scene {
            pass.set_pipeline(&self.voxel_pipeline);
            pass.set_bind_group(0, &self.globals_bind_group, &[]);
            for draw in &scene.draws {
                let Some(mesh) = scene.meshes.get(draw.mesh) else {
                    continue;
                };
                if mesh.index_count == 0 {
                    continue;
                }
                pass.set_bind_group(1, &scene.model_bind_group, &[draw.uniform_offset]);
                pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.index_count, 0, 0..1);
            }
        }

        if let Some(overlays) = &self.overlays {
            let wanted = [
                (params.show_grid, overlays.ranges.grid.clone()),
                (params.show_bbox, overlays.ranges.bbox.clone()),
                (params.show_axes, overlays.ranges.axes.clone()),
            ];
            if wanted.iter().any(|(on, r)| *on && !r.is_empty()) {
                pass.set_pipeline(&self.line_pipeline);
                pass.set_bind_group(0, &self.globals_bind_group, &[]);
                pass.set_vertex_buffer(0, overlays.buffer.slice(..));
                for (on, range) in wanted {
                    if on && !range.is_empty() {
                        pass.draw(range, 0..1);
                    }
                }
            }
        }

        if self.hud_index_count > 0 {
            pass.set_pipeline(&self.hud_pipeline);
            pass.set_bind_group(0, &self.globals_bind_group, &[]);
            pass.set_bind_group(1, &self.font_bind_group, &[]);
            pass.set_vertex_buffer(0, self.hud_vertices.buffer.slice(..));
            pass.set_index_buffer(self.hud_indices.buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.hud_index_count, 0, 0..1);
        }
    }

    /// Bounds of the uploaded scene, for framing the camera.
    pub fn bounds(&self) -> Option<Bounds> {
        self.scene.as_ref().map(|s| s.bounds)
    }
}

/// `create_buffer_init` rejects zero-length contents; models can legitimately
/// be empty, so give them a single padding element instead.
fn non_empty(bytes: &[u8]) -> &[u8] {
    if bytes.is_empty() {
        &[0, 0, 0, 0]
    } else {
        bytes
    }
}

fn uniform_entry(
    binding: u32,
    visibility: wgpu::ShaderStages,
    has_dynamic_offset: bool,
    min_binding_size: Option<NonZeroU64>,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset,
            min_binding_size,
        },
        count: None,
    }
}

fn create_depth(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

fn create_font(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
) -> wgpu::BindGroup {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("font atlas"),
        size: wgpu::Extent3d {
            width: font::ATLAS_W,
            height: font::ATLAS_H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &font::atlas_pixels(),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(font::ATLAS_W),
            rows_per_image: Some(font::ATLAS_H),
        },
        wgpu::Extent3d {
            width: font::ATLAS_W,
            height: font::ATLAS_H,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    // Nearest sampling: the font is a bitmap and should stay sharp.
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("font sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("font"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    })
}

fn depth_state(write: bool, compare: wgpu::CompareFunction) -> Option<wgpu::DepthStencilState> {
    Some(wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: Some(write),
        depth_compare: Some(compare),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    })
}

fn create_voxel_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    globals: &wgpu::BindGroupLayout,
    model: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("voxel"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/voxel.wgsl").into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("voxel layout"),
        bind_group_layouts: &[Some(globals), Some(model)],
        // Immediate (push) constants are not used; everything is in uniforms.
        immediate_size: 0,
    });
    let attributes = wgpu::vertex_attr_array![0 => Float32x3, 1 => Uint32];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("voxel"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &attributes,
            })],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        },
        depth_stencil: depth_state(true, wgpu::CompareFunction::Less),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn create_line_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    globals: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("line"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/line.wgsl").into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("line layout"),
        bind_group_layouts: &[Some(globals)],
        // Immediate (push) constants are not used; everything is in uniforms.
        immediate_size: 0,
    });
    let attributes = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("line"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<LineVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &attributes,
            })],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::LineList,
            ..Default::default()
        },
        // Lines read depth so the model hides them, but do not write it.
        depth_stencil: depth_state(false, wgpu::CompareFunction::Less),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn create_hud_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    globals: &wgpu::BindGroupLayout,
    font_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("hud"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/hud.wgsl").into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("hud layout"),
        bind_group_layouts: &[Some(globals), Some(font_layout)],
        // Immediate (push) constants are not used; everything is in uniforms.
        immediate_size: 0,
    });
    let attributes = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("hud"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<HudVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &attributes,
            })],
        },
        primitive: wgpu::PrimitiveState::default(),
        // The HUD sits on top of everything, but the pass it joins has a
        // depth attachment, so the pipeline has to declare the same format.
        depth_stencil: depth_state(false, wgpu::CompareFunction::Always),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}
