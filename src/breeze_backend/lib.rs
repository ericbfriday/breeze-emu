#![deny(warnings)]
#![deny(unused_import_braces, unused_qualifications, unused_extern_crates)]

pub mod input;
pub mod dummy;
pub mod ppu;
pub mod viewport;

use std::error::Error;

/// An action that can be performed by the user, is detected by the backend and executed by the
/// emulator core.
pub enum BackendAction {
    /// Exit the emulator
    Exit,
    /// Create a save state
    SaveState,
    /// Restore the last save state
    LoadState,
}

/// Result with an erased error type.
pub type BackendResult<T> = Result<T, Box<dyn Error>>;

/// Trait for screen renderers. Once per frame, they are given the raw screen data produced by the
/// PPU and can then render this content in a backend-specific way.
pub trait Renderer {
    /// Creates a new renderer.
    fn create() -> BackendResult<Self> where Self: Sized;

    /// Render a frame produced by the PPU. For optimal experience, the `Renderer` implementation
    /// should make sure that the frame is visible as soon as possible.
    ///
    /// The passed `frame_data` contains `RGB24` data: For each pixel, R, G and B values are stored
    /// in order, one byte per channel, then for the next pixel (left to right, top line to bottom
    /// line).
    ///
    /// The renderer is also responsible for timing: It should only return to the caller when
    /// another frame should be rendered. This also affects input latency: As soon as the `render`
    /// method returns, the input devices can be queried by the running program. This allows
    /// intricate timing mechanisms for better input latency and makes support for dynamic refresh
    /// easier. If the renderer returns immediately, the emulator will run at maximum speed.
    fn render(&mut self, frame_data: &[u8]) -> BackendResult<Vec<BackendAction>>;

    /// Set the ROM title. This usually sets the window title.
    fn set_rom_title(&mut self, title: &str);
}

// XXX https://github.com/rust-lang/rust/issues/22194
impl<T: Renderer + ?Sized> Renderer for Box<T> {
    fn create() -> BackendResult<Self> where Self: Sized {
        Err("attempted to instantiate erased Renderer type".into())
    }

    fn render(&mut self, frame_data: &[u8]) -> BackendResult<Vec<BackendAction>> {
        (**self).render(frame_data)
    }

    fn set_rom_title(&mut self, title: &str) {
        (**self).set_rom_title(title)
    }
}

/// Trait for audio backends. Provides methods for writing to a stereo audio channel.
pub trait AudioSink {
    /// Creates a new audio sink.
    fn create() -> BackendResult<Self> where Self: Sized;

    /// Write 32 kHz 16-bit data to the device.
    ///
    /// The data contains 16-bit samples for the left and right channel.
    fn write(&mut self, data: &[(i16, i16)]);
}

impl<T: AudioSink + ?Sized> AudioSink for Box<T> {
    fn create() -> BackendResult<Self> where Self: Sized {
        Err("attempted to instantiate erased AudioSink type".into())
    }

    fn write(&mut self, data: &[(i16, i16)]) {
        (**self).write(data);
    }
}
