use std::collections::HashSet;
use winit::event::{Event, WindowEvent, ElementState, KeyEvent};
use winit::keyboard::{PhysicalKey, KeyCode};

/// A simple input handler to replace winit_input_helper
#[derive(Default)]
pub struct InputHandler {
    keys_pressed: HashSet<KeyCode>,
    keys_held: HashSet<KeyCode>,
    keys_released: HashSet<KeyCode>,
    close_requested: bool,
    scale_factor: Option<f64>,
}

impl InputHandler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update the input state with a new event
    /// Returns true if the event was handled and the main loop should continue processing
    pub fn update<T>(&mut self, event: &Event<T>) -> bool {
        // Clear per-frame state
        self.keys_pressed.clear();
        self.keys_released.clear();
        self.scale_factor = None;

        match event {
            Event::WindowEvent { event, .. } => {
                match event {
                    WindowEvent::CloseRequested => {
                        self.close_requested = true;
                        true
                    }
                    WindowEvent::KeyboardInput { 
                        event: KeyEvent { 
                            physical_key: PhysicalKey::Code(keycode),
                            state,
                            ..
                        },
                        ..
                    } => {
                        match state {
                            ElementState::Pressed => {
                                if !self.keys_held.contains(keycode) {
                                    self.keys_pressed.insert(*keycode);
                                }
                                self.keys_held.insert(*keycode);
                            }
                            ElementState::Released => {
                                self.keys_held.remove(keycode);
                                self.keys_released.insert(*keycode);
                            }
                        }
                        true
                    }
                    WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                        self.scale_factor = Some(*scale_factor);
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// Check if a key was just pressed this frame
    pub fn key_pressed(&self, keycode: KeyCode) -> bool {
        self.keys_pressed.contains(&keycode)
    }

    /// Check if a key is currently being held
    pub fn key_held(&self, keycode: KeyCode) -> bool {
        self.keys_held.contains(&keycode)
    }

    /// Check if a key was just released this frame
    pub fn key_released(&self, keycode: KeyCode) -> bool {
        self.keys_released.contains(&keycode)
    }

    /// Check if the close button was clicked
    pub fn close_requested(&self) -> bool {
        self.close_requested
    }

    /// Get the scale factor if it changed this frame
    pub fn scale_factor(&self) -> Option<f64> {
        self.scale_factor
    }

    /// Reset the close requested flag
    pub fn reset_close_requested(&mut self) {
        self.close_requested = false;
    }
}
