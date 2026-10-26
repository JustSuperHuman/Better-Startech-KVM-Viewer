use super::*;

struct TestGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    generator: MipGenerator,
}

impl TestGpu {
    fn new() -> Self {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .expect("Rendering tests require a headless wgpu adapter (no window or USB device)");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("Video mip readback tests"),
            ..Default::default()
        }))
        .expect("Could not create headless rendering test device");
        let generator = MipGenerator::new(&device);
        Self {
            device,
            queue,
            generator,
        }
    }

    fn generate(&self, image: &mut VideoTexture) {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        assert!(self.generator.generate(&mut encoder, image));
        // An unchanged redraw records no further passes and leaves caches alone.
        assert!(!self.generator.generate(&mut encoder, image));
        self.queue.submit([encoder.finish()]);
    }

    fn read(&self, texture: &wgpu::Texture, level: u32) -> Vec<u8> {
        let width = (texture.width() >> level).max(1);
        let height = (texture.height() >> level).max(1);
        let row = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Video test mip readback"),
            size: u64::from(row) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: level,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            self.device.poll(wgpu::PollType::Poll).unwrap();
            match receiver.try_recv() {
                Ok(result) => {
                    result.expect("GPU mip readback failed");
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty)
                    if std::time::Instant::now() < deadline =>
                {
                    std::thread::yield_now();
                }
                _ => panic!("GPU mip readback did not complete within 5 seconds"),
            }
        }
        let mapping = buffer.slice(..).get_mapped_range();
        let result = mapping
            .chunks_exact(row as usize)
            .flat_map(|row| row[..width as usize * 4].iter().copied())
            .collect();
        drop(mapping);
        buffer.unmap();
        result
    }
}

fn pixels(dimensions: [u32; 2], mut pixel: impl FnMut(u32, u32) -> [u8; 4]) -> Vec<u8> {
    let mut result = Vec::with_capacity((dimensions[0] * dimensions[1] * 4) as usize);
    for y in 0..dimensions[1] {
        for x in 0..dimensions[0] {
            result.extend_from_slice(&pixel(x, y));
        }
    }
    result
}

fn assert_close(actual: &[u8], expected: &[u8], tolerance: u8) {
    assert_eq!(actual.len(), expected.len());
    for (index, (&actual, &expected)) in actual.iter().zip(expected).enumerate() {
        assert!(
            actual.abs_diff(expected) <= tolerance,
            "byte {index}: {actual} differs from expected {expected} (tolerance {tolerance})"
        );
    }
}

fn decode_srgb(value: u8) -> f64 {
    let encoded = f64::from(value) / 255.0;
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

fn encode_srgb(value: f64) -> u8 {
    let encoded = if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round().clamp(0.0, 255.0) as u8
}

// Independent CPU area-integral reference, used only by readback tests.
fn reference_mip(source: &[u8], dimensions: [u32; 2]) -> (Vec<u8>, [u32; 2]) {
    let target = dimensions.map(|size| (size / 2).max(1));
    let footprint = [
        f64::from(dimensions[0]) / f64::from(target[0]),
        f64::from(dimensions[1]) / f64::from(target[1]),
    ];
    let result = pixels(target, |x, y| {
        let start = [f64::from(x) * footprint[0], f64::from(y) * footprint[1]];
        let end = [start[0] + footprint[0], start[1] + footprint[1]];
        let mut sum = [0.0; 4];
        for sy in start[1].floor() as u32..(end[1].ceil() as u32).min(dimensions[1]) {
            for sx in start[0].floor() as u32..(end[0].ceil() as u32).min(dimensions[0]) {
                let weight = (end[0].min(f64::from(sx + 1)) - start[0].max(f64::from(sx)))
                    * (end[1].min(f64::from(sy + 1)) - start[1].max(f64::from(sy)));
                let offset = ((sy * dimensions[0] + sx) * 4) as usize;
                for channel in 0..4 {
                    let value = if channel == 3 {
                        f64::from(source[offset + channel]) / 255.0
                    } else {
                        decode_srgb(source[offset + channel])
                    };
                    sum[channel] += value * weight / (footprint[0] * footprint[1]);
                }
            }
        }
        [
            encode_srgb(sum[0]),
            encode_srgb(sum[1]),
            encode_srgb(sum[2]),
            (sum[3] * 255.0).round() as u8,
        ]
    });
    (result, target)
}

#[test]
fn nearest_changes_only_magnification() {
    for nearest in [false, true] {
        let sampler = video_sampler(nearest);
        assert_eq!(sampler.min_filter, wgpu::FilterMode::Linear);
        assert_eq!(sampler.mipmap_filter, wgpu::FilterMode::Linear);
        assert_eq!(sampler.lod_min_clamp, 0.0);
        assert!(sampler.lod_max_clamp >= 16.0);
        assert_eq!(sampler.address_mode_u, wgpu::AddressMode::ClampToEdge);
        assert_eq!(sampler.address_mode_v, wgpu::AddressMode::ClampToEdge);
        assert_eq!(
            sampler.mag_filter,
            if nearest {
                wgpu::FilterMode::Nearest
            } else {
                wgpu::FilterMode::Linear
            }
        );
    }
}

#[test]
fn gpu_mips_preserve_constant_colors_and_native_bytes() {
    let gpu = TestGpu::new();
    for dimensions in [[1, 1], [1, 7], [7, 1], [13, 9]] {
        let mut image = VideoTexture::new(&gpu.device, &gpu.generator, dimensions);
        assert_eq!(
            image.mip_views.len(),
            image.texture.mip_level_count() as usize
        );
        assert_eq!(image.mip_sources.len() + 1, image.mip_views.len());
        assert_eq!(image.texture.format(), VIDEO_FORMAT);
        for color in [
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [17, 93, 201, 255],
            [255, 0, 64, 255],
        ] {
            let source = pixels(dimensions, |_, _| color);
            image.upload(&gpu.queue, &source);
            gpu.generate(&mut image);
            assert_eq!(gpu.read(&image.texture, 0), source);
            for level in 1..image.texture.mip_level_count() {
                let size = dimensions.map(|size| (size >> level).max(1));
                assert_close(
                    &gpu.read(&image.texture, level),
                    &pixels(size, |_, _| color),
                    1,
                );
            }
        }
    }
}

#[test]
fn gpu_checker_mips_average_in_linear_light_and_retain_odd_edges() {
    let gpu = TestGpu::new();
    for dimensions in [[8, 8], [7, 5], [1, 9], [9, 1], [3, 3], [11, 9]] {
        let mut image = VideoTexture::new(&gpu.device, &gpu.generator, dimensions);
        for inverted in [false, true] {
            let source = pixels(dimensions, |x, y| {
                let value = if ((x + y) % 2 == 0) != inverted {
                    255
                } else {
                    0
                };
                [value, value, value, 255]
            });
            let white_count = source
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|pixel| pixel[0] == 255)
                .count();
            image.upload(&gpu.queue, &source);
            gpu.generate(&mut image);
            assert_eq!(gpu.read(&image.texture, 0), source);
            let mut reference = source;
            let mut size = dimensions;
            for level in 1..image.texture.mip_level_count() {
                (reference, size) = reference_mip(&reference, size);
                assert_close(&gpu.read(&image.texture, level), &reference, 2);
            }
            let average =
                encode_srgb(white_count as f64 / f64::from(dimensions[0] * dimensions[1]));
            let last = gpu.read(&image.texture, image.texture.mip_level_count() - 1);
            assert_close(&last, &[average, average, average, 255], 2);
            // Half white should be about 188, not encoded-value average 128.
            assert!(last[0] > 170);
        }
    }
    let dimensions = [7, 5];
    let mut image = VideoTexture::new(&gpu.device, &gpu.generator, dimensions);
    let mut reference = pixels(dimensions, |x, y| {
        [
            if x == dimensions[0] - 1 { 255 } else { 0 },
            if (x + y) % 2 == 0 { 255 } else { 0 },
            if y == dimensions[1] - 1 { 255 } else { 0 },
            255,
        ]
    });
    image.upload(&gpu.queue, &reference);
    gpu.generate(&mut image);
    let mut size = dimensions;
    for level in 1..image.texture.mip_level_count() {
        (reference, size) = reference_mip(&reference, size);
        assert_close(&gpu.read(&image.texture, level), &reference, 2);
    }
}

#[test]
fn gpu_refresh_reuses_chain_and_clear_releases_both_sampler_bindings() {
    let gpu = TestGpu::new();
    let mut renderer = Renderer::new(&gpu.device, VIDEO_FORMAT, None, 1, false);
    for dimensions in [[7, 5], [3, 9], [1, 1]] {
        let mut video = Video::new(&gpu.device, &mut renderer, &gpu.generator, dimensions);
        let ids = video.ids;
        for color in [[255, 0, 0, 255], [0, 0, 255, 255]] {
            video
                .image
                .upload(&gpu.queue, &pixels(dimensions, |_, _| color));
            gpu.generate(&mut video.image);
            for level in 0..video.image.texture.mip_level_count() {
                let size = dimensions.map(|size| (size >> level).max(1));
                assert_eq!(
                    gpu.read(&video.image.texture, level),
                    pixels(size, |_, _| color)
                );
            }
            assert_eq!(video.ids, ids);
        }
        // Multiple uploads before a draw must generate only the most recent one.
        video
            .image
            .upload(&gpu.queue, &pixels(dimensions, |_, _| [255, 0, 0, 255]));
        video
            .image
            .upload(&gpu.queue, &pixels(dimensions, |_, _| [0, 255, 0, 255]));
        gpu.generate(&mut video.image);
        let level = video.image.texture.mip_level_count() - 1;
        assert_eq!(gpu.read(&video.image.texture, level), [0, 255, 0, 255]);
        video.free(&mut renderer);
        assert!(ids.iter().all(|id| renderer.texture(id).is_none()));
    }
}

fn render_video(
    gpu: &TestGpu,
    renderer: &mut Renderer,
    video: &mut Video,
    scale: f32,
    nearest: bool,
) -> Vec<u8> {
    let descriptor = ScreenDescriptor {
        size_in_pixels: [20, 20],
        pixels_per_point: 1.0,
    };
    let mut mesh = egui::Mesh::with_texture(video.ids[0]);
    mesh.add_rect_with_uv(
        egui::Rect::from_min_size(
            egui::pos2(1.0, 1.0),
            egui::vec2(
                video.image.dimensions[0] as f32 * scale,
                video.image.dimensions[1] as f32 * scale,
            ),
        ),
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
    let mut jobs = vec![egui::ClippedPrimitive {
        clip_rect: egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(20.0, 20.0)),
        primitive: egui::epaint::Primitive::Mesh(mesh),
    }];
    video.select_sampler(&mut jobs, nearest);
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    // Exercise the production order: upload, mips, buffers, UI, screenshot, submit.
    gpu.generator.generate(&mut encoder, &mut video.image);
    let buffers =
        renderer.update_buffers(&gpu.device, &gpu.queue, &mut encoder, &jobs, &descriptor);
    let targets: [_; 2] = std::array::from_fn(|_| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Headless UI / screenshot parity target"),
            size: wgpu::Extent3d {
                width: 20,
                height: 20,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: VIDEO_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    });
    for target in &targets {
        render_ui(
            renderer,
            &mut encoder,
            &target.create_view(&Default::default()),
            &jobs,
            &descriptor,
        );
    }
    gpu.queue
        .submit(buffers.into_iter().chain([encoder.finish()]));
    let rendered = gpu.read(&targets[0], 0);
    assert_eq!(rendered, gpu.read(&targets[1], 0));
    rendered
}

fn image_region(rendered: &[u8], size: u32) -> Vec<u8> {
    let mut result = Vec::new();
    for y in 1..=size {
        let offset = (y * 20 + 1) as usize * 4;
        result.extend_from_slice(&rendered[offset..offset + size as usize * 4]);
    }
    result
}

#[test]
fn gpu_egui_native_colors_trilinear_sampling_and_screenshot_parity() {
    let gpu = TestGpu::new();
    let mut renderer = Renderer::new(&gpu.device, VIDEO_FORMAT, None, 1, false);
    let mut video = Video::new(&gpu.device, &mut renderer, &gpu.generator, [8, 8]);
    let source = pixels([8, 8], |x, y| [(x * 31) as u8, (y * 31) as u8, 93, 255]);
    video.image.upload(&gpu.queue, &source);
    for nearest in [false, true] {
        let native = render_video(&gpu, &mut renderer, &mut video, 1.0, nearest);
        assert_eq!(image_region(&native, 8), source);
    }
    let checker = pixels([8, 8], |x, y| {
        let value = if (x + y) % 2 == 0 { 255 } else { 0 };
        [value, value, value, 255]
    });
    video.image.upload(&gpu.queue, &checker);
    let reduced = render_video(&gpu, &mut renderer, &mut video, 0.5, false);
    assert_close(
        &image_region(&reduced, 4),
        &pixels([4, 4], |_, _| [188, 188, 188, 255]),
        1,
    );
    for scale in [0.71, 0.5, 0.25, 0.63] {
        let linear = render_video(&gpu, &mut renderer, &mut video, scale, false);
        let nearest = render_video(&gpu, &mut renderer, &mut video, scale, true);
        assert_eq!(
            linear, nearest,
            "Nearest MAG changed minification at {scale}"
        );
    }
    let linear = render_video(&gpu, &mut renderer, &mut video, 2.0, false);
    let nearest = render_video(&gpu, &mut renderer, &mut video, 2.0, true);
    assert_ne!(linear, nearest, "Nearest MAG should affect enlargement");
    assert!(renderer.texture(&video.ids[0]).is_some());
    video.free(&mut renderer);
}
