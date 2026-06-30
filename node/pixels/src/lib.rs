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

/// Scale RGBA straight to a tightly packed **I420** (YUV 4:2:0) buffer in a
/// single pass — the H.264 path's real shape. Replaces the old two-step
/// `scale_rgba_to_rgb` → openh264 RGB→YUV, which materialised a full RGB
/// intermediate and then walked it again: this fuses the downscale and the
/// colour conversion so every output pixel is touched once. The layout is
/// the contiguous I420 openh264's `YUVBuffer::from_vec` expects — Y plane
/// (`dw*dh`), then U, then V (each `dw/2 * dh/2`) — so the caller can feed
/// it to the encoder with a borrowing `YUVSource` and never copy.
///
/// Both output edges must be even (4:2:0 needs it; callers already force it
/// via `fit_within_even`). Conversion is the standard integer BT.601
/// limited-range transform; chroma is the average of each 2×2 luma block.
pub fn scale_rgba_to_i420(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    debug_assert!(dw % 2 == 0 && dh % 2 == 0, "I420 needs even output edges");
    // Source byte-offset of each output column, computed once (nearest
    // neighbour, same mapping the RGBA/RGB scalers use).
    let xmap: Vec<usize> = (0..dw).map(|x| (x * sw / dw) * 4).collect();
    let ysize = dw * dh;
    let csize = (dw / 2) * (dh / 2);
    let mut out = vec![0u8; ysize + 2 * csize];
    let (y_plane, chroma) = out.split_at_mut(ysize);
    let (u_plane, v_plane) = chroma.split_at_mut(csize);
    let cw = dw / 2;
    // Two output rows at a time: 4:2:0 chroma is one sample per 2×2 luma block.
    for by in 0..dh / 2 {
        let y0 = 2 * by;
        let y1 = y0 + 1;
        let srow0 = &src[(y0 * sh / dh) * sw * 4..][..sw * 4];
        let srow1 = &src[(y1 * sh / dh) * sw * 4..][..sw * 4];
        for bx in 0..cw {
            let x0 = 2 * bx;
            let (sx0, sx1) = (xmap[x0], xmap[x0 + 1]);
            let p00 = &srow0[sx0..sx0 + 3];
            let p10 = &srow0[sx1..sx1 + 3];
            let p01 = &srow1[sx0..sx0 + 3];
            let p11 = &srow1[sx1..sx1 + 3];
            y_plane[y0 * dw + x0] = rgb_to_y(p00);
            y_plane[y0 * dw + x0 + 1] = rgb_to_y(p10);
            y_plane[y1 * dw + x0] = rgb_to_y(p01);
            y_plane[y1 * dw + x0 + 1] = rgb_to_y(p11);
            // Average the 2×2 RGB block, then convert once for U and V.
            let avg = |i: usize| {
                (p00[i] as u32 + p10[i] as u32 + p01[i] as u32 + p11[i] as u32 + 2) / 4
            };
            let (r, g, b) = (avg(0), avg(1), avg(2));
            let ci = by * cw + bx;
            u_plane[ci] = rgb_to_u(r, g, b);
            v_plane[ci] = rgb_to_v(r, g, b);
        }
    }
    out
}

#[inline]
fn rgb_to_y(p: &[u8]) -> u8 {
    let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
    (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16).clamp(0, 255) as u8
}

#[inline]
fn rgb_to_u(r: u32, g: u32, b: u32) -> u8 {
    let (r, g, b) = (r as i32, g as i32, b as i32);
    (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128).clamp(0, 255) as u8
}

#[inline]
fn rgb_to_v(r: u32, g: u32, b: u32) -> u8 {
    let (r, g, b) = (r as i32, g as i32, b as i32);
    (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128).clamp(0, 255) as u8
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
    fn i420_has_correct_layout_and_neutral_chroma() {
        // A 2×2 mid-grey image: every channel 128, alpha ignored.
        let grey = [128u8, 128, 128, 255];
        let src: Vec<u8> = grey.repeat(4);
        let out = scale_rgba_to_i420(&src, 2, 2, 2, 2);
        // Y plane (4) + U (1) + V (1) for a 2×2.
        assert_eq!(out.len(), 2 * 2 + 1 + 1);
        // Grey → Y≈126 (16 + 219*0.5), U=V=128 (no colour).
        for &y in &out[..4] {
            assert!((125..=127).contains(&y), "grey luma {y} out of range");
        }
        assert_eq!(out[4], 128, "neutral U");
        assert_eq!(out[5], 128, "neutral V");
    }

    #[test]
    fn i420_black_and_white_hit_limited_range_endpoints() {
        let black: Vec<u8> = [0u8, 0, 0, 255].repeat(4);
        let white: Vec<u8> = [255u8, 255, 255, 255].repeat(4);
        let kb = scale_rgba_to_i420(&black, 2, 2, 2, 2);
        let kw = scale_rgba_to_i420(&white, 2, 2, 2, 2);
        // BT.601 limited range: black luma 16, white luma 235.
        assert!(kb[..4].iter().all(|&y| y == 16), "black luma = 16");
        assert!(kw[..4].iter().all(|&y| y == 235), "white luma = 235");
    }

    #[test]
    fn i420_downscale_picks_the_same_columns_as_the_rgb_scaler() {
        // 4×2 → 2×2: nearest-neighbour keeps columns 0 and 2, rows 0 and 1.
        // Build a frame whose top-left 2×2 (after subsample) is pure red.
        let red = [255u8, 0, 0, 255];
        let blu = [0u8, 0, 255, 255];
        // row pattern: red blu red blu  (cols 0,2 = red; the scaler samples those)
        let row: Vec<u8> = [red, blu, red, blu].concat();
        let src: Vec<u8> = [row.clone(), row].concat();
        let out = scale_rgba_to_i420(&src, 4, 2, 2, 2);
        // Red in BT.601: Y≈81. All four luma should match the red sample.
        assert!(out[..4].iter().all(|&y| (80..=82).contains(&y)), "red luma");
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
