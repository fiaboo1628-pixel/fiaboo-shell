//! Fullscreen shader background for the music view: one wlr layer surface drawn with wgpu.
//! fiaboo-shell starts it, writes "bass loud" lines (0..1) to stdin every frame and draws the
//! title on its own surface above it. Prints "ready" once the first frame is up; exits when
//! stdin closes, so it never outlives the shell.

use std::{
    io::BufRead,
    ptr::NonNull,
    sync::Mutex,
    time::Instant,
};

use raw_window_handle::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle};
use wayland_client::{
    delegate_noop,
    protocol::{wl_compositor, wl_registry, wl_surface},
    Connection, Dispatch, Proxy, QueueHandle,
};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

static AUDIO: Mutex<(f32, f32)> = Mutex::new((0., 0.)); // (bass, loud)

/// Uniforms for wave.wgsl: resolution, time, bass, then the 60 squares of the original shader laid
/// out for this frame (its per-pixel maths, moved here).
fn uniforms(res: (u32, u32), time: f32, bass: f32) -> Vec<f32> {
    const TOTAL: f32 = 60.;
    const MIN_SIZE: f32 = 0.03;
    const MAX_SIZE: f32 = 0.05;
    let mut u = vec![res.0 as f32, res.1 as f32, time, bass];
    let mut turns = Vec::with_capacity(60 * 4);
    for i in 0..60 {
        let index = i as f32 / TOTAL;
        let rnd = ((index * 12.9898 + index * 78.233).sin() * 43758.5453).rem_euclid(1.);
        let x = (-time / 8. * rnd + index).rem_euclid(1.) * 4. - 2.;
        let y = (index * rnd * 1000. + time / 10.).sin() * 0.5;
        let z = MAX_SIZE * rnd + MIN_SIZE;
        let (s, c) = (i as f32 + time / 2.).sin_cos();
        // where the square lands on screen: uv with rotate(uv - p + h) + p + h == p
        let h = z / 2.;
        let centre = (x - h - c * h + s * h, y - h - s * h - c * h);
        u.extend([x, y, z, 0.]);
        turns.push([c, s, centre.0, centre.1]);
    }
    // interleave: [2i] position, [2i + 1] turn
    let pos = u.split_off(4);
    for (p, t) in pos.chunks(4).zip(turns) {
        u.extend_from_slice(p);
        u.extend(t);
    }
    u
}

#[derive(Default)]
struct Wl {
    compositor: Option<wl_compositor::WlCompositor>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    size: Option<(u32, u32)>,
    closed: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Wl {
    fn event(s: &mut Self, reg: &wl_registry::WlRegistry, ev: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = ev {
            match interface.as_str() {
                "wl_compositor" => s.compositor = Some(reg.bind(name, version.min(4), qh, ())),
                "zwlr_layer_shell_v1" => s.layer_shell = Some(reg.bind(name, version.min(4), qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for Wl {
    fn event(s: &mut Self, ls: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ev: zwlr_layer_surface_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                ls.ack_configure(serial);
                s.size = Some((width.max(1), height.max(1)));
            }
            zwlr_layer_surface_v1::Event::Closed => s.closed = true,
            _ => {}
        }
    }
}

delegate_noop!(Wl: ignore wl_compositor::WlCompositor);
delegate_noop!(Wl: ignore wl_surface::WlSurface);
delegate_noop!(Wl: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);

fn main() {
    if let Err(e) = run() {
        eprintln!("fiaboo-wave: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    std::thread::spawn(|| {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            let mut it = line.split_whitespace().filter_map(|v| v.parse::<f32>().ok());
            if let (Some(b), Some(l)) = (it.next(), it.next()) {
                *AUDIO.lock().unwrap() = (b, l);
            }
        }
        std::process::exit(0); // the shell closed the view (or died)
    });

    let conn = Connection::connect_to_env()?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut wl = Wl::default();
    queue.roundtrip(&mut wl)?;
    let (Some(compositor), Some(shell)) = (wl.compositor.clone(), wl.layer_shell.clone()) else {
        return Err("compositor has no wlr layer shell".into());
    };
    let surface = compositor.create_surface(&qh, ());
    use zwlr_layer_surface_v1::{Anchor, KeyboardInteractivity};
    let layer = shell.get_layer_surface(&surface, None, zwlr_layer_shell_v1::Layer::Overlay, "fiaboo-wave".into(), &qh, ());
    layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
    layer.set_exclusive_zone(-1); // cover the taskbar too
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    surface.commit();
    while wl.size.is_none() {
        queue.blocking_dispatch(&mut wl)?;
    }

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN | wgpu::Backends::GL,
        flags: wgpu::InstanceFlags::default(),
        backend_options: wgpu::BackendOptions::default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        display: None,
    });
    let target = wgpu::SurfaceTargetUnsafe::RawHandle {
        raw_display_handle: Some(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
            NonNull::new(conn.backend().display_ptr().cast()).ok_or("no wl_display")?,
        ))),
        raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(NonNull::new(surface.id().as_ptr().cast()).ok_or("no wl_surface")?)),
    };
    // Safety: the connection and the surface live until the end of this function, after the wgpu surface.
    let gpu_surface = unsafe { instance.create_surface_unsafe(target)? };
    let adapter = futures::executor::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: Some(&gpu_surface),
        force_fallback_adapter: false,
    }))?;
    let (device, gpu_queue) = futures::executor::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;

    // a plain (non-sRGB) target, like Shadertoy's, or the colours come out washed out
    let caps = gpu_surface.get_capabilities(&adapter);
    let format = [wgpu::TextureFormat::Bgra8Unorm, wgpu::TextureFormat::Rgba8Unorm]
        .into_iter()
        .find(|f| caps.formats.contains(f))
        .unwrap_or(caps.formats[0]);
    let mut config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        width: 0,
        height: 0,
        present_mode: wgpu::PresentMode::Fifo,
        desired_maximum_frame_latency: 2,
        alpha_mode: caps.alpha_modes[0],
        view_formats: vec![],
    };
    let mut configured = (0, 0);

    let module = device.create_shader_module(wgpu::include_wgsl!("wave.wgsl"));
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("wave"),
        layout: None,
        vertex: wgpu::VertexState { module: &module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(format.into())],
        }),
        multiview_mask: None,
        cache: None,
    });
    let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("wave uniforms"),
        size: (4 + 120 * 4) * 4,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform_buf.as_entire_binding() }],
    });

    let (mut time, mut bass, mut last, mut ready) = (0f32, 0f32, Instant::now(), false);
    while !wl.closed {
        conn.flush()?;
        if let Some(guard) = queue.prepare_read() {
            let _ = guard.read(); // WouldBlock when nothing came in
        }
        queue.dispatch_pending(&mut wl)?;
        let size = wl.size.unwrap_or((1, 1));
        if size != configured {
            (config.width, config.height) = size;
            gpu_surface.configure(&device, &config);
            configured = size;
        }

        let dt = last.elapsed().as_secs_f32().min(0.1);
        last = Instant::now();
        let (b, loud) = *AUDIO.lock().unwrap();
        // soft: the scene drifts at 60 % speed in silence and up to ~1.8x with loud music
        time += dt * (0.6 + 1.2 * loud);
        bass += (b - bass) * (1. - (-dt * 6.).exp());
        let data = uniforms(size, time, bass);
        gpu_queue.write_buffer(&uniform_buf, 0, &data.iter().flat_map(|v| v.to_ne_bytes()).collect::<Vec<u8>>());

        let frame = match gpu_surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            _ => {
                gpu_surface.configure(&device, &config);
                continue;
            }
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        gpu_queue.submit([enc.finish()]);
        frame.present();
        if !ready {
            ready = true;
            println!("ready");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn square_layout() {
        let u = super::uniforms((1920, 1080), 7., 0.3);
        assert_eq!(u.len(), 4 + 120 * 4);
        for i in 0..60 {
            let (p, t) = (&u[4 + 8 * i..8 + 8 * i], &u[8 + 8 * i..12 + 8 * i]);
            assert!(p[0] >= -2. && p[0] < 2. && p[2] >= 0.03 && p[2] <= 0.08, "square {i}: {p:?}");
            assert!((t[0] * t[0] + t[1] * t[1] - 1.).abs() < 1e-4, "square {i} turn {t:?}");
            // the square's centre is near its position (it only turns around a nearby corner)
            assert!((t[2] - p[0]).abs() < 0.1 && (t[3] - p[1]).abs() < 0.1, "square {i} centre {t:?} vs {p:?}");
        }
    }
}
