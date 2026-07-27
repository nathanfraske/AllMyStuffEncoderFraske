export interface ActiveRegion {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
}

/**
 * Find symmetric black bars around content in an RGBA frame.
 *
 * Returns null for a frame that carries too little visible content to be
 * evidence. Asymmetric dark edges are retained as content, since cropping
 * those would corrupt pointer mapping.
 */
export function detectSymmetricActiveRegion(
  data: ArrayLike<number>,
  width: number,
  height: number,
): ActiveRegion | null {
  if (width <= 0 || height <= 0 || data.length < width * height * 4) return null;
  const dark = 24;
  const median = (values: number[]) =>
    values.slice().sort((left, right) => left - right)[values.length >> 1];
  const bright = (x: number, y: number) => {
    const pixel = (y * width + x) * 4;
    return data[pixel] > dark || data[pixel + 1] > dark || data[pixel + 2] > dark;
  };

  const left: number[] = [];
  const right: number[] = [];
  for (let sample = 1; sample <= 6; sample += 1) {
    const y = Math.floor((height * sample) / 7);
    let first = 0;
    while (first < width && !bright(first, y)) first += 1;
    let last = width - 1;
    while (last > first && !bright(last, y)) last -= 1;
    if (first < last) {
      left.push(first);
      right.push(last);
    }
  }
  if (left.length < 4) return null;

  const top: number[] = [];
  const bottom: number[] = [];
  for (let sample = 1; sample <= 6; sample += 1) {
    const x = Math.floor((width * sample) / 7);
    let first = 0;
    while (first < height && !bright(x, first)) first += 1;
    let last = height - 1;
    while (last > first && !bright(x, last)) last -= 1;
    if (first < last) {
      top.push(first);
      bottom.push(last);
    }
  }

  const x0 = median(left);
  const x1 = median(right) + 1;
  const y0 = top.length >= 4 ? median(top) : 0;
  const y1 = top.length >= 4 ? median(bottom) + 1 : height;
  const barLeft = x0;
  const barRight = width - x1;
  const barTop = y0;
  const barBottom = height - y1;
  const symmetricX =
    Math.min(barLeft, barRight) > width * 0.015 &&
    Math.abs(barLeft - barRight) < width * 0.02;
  const symmetricY =
    Math.min(barTop, barBottom) > height * 0.015 &&
    Math.abs(barTop - barBottom) < height * 0.02;

  return {
    x0: symmetricX ? x0 / width : 0,
    x1: symmetricX ? x1 / width : 1,
    y0: symmetricY ? y0 / height : 0,
    y1: symmetricY ? y1 / height : 1,
  };
}
