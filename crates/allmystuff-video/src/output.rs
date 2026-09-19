//! Allocation policy for decoded RGBA output.

/// A decoded picture with no application or transport envelope.
pub struct RgbaFrame {
    pub ts_us: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Allocates the final output object before conversion writes its pixels.
/// `rgba_mut` must return exactly `width * height * 4` writable bytes.
/// Applications may reserve their own header in this same allocation.
pub trait DecodeOutput: 'static {
    type Frame;

    fn allocate(ts_us: u64, width: u32, height: u32) -> Self::Frame;
    fn rgba_mut(frame: &mut Self::Frame) -> &mut [u8];
}

pub struct RgbaOutput;

impl DecodeOutput for RgbaOutput {
    type Frame = RgbaFrame;

    fn allocate(ts_us: u64, width: u32, height: u32) -> RgbaFrame {
        RgbaFrame {
            ts_us,
            width,
            height,
            rgba: vec![0; (width as usize) * (height as usize) * 4],
        }
    }

    fn rgba_mut(frame: &mut RgbaFrame) -> &mut [u8] {
        &mut frame.rgba
    }
}
