import assert from "node:assert/strict";
import test from "node:test";
import { createH264FrameDecoder } from "../src/video-decoder.ts";
import type { VideoFrameMsg } from "../src/types.ts";

class SilentVideoDecoder {
  state: "unconfigured" | "configured" | "closed" = "unconfigured";
  decodeQueueSize = 0;

  constructor(_init: VideoDecoderInit) {}

  configure(): void {
    this.state = "configured";
  }

  decode(): void {
    if (this.state !== "configured") throw new Error("decoder is not configured");
  }

  close(): void {
    this.state = "closed";
  }
}

class TestEncodedVideoChunk {
  constructor(_init: EncodedVideoChunkInit) {}
}

function frame(seq: number, key: boolean): VideoFrameMsg {
  return {
    kind: "h264",
    key,
    width: 0,
    height: 0,
    sourceWidth: 0,
    sourceHeight: 0,
    seq,
    data: new Uint8Array([0, 0, 0, 1, key ? 0x65 : 0x41]),
  };
}

test("pre-key transport traffic cannot demote WebCodecs", () => {
  const originalDecoder = globalThis.VideoDecoder;
  const originalChunk = globalThis.EncodedVideoChunk;
  Object.assign(globalThis, {
    VideoDecoder: SilentVideoDecoder,
    EncodedVideoChunk: TestEncodedVideoChunk,
  });

  const paths: string[] = [];
  let fallbacks = 0;
  const decoder = createH264FrameDecoder({
    onFrame: () => {},
    onFallback: () => {
      fallbacks += 1;
    },
    onPath: (path) => paths.push(path),
  });

  try {
    for (let i = 0; i < 50; i += 1) decoder.push(frame(i, false));
    decoder.checkStall(25, 0);
    decoder.checkStall(25, 0);
    assert.equal(decoder.stats().path, "WebCodecs (hardware requested)");
    assert.equal(fallbacks, 0);

    decoder.push(frame(50, true));
    for (let i = 0; i < 18; i += 1) decoder.push(frame(51 + i, false));
    decoder.checkStall(19, 0);
    assert.equal(decoder.stats().path, "WebCodecs (hardware requested)");

    decoder.push(frame(69, false));
    decoder.checkStall(1, 0);
    assert.equal(decoder.stats().path, "WebCodecs (software requested)");
    assert.deepEqual(paths, [
      "WebCodecs (hardware requested)",
      "WebCodecs (software requested)",
    ]);
  } finally {
    decoder.close();
    Object.assign(globalThis, {
      VideoDecoder: originalDecoder,
      EncodedVideoChunk: originalChunk,
    });
  }
});
