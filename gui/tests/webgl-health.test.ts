import assert from "node:assert/strict";
import test from "node:test";
import {
  assertWebGlFramebufferComplete,
  assertWebGlNoError,
} from "../src/webgl-health.ts";

test("every non-zero WebGL error fails activation", () => {
  for (const error of [0x0500, 0x0501, 0x0502, 0x0505, 0x0506, 0x9242]) {
    assert.throws(
      () => assertWebGlNoError({ NO_ERROR: 0, getError: () => error }, "FSR pass"),
      /FSR pass: WebGL/,
    );
  }
});

test("NO_ERROR remains healthy", () => {
  assert.doesNotThrow(() => assertWebGlNoError({ NO_ERROR: 0, getError: () => 0 }, "FSR pass"));
});

test("incomplete framebuffer fails before the overlay can activate", () => {
  const base = {
    FRAMEBUFFER: 0x8d40,
    FRAMEBUFFER_COMPLETE: 0x8cd5,
  };
  assert.doesNotThrow(() =>
    assertWebGlFramebufferComplete(
      { ...base, checkFramebufferStatus: () => base.FRAMEBUFFER_COMPLETE },
      "FSR EASU",
    ),
  );
  assert.throws(
    () =>
      assertWebGlFramebufferComplete(
        { ...base, checkFramebufferStatus: () => 0x8cd6 },
        "FSR EASU",
      ),
    /incomplete framebuffer/,
  );
});
