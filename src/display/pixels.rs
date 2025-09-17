use crate::cartridge;
use crate::display::gui::Framework;
use crate::gb;
use crate::ppu;

use winit::dpi::LogicalSize;
use winit::event::{Event,WindowEvent};
use winit::event_loop::EventLoop;
use winit::keyboard::KeyCode;
use winit::window::WindowBuilder;
use winit_input_helper::WinitInputHelper;
use pixels::{Error,Pixels,SurfaceTexture};

const WIDTH: u32 = 160;
const HEIGHT: u32 = 144;
const DEFAULT_SCALE: u32 = 3;

pub fn run_with_gui(bios: Option<String>, rom: Option<String>) -> Result<(), Error> {
    let event_loop = EventLoop::new().unwrap();
    let mut input = WinitInputHelper::new();
    let window = {
        let size = LogicalSize::new((WIDTH * DEFAULT_SCALE) as f64, (HEIGHT * DEFAULT_SCALE) as f64);
        WindowBuilder::new()
            .with_title("RustyBoi")
            .with_inner_size(size)
            .with_min_inner_size(LogicalSize::new(WIDTH as f64, HEIGHT as f64))
            .build(&event_loop)
            .unwrap()
    };

    let (mut pixels, mut framework) = {
        let window_size = window.inner_size();
        let scale_factor = window.scale_factor() as f32;
        let surface_texture = SurfaceTexture::new(window_size.width, window_size.height, &window);
        let pixels = Pixels::new(WIDTH, HEIGHT, surface_texture)?;
        let framework = Framework::new(
            &event_loop,
            window_size.width,
            window_size.height,
            scale_factor,
            &pixels,
        );

        (pixels, framework)
    };
    let mut world = World::new(bios, rom);

    let res = event_loop.run(|event, elwt| {
        if input.update(&event) {
            if input.key_pressed(KeyCode::Escape) || input.close_requested() {
                elwt.exit();
                return;
            }

            if let Some(scale_factor) = input.scale_factor() {
                framework.scale_factor(scale_factor);
            }

            // Update internal state and request a redraw (only if not resizing)
            world.update();
            window.request_redraw();
        }

        match event {
            Event::WindowEvent {
                event: WindowEvent::Resized(size),
                ..
            } => {
                if let Err(err) = pixels.resize_surface(size.width, size.height) {
                    println!("Failed to resize surface during window event: {}", err);
                    elwt.exit();
                    return;
                }
                framework.resize(size.width, size.height);
            }
            Event::WindowEvent {
                event: WindowEvent::RedrawRequested,
                ..
            } => {
                world.draw(pixels.frame_mut());

                let should_exit = framework.prepare(&window);
                if should_exit {
                    elwt.exit();
                    return;
                }
                // Prepare egui
                framework.prepare(&window);

                // Render everything together
                let render_result = pixels.render_with(|encoder, render_target, context| {
                    context.scaling_renderer.render(encoder, render_target);

                    framework.render(encoder, render_target, context);

                    Ok(())
                });

                // Basic error handling
                if let Err(err) = render_result {
                    println!("Failed to render pixels: {}", err);
                    elwt.exit();
                }
            }
            Event::WindowEvent { event, .. } => {
                framework.handle_event(&window, &event);
            }
            _ => (),
        }
    });
    res.map_err(|e| Error::UserDefined(Box::new(e)))
}

struct World {
    gb: gb::GB,
    frame: Option<[u8; ppu::FRAMEBUFFER_SIZE * 4]>,
}

impl World {
    fn new(bios: Option<String>, rom: Option<String>) -> Self {
        let mut gb = gb::GB::new();

        if let Some(rom) = rom {
            let cartridge = cartridge::Cartridge::load(&rom)
                .expect("Failed to load ROM file");
            gb.insert(cartridge);
        }

        if let Some(bios) = bios {
            gb.load_bios(&bios)
                .expect("Failed to load BIOS file");
        }

        Self {
            gb,
            frame: None,
        }
    }

    fn draw(&mut self, frame: &mut [u8]) {
        if let Some(rgba_frame) = &self.frame {
            frame.copy_from_slice(rgba_frame);
            self.frame = None;
        }
    }

    fn update(&mut self) {
        self.frame = Some(convert_to_rgba(&self.gb.run_until_frame()));
    }
}

fn convert_to_rgba(frame: &[u8; ppu::FRAMEBUFFER_SIZE]) -> [u8; ppu::FRAMEBUFFER_SIZE * 4] {
    let mut rgba_frame = [0; ppu::FRAMEBUFFER_SIZE * 4];
    for (i, &pixel) in frame.iter().enumerate() {
        let rgba = match pixel {
            0 => [0xFF, 0xFF, 0xFF, 0xFF], // White
            1 => [0xAA, 0xAA, 0xAA, 0xFF], // Light gray
            2 => [0x55, 0x55, 0x55, 0xFF], // Dark gray
            3 => [0x00, 0x00, 0x00, 0xFF], // Black
            _ => [0xFF, 0x00, 0xFF, 0xFF], // Fallback (magenta)
        };
        let offset = i * 4;
        rgba_frame[offset..offset + 4].copy_from_slice(&rgba);
    }
    rgba_frame
}
