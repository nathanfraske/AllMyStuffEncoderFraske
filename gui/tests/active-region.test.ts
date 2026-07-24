import test from "node:test";
import assert from "node:assert/strict";
import { detectSymmetricActiveRegion } from "../src/active-region.ts";

function frame(
  width: number,
  height: number,
  content: { x0: number; y0: number; x1: number; y1: number } | null,
): Uint8Array {
  const rgba = new Uint8Array(width * height * 4);
  if (!content) return rgba;
  for (let y = content.y0; y < content.y1; y += 1) {
    for (let x = content.x0; x < content.x1; x += 1) {
      const pixel = (y * width + x) * 4;
      rgba[pixel] = 180;
      rgba[pixel + 1] = 140;
      rgba[pixel + 2] = 100;
      rgba[pixel + 3] = 255;
    }
  }
  return rgba;
}

test("detects symmetric pillarbox and letterbox bars from raw RGBA", () => {
  const width = 200;
  const height = 100;
  const region = detectSymmetricActiveRegion(
    frame(width, height, { x0: 20, y0: 10, x1: 180, y1: 90 }),
    width,
    height,
  );
  assert.deepEqual(region, { x0: 0.1, x1: 0.9, y0: 0.1, y1: 0.9 });
});

test("does not crop an asymmetric dark edge", () => {
  const width = 200;
  const height = 100;
  const region = detectSymmetricActiveRegion(
    frame(width, height, { x0: 20, y0: 0, x1: 200, y1: 100 }),
    width,
    height,
  );
  assert.deepEqual(region, { x0: 0, x1: 1, y0: 0, y1: 1 });
});

test("returns no evidence for an all-black frame", () => {
  assert.equal(detectSymmetricActiveRegion(frame(200, 100, null), 200, 100), null);
});
