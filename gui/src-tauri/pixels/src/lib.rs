//! Nearest-neighbour downscale of captured RGBA frames — the cheapest
//! resampler wins at streaming sizes (the codec dominates visually), but
//! it touches every output pixel every frame, so the source column for
//! each output column is computed once and the inner loops are pure
//! row-sliced copies.

/// Scale RGBA to RGBA. Never upscales in practice (`dw <= sw`, `dh <= sh`
/// from the callers' fit), but growth is handled rather than guarded.
pub fn scale_rgba(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    let xmap: Vec<usize> = (0..dw).map(|x| (x * sw / dw) * 4).collect();
    let mut out = vec![0u8; dw * dh * 4];
    for (y, drow) in out.chunks_exact_mut(dw * 4).enumerate() {
        let sy = y * sh / dh;
        let srow = &src[sy * sw * 4..][..sw * 4];
        for (dst, &sx) in drow.chunks_exact_mut(4).zip(&xmap) {
            dst.copy_from_slice(&srow[sx..sx + 4]);
        }
    }
    out
}

/// Scale RGBA straight to tightly packed RGB (alpha dropped) — the H.264
/// path's shape: the encoder's fast RGB→YUV conversion wants 3-byte
/// pixels, and the unchanged-frame compare gets 25% cheaper. Also serves
/// as the plain strip pass when no scaling is needed.
pub fn scale_rgba_to_rgb(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    let xmap: Vec<usize> = (0..dw).map(|x| (x * sw / dw) * 4).collect();
    let mut out = vec![0u8; dw * dh * 3];
    for (y, drow) in out.chunks_exact_mut(dw * 3).enumerate() {
        let sy = y * sh / dh;
        let srow = &src[sy * sw * 4..][..sw * 4];
        for (dst, &sx) in drow.chunks_exact_mut(3).zip(&xmap) {
            dst.copy_from_slice(&srow[sx..sx + 3]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_rgba_samples_the_right_pixels() {
        // 2x1 image: red then blue. Downscale to 1x1 keeps the left pixel.
        let src = [255, 0, 0, 255, 0, 0, 255, 255];
        assert_eq!(scale_rgba(&src, 2, 1, 1, 1), vec![255, 0, 0, 255]);
        // Growth repeats pixels (callers never ask, the fn never errors).
        let one = [9, 8, 7, 255];
        assert_eq!(scale_rgba(&one, 1, 1, 2, 2), one.repeat(4));
    }

    #[test]
    fn rgb_variant_strips_alpha_and_samples_identically() {
        let src = [255, 0, 0, 255, 0, 0, 255, 255];
        assert_eq!(scale_rgba_to_rgb(&src, 2, 1, 1, 1), vec![255, 0, 0]);
        // The no-scale case is a pure strip pass.
        assert_eq!(
            scale_rgba_to_rgb(&src, 2, 1, 2, 1),
            vec![255, 0, 0, 0, 0, 255]
        );
    }
}
