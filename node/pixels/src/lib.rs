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

/// Rotate a packed RGBA buffer clockwise by `quarter_turns` × 90°
/// (`quarter_turns` is taken mod 4). For an odd number of turns the output
/// dimensions are swapped. Used to bring a capture backend's raw, *unrotated*
/// framebuffer (Windows DXGI hands one over) upright so the streamed video
/// matches a physically-rotated monitor — the orientation remote control
/// already targets. Index-mapped 4-byte copies, like the scalers above.
pub fn rotate_rgba(src: &[u8], w: u32, h: u32, quarter_turns: u8) -> (Vec<u8>, u32, u32) {
    let (w, h) = (w as usize, h as usize);
    match quarter_turns & 3 {
        // No rotation — hand the buffer straight back.
        0 => (src.to_vec(), w as u32, h as u32),
        // 90° clockwise: dest is h×w; src (x, y) → dest (h-1-y, x).
        1 => {
            let (ow, oh) = (h, w);
            let mut out = vec![0u8; ow * oh * 4];
            for y in 0..h {
                for x in 0..w {
                    let s = (y * w + x) * 4;
                    let d = (x * ow + (h - 1 - y)) * 4;
                    out[d..d + 4].copy_from_slice(&src[s..s + 4]);
                }
            }
            (out, ow as u32, oh as u32)
        }
        // 180°: same dims, reversed pixel order.
        2 => {
            let mut out = vec![0u8; w * h * 4];
            for y in 0..h {
                for x in 0..w {
                    let s = (y * w + x) * 4;
                    let d = ((h - 1 - y) * w + (w - 1 - x)) * 4;
                    out[d..d + 4].copy_from_slice(&src[s..s + 4]);
                }
            }
            (out, w as u32, h as u32)
        }
        // 270° clockwise (= 90° counter-clockwise): src (x, y) → dest (y, w-1-x).
        _ => {
            let (ow, oh) = (h, w);
            let mut out = vec![0u8; ow * oh * 4];
            for y in 0..h {
                for x in 0..w {
                    let s = (y * w + x) * 4;
                    let d = ((w - 1 - x) * ow + y) * 4;
                    out[d..d + 4].copy_from_slice(&src[s..s + 4]);
                }
            }
            (out, ow as u32, oh as u32)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotate_rgba_turns_and_swaps_dimensions() {
        // A 2×1 image: pixel A at (0,0), pixel B at (1,0).
        let a = [1, 2, 3, 4];
        let b = [5, 6, 7, 8];
        let src: Vec<u8> = a.iter().chain(&b).copied().collect();

        // 0 turns is the identity.
        assert_eq!(rotate_rgba(&src, 2, 1, 0), (src.clone(), 2, 1));

        // 90° CW: 2×1 → 1×2. A (left) goes to the top, B to the bottom.
        let (out, w, h) = rotate_rgba(&src, 2, 1, 1);
        assert_eq!((w, h), (1, 2));
        assert_eq!(out, [a, b].concat());

        // 270° CW: 2×1 → 1×2. A to the bottom, B to the top.
        let (out, w, h) = rotate_rgba(&src, 2, 1, 3);
        assert_eq!((w, h), (1, 2));
        assert_eq!(out, [b, a].concat());

        // 180°: dims unchanged, order reversed (A and B swap).
        let (out, w, h) = rotate_rgba(&src, 2, 1, 2);
        assert_eq!((w, h), (2, 1));
        assert_eq!(out, [b, a].concat());

        // Four 90° turns return to the original (round-trip sanity).
        let mut buf = (src.clone(), 2u32, 1u32);
        for _ in 0..4 {
            buf = rotate_rgba(&buf.0, buf.1, buf.2, 1);
        }
        assert_eq!(buf, (src, 2, 1));
    }

    #[test]
    fn dxgi_rotation_enum_maps_to_clockwise_turns() {
        // DXGI_OUTDUPL_DESC.Rotation enum values: UNSPECIFIED=0, IDENTITY=1,
        // ROTATE90=2, ROTATE180=3, ROTATE270=4. To bring the raw native
        // scan-out upright, rotate clockwise by `max(0, enum-1)` turns — the
        // mapping win_capture encodes as degrees (90→1, 180→2, 270→3) and the
        // operator orient_to_monitor applies. Asserted here, in the shared
        // crate, since the live DXGI constants are Windows-only.
        let enum_to_turns = |e: i32| -> u8 { (e.saturating_sub(1).max(0) as u8) & 3 };
        assert_eq!(enum_to_turns(0), 0, "UNSPECIFIED → 0");
        assert_eq!(enum_to_turns(1), 0, "IDENTITY → 0");
        assert_eq!(enum_to_turns(2), 1, "ROTATE90 → 1 CW");
        assert_eq!(enum_to_turns(3), 2, "ROTATE180 → 2");
        assert_eq!(enum_to_turns(4), 3, "ROTATE270 → 3 CW");

        // And those turn counts produce the expected dim behavior on a 4×2.
        let buf: Vec<u8> = (0..(4 * 2 * 4)).map(|i| i as u8).collect();
        assert_eq!(rotate_rgba(&buf, 4, 2, enum_to_turns(2)).1, 2); // 90 → w becomes 2
        assert_eq!(rotate_rgba(&buf, 4, 2, enum_to_turns(3)).1, 4); // 180 → w stays 4
        assert_eq!(rotate_rgba(&buf, 4, 2, enum_to_turns(4)).1, 2); // 270 → w becomes 2
    }

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
