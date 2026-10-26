use crate::transport::Frame;
use anyhow::{Context, Result};
use egui_wgpu::{Renderer, ScreenDescriptor};
use std::sync::Arc;
use winit::{dpi::PhysicalSize, window::Window};

const VIDEO_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const MIP_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

fn video_sampler(nearest: bool) -> wgpu::SamplerDescriptor<'static> {
    wgpu::SamplerDescriptor {
        label: Some("Video trilinear minification"),
        mag_filter: if nearest {
            wgpu::FilterMode::Nearest
        } else {
            wgpu::FilterMode::Linear
        },
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    }
}

struct MipGenerator {
    pipeline: wgpu::RenderPipeline,
    source_layout: wgpu::BindGroupLayout,
}

impl MipGenerator {
    fn new(device: &wgpu::Device) -> Self {
        let source_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Video mip source layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Video mip pipeline layout"),
            bind_group_layouts: &[&source_layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("graphics_mipmap.wgsl"));
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Linear-light video mip downsampling"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: MIP_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        Self {
            pipeline,
            source_layout,
        }
    }

    // All views/bindings are cached. Each dirty source is processed exactly once,
    // in the same command encoder as the subsequent egui and screenshot passes.
    fn generate(&self, encoder: &mut wgpu::CommandEncoder, image: &mut VideoTexture) -> bool {
        if !image.dirty {
            return false;
        }
        for (source, target) in image.mip_sources.iter().zip(&image.mip_views[1..]) {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Video mip level"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, source, &[]);
            pass.draw(0..3, 0..1);
        }
        image.dirty = false;
        true
    }
}

struct VideoTexture {
    texture: wgpu::Texture,
    display_view: wgpu::TextureView,
    dimensions: [u32; 2],
    mip_views: Vec<wgpu::TextureView>,
    mip_sources: Vec<wgpu::BindGroup>,
    dirty: bool,
}

impl VideoTexture {
    fn new(device: &wgpu::Device, generator: &MipGenerator, dimensions: [u32; 2]) -> Self {
        let size = wgpu::Extent3d {
            width: dimensions[0],
            height: dimensions[1],
            depth_or_array_layers: 1,
        };
        let mip_level_count = size.max_mips(wgpu::TextureDimension::D2);
        let mut usage = wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::RENDER_ATTACHMENT;
        // Production never reads back source video. Module tests inspect each mip.
        if cfg!(test) {
            usage |= wgpu::TextureUsages::COPY_SRC;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Latest active RGBA frame and mip chain"),
            size,
            mip_level_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: VIDEO_FORMAT,
            usage,
            view_formats: &[MIP_FORMAT],
        });
        // egui expects gamma-encoded UNORM values, not an sRGB-decoding view.
        // Leave mip 0 untouched, including its exact native-size colors.
        let display_view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("Video full-chain egui UNORM view"),
            format: Some(VIDEO_FORMAT),
            ..Default::default()
        });
        let mip_views: Vec<_> = (0..mip_level_count)
            .map(|level| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("Video single-level linear-light view"),
                    format: Some(MIP_FORMAT),
                    base_mip_level: level,
                    mip_level_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let mip_sources = mip_views[..mip_views.len() - 1]
            .iter()
            .map(|view| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Video cached mip source"),
                    layout: &generator.source_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(view),
                    }],
                })
            })
            .collect();
        Self {
            texture,
            display_view,
            dimensions,
            mip_views,
            mip_sources,
            dirty: false,
        }
    }

    fn upload(&mut self, queue: &wgpu::Queue, rgba: &[u8]) {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.dimensions[0] * 4),
                rows_per_image: Some(self.dimensions[1]),
            },
            wgpu::Extent3d {
                width: self.dimensions[0],
                height: self.dimensions[1],
                depth_or_array_layers: 1,
            },
        );
        self.dirty = true;
    }
}

struct Video {
    image: VideoTexture,
    ids: [egui::TextureId; 2],
}

impl Video {
    fn new(
        device: &wgpu::Device,
        renderer: &mut Renderer,
        generator: &MipGenerator,
        dimensions: [u32; 2],
    ) -> Self {
        let image = VideoTexture::new(device, generator, dimensions);
        // egui only accepts sampler descriptors, not existing sampler/bindgroup
        // handles. Register both once so toggling MAG never allocates GPU objects.
        let ids = std::array::from_fn(|index| {
            renderer.register_native_texture_with_sampler_options(
                device,
                &image.display_view,
                video_sampler(index == 1),
            )
        });
        Self { image, ids }
    }

    fn select_sampler(&self, jobs: &mut [egui::ClippedPrimitive], nearest: bool) {
        // Keep the public texture ID stable, even when set_nearest is called
        // after egui built this frame's shapes. Both passes consume these jobs.
        for job in jobs {
            if let egui::epaint::Primitive::Mesh(mesh) = &mut job.primitive
                && mesh.texture_id == self.ids[0]
            {
                mesh.texture_id = self.ids[usize::from(nearest)];
            }
        }
    }

    fn free(self, renderer: &mut Renderer) {
        for id in self.ids {
            renderer.free_texture(&id);
        }
    }
}

fn render_ui(
    renderer: &Renderer,
    encoder: &mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    jobs: &[egui::ClippedPrimitive],
    descriptor: &ScreenDescriptor,
) {
    let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("Viewer and controls"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color {
                    r: 0.025,
                    g: 0.03,
                    b: 0.04,
                    a: 1.0,
                }),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
    });
    renderer.render(&mut pass.forget_lifetime(), jobs, descriptor);
}

pub struct Graphics {
    // Surface owns an Arc<Window>; no unsafe lifetime extension.
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    mip_generator: MipGenerator,
    video: Option<Video>,
    nearest: bool,
}

impl Graphics {
    pub async fn new(window: Arc<Window>) -> Result<Self> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance
            .create_surface(window)
            .context("Could not create window surface")?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .context("No compatible graphics adapter found")?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Crash Cart viewer"),
                ..Default::default()
            })
            .await
            .context("Could not initialize graphics device")?;
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|format| !format.is_srgb())
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);
        let renderer = Renderer::new(&device, format, None, 1, false);
        let mip_generator = MipGenerator::new(&device);
        Ok(Self {
            surface,
            device,
            queue,
            config,
            renderer,
            mip_generator,
            video: None,
            nearest: false,
        })
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width > 0 && size.height > 0 {
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    pub fn upload(&mut self, frame: Frame) {
        let dimensions = [frame.width, frame.height];
        if self
            .video
            .as_ref()
            .is_none_or(|video| video.image.dimensions != dimensions)
        {
            self.clear_video();
            self.video = Some(Video::new(
                &self.device,
                &mut self.renderer,
                &self.mip_generator,
                dimensions,
            ));
        }
        if let Some(video) = &mut self.video {
            video.image.upload(&self.queue, &frame.rgba);
        }
    }

    pub fn clear_video(&mut self) {
        if let Some(video) = self.video.take() {
            video.free(&mut self.renderer);
        }
    }
    pub fn texture_id(&self) -> Option<egui::TextureId> {
        self.video.as_ref().map(|video| video.ids[0])
    }
    pub fn set_nearest(&mut self, nearest: bool) {
        // Enlargement preference only; MIN and MIP remain linear in both samplers.
        self.nearest = nearest;
    }

    pub fn draw(
        &mut self,
        context: &egui::Context,
        output: egui::FullOutput,
        screenshot: Option<&std::path::Path>,
    ) -> Result<bool> {
        if screenshot.is_some()
            && !matches!(
                self.config.format,
                wgpu::TextureFormat::Bgra8Unorm
                    | wgpu::TextureFormat::Bgra8UnormSrgb
                    | wgpu::TextureFormat::Rgba8Unorm
                    | wgpu::TextureFormat::Rgba8UnormSrgb
            )
        {
            anyhow::bail!("Screenshot requires an RGBA8 or BGRA8 surface");
        }
        let descriptor = ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point: output.pixels_per_point,
        };
        for (id, delta) in &output.textures_delta.set {
            self.renderer
                .update_texture(&self.device, &self.queue, *id, delta);
        }
        let mut jobs = context.tessellate(output.shapes, output.pixels_per_point);
        if let Some(video) = &self.video {
            video.select_sampler(&mut jobs, self.nearest);
        }
        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.device, &self.config);
                for id in &output.textures_delta.free {
                    self.renderer.free_texture(id);
                }
                return Ok(false);
            }
            Err(wgpu::SurfaceError::Timeout | wgpu::SurfaceError::Other) => {
                for id in &output.textures_delta.free {
                    self.renderer.free_texture(id);
                }
                return Ok(false);
            }
            Err(error) => return Err(error.into()),
        };
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Viewer frame"),
            });
        // Surface acquisition can skip a frame. Only clear dirty after encoding
        // work that will be submitted, before either consumer of the mip chain.
        if let Some(video) = &mut self.video {
            self.mip_generator.generate(&mut encoder, &mut video.image);
        }
        let buffers = self.renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &jobs,
            &descriptor,
        );
        render_ui(&self.renderer, &mut encoder, &view, &jobs, &descriptor);
        // An explicit smoke-test screenshot is rendered into an owned GPU
        // texture. No desktop pixels or other application windows are captured.
        let capture = screenshot.map(|_| {
            let size = wgpu::Extent3d {
                width: self.config.width,
                height: self.config.height,
                depth_or_array_layers: 1,
            };
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Explicit UI screenshot"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.config.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let capture_view = texture.create_view(&Default::default());
            render_ui(
                &self.renderer,
                &mut encoder,
                &capture_view,
                &jobs,
                &descriptor,
            );
            let padded_row = (self.config.width * 4).div_ceil(256) * 256;
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Bounded screenshot readback"),
                size: u64::from(padded_row) * u64::from(self.config.height),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(padded_row),
                        rows_per_image: Some(self.config.height),
                    },
                },
                size,
            );
            (buffer, padded_row)
        });
        self.queue
            .submit(buffers.into_iter().chain([encoder.finish()]));
        frame.present();
        if let (Some(path), Some((buffer, padded_row))) = (screenshot, capture) {
            self.save_screenshot(path, &buffer, padded_row)?;
        }
        for id in &output.textures_delta.free {
            self.renderer.free_texture(id);
        }
        Ok(screenshot.is_some())
    }

    fn save_screenshot(
        &self,
        path: &std::path::Path,
        buffer: &wgpu::Buffer,
        padded_row: u32,
    ) -> Result<()> {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            self.device.poll(wgpu::PollType::Poll)?;
            match receiver.try_recv() {
                Ok(result) => {
                    result?;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty)
                    if std::time::Instant::now() < deadline =>
                {
                    std::thread::yield_now()
                }
                _ => anyhow::bail!("GPU screenshot readback did not complete within 3 seconds"),
            }
        }
        let mapping = buffer.slice(..).get_mapped_range();
        let mut rgba = Vec::with_capacity((self.config.width * self.config.height * 4) as usize);
        for row in mapping.chunks_exact(padded_row as usize) {
            rgba.extend_from_slice(&row[..self.config.width as usize * 4]);
        }
        if matches!(
            self.config.format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        ) {
            for pixel in rgba.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
        } else if !matches!(
            self.config.format,
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb
        ) {
            anyhow::bail!(
                "Unsupported screenshot pixel format {:?}",
                self.config.format
            );
        }
        drop(mapping);
        buffer.unmap();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .context("Could not create a new screenshot output")?;
        let mut encoder = png::Encoder::new(
            std::io::BufWriter::new(file),
            self.config.width,
            self.config.height,
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&rgba)?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "graphics_tests.rs"]
mod tests;
