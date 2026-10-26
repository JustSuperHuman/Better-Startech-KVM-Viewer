//! Minimal egui-on-wgpu presentation for the setup window.
use anyhow::{Context, Result};
use egui_wgpu::{Renderer, ScreenDescriptor};
use std::sync::Arc;
use winit::{dpi::PhysicalSize, window::Window};

pub struct Render {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
}

impl Render {
    pub async fn new(window: Arc<Window>) -> Result<Self> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance
            .create_surface(window)
            .context("Could not create the setup window surface")?;
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
                label: Some("Crash Cart setup"),
                ..Default::default()
            })
            .await
            .context("Could not initialize graphics")?;
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
        Ok(Self {
            surface,
            device,
            queue,
            config,
            renderer,
        })
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width > 0 && size.height > 0 {
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    pub fn draw(&mut self, context: &egui::Context, output: egui::FullOutput) -> Result<()> {
        for (id, delta) in &output.textures_delta.set {
            self.renderer
                .update_texture(&self.device, &self.queue, *id, delta);
        }
        let jobs = context.tessellate(output.shapes, output.pixels_per_point);
        let descriptor = ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point: output.pixels_per_point,
        };
        let frame = match self.surface.get_current_texture() {
            Ok(frame) => Some(frame),
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.device, &self.config);
                None
            }
            Err(wgpu::SurfaceError::Timeout | wgpu::SurfaceError::Other) => None,
            Err(error) => return Err(error.into()),
        };
        if let Some(frame) = frame {
            let view = frame.texture.create_view(&Default::default());
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Setup frame"),
                });
            let buffers = self.renderer.update_buffers(
                &self.device,
                &self.queue,
                &mut encoder,
                &jobs,
                &descriptor,
            );
            {
                let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Setup"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                self.renderer
                    .render(&mut pass.forget_lifetime(), &jobs, &descriptor);
            }
            self.queue
                .submit(buffers.into_iter().chain([encoder.finish()]));
            frame.present();
        }
        for id in &output.textures_delta.free {
            self.renderer.free_texture(id);
        }
        Ok(())
    }
}
