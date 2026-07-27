import type { VideoFrameMsg } from "./types";

export interface DecodeStallSample {
  submitted: number;
  silentSubmissions: number;
}

/**
 * Counts only chunks that were successfully submitted to a live decoder.
 * Incoming deltas discarded while waiting for a key unit are deliberately
 * absent, so transport traffic cannot be mistaken for decoder evidence.
 */
export class DecodeStallEvidence {
  private submittedSinceSample = 0;
  private silentSubmissions = 0;

  noteSubmission(): void {
    this.submittedSinceSample += 1;
  }

  sample(decodedSinceLastSample: number): DecodeStallSample {
    const submitted = this.submittedSinceSample;
    this.submittedSinceSample = 0;

    if (decodedSinceLastSample > 0) {
      this.silentSubmissions = 0;
    } else if (submitted > 0) {
      this.silentSubmissions += submitted;
    }

    return { submitted, silentSubmissions: this.silentSubmissions };
  }

  reset(): void {
    this.submittedSinceSample = 0;
    this.silentSubmissions = 0;
  }
}

export type ViewerDecodePath =
  | "WebCodecs (hardware requested)"
  | "WebCodecs (software requested)"
  | "Native (NVDEC/OpenH264)";

export interface H264DecoderStats {
  path: ViewerDecodePath;
  queueDepth: number;
  decodeFails: number;
  decodedFrames: number;
  paintedFrames: number;
}

export interface H264FrameDecoder {
  push(frame: VideoFrameMsg): void;
  checkStall(inputFps: number, paintFps: number): void;
  stats(): H264DecoderStats;
  close(): void;
}

interface H264FrameDecoderOptions {
  /** Called synchronously with the latest decoded frame. The decoder closes it. */
  onFrame: (frame: VideoFrame) => void;
  /** Re-watch the route with backend decode after both WebCodecs rungs fail. */
  onFallback: (reason: string) => void;
  /** Ask the sender for a clean key unit after a decoder reset. */
  onRefresh?: (reason: string) => void;
  onPath?: (path: ViewerDecodePath) => void;
  onDecodeError?: (error: unknown) => void;
  onDebug?: (line: string) => void;
}

/**
 * Read the exact AVC profile, constraints and level from an Annex-B SPS.
 * The fixed fallback matches the legacy probe/config used by the viewer, but
 * a stream-provided SPS always wins.
 */
export function h264CodecString(accessUnit: Uint8Array): string | null {
  for (let i = 0; i + 4 < accessUnit.length; i++) {
    if (accessUnit[i] !== 0 || accessUnit[i + 1] !== 0) continue;
    const offset =
      accessUnit[i + 2] === 1
        ? i + 3
        : accessUnit[i + 2] === 0 && accessUnit[i + 3] === 1
          ? i + 4
          : 0;
    if (!offset) continue;
    if ((accessUnit[offset] & 0x1f) === 7 && offset + 3 < accessUnit.length) {
      const hex = (n: number) => n.toString(16).padStart(2, "0").toUpperCase();
      return `avc1.${hex(accessUnit[offset + 1])}${hex(accessUnit[offset + 2])}${hex(accessUnit[offset + 3])}`;
    }
    i = offset;
  }
  return null;
}

/** Ask WebCodecs itself instead of treating the API's presence as support. */
export async function webCodecsH264Supported(): Promise<boolean> {
  if (typeof VideoDecoder === "undefined" || typeof EncodedVideoChunk === "undefined") return false;
  try {
    const result = await VideoDecoder.isConfigSupported({
      codec: "avc1.42E01F",
      optimizeForLatency: true,
    });
    return result.supported === true;
  } catch {
    return false;
  }
}

/**
 * A latest-frame H.264 decoder shared by popouts and room tiles.
 *
 * It starts by requesting hardware WebCodecs, steps to software WebCodecs on
 * a dead or stalled decoder, then asks its owner to re-watch with the native
 * backend ladder. Output is superseded before requestAnimationFrame so an
 * occluded or busy webview cannot build a presentation backlog.
 */
export function createH264FrameDecoder(options: H264FrameDecoderOptions): H264FrameDecoder {
  let decoder: VideoDecoder | null = null;
  let codec: string | null = null;
  let acceleration: HardwareAcceleration = "prefer-hardware";
  let path: ViewerDecodePath = "WebCodecs (hardware requested)";
  let generation = 0;
  let decodeOutputs = 0;
  let rungErrors = 0;
  let decodeFails = 0;
  let decodedFrames = 0;
  let paintedFrames = 0;
  let decodedAtLastCheck = 0;
  const stallEvidence = new DecodeStallEvidence();
  let pendingFrame: VideoFrame | null = null;
  let paintRequest: number | null = null;
  let fallbackRequested = false;
  let closed = false;

  options.onPath?.(path);

  const disposeDecoder = () => {
    generation += 1;
    try {
      if (decoder && decoder.state !== "closed") decoder.close();
    } catch {
      // The decoder was already invalidated by the browser.
    }
    decoder = null;
  };

  const requestFallback = (reason: string) => {
    if (closed || fallbackRequested) return;
    fallbackRequested = true;
    disposeDecoder();
    options.onDebug?.(`[video-decoder] ${reason}; switching to native decode`);
    options.onFallback(reason);
  };

  const stepDown = (reason: string) => {
    if (closed || fallbackRequested) return;
    disposeDecoder();
    decodeOutputs = 0;
    rungErrors = 0;
    stallEvidence.reset();
    decodedAtLastCheck = decodedFrames;
    if (acceleration !== "prefer-software") {
      acceleration = "prefer-software";
      path = "WebCodecs (software requested)";
      options.onPath?.(path);
      options.onDebug?.(`[video-decoder] ${reason}; retrying with software WebCodecs`);
      options.onRefresh?.(reason);
    } else {
      requestFallback(reason);
    }
  };

  const paintLatest = () => {
    paintRequest = null;
    const frame = pendingFrame;
    pendingFrame = null;
    if (!frame) return;
    try {
      if (!closed) {
        options.onFrame(frame);
        paintedFrames += 1;
      }
    } catch (error) {
      options.onDecodeError?.(error);
      options.onDebug?.(`[video-decoder] paint error: ${String(error)}`);
    } finally {
      frame.close();
    }
  };

  const dropDecoder = (error: unknown, instanceGeneration: number) => {
    if (closed || instanceGeneration !== generation) return;
    const neverOutput = decodeOutputs === 0;
    decodeFails += 1;
    rungErrors += 1;
    options.onDecodeError?.(error);
    options.onDebug?.(`[video-decoder] decode error: ${String(error)}`);
    disposeDecoder();
    if (rungErrors >= 3) {
      stepDown(
        neverOutput
          ? "decoder failed before first output three times"
          : "decoder failed three times on the same acceleration rung",
      );
    } else {
      options.onRefresh?.("decoder error");
    }
  };

  const configureAtKey = (frame: VideoFrameMsg) => {
    codec = h264CodecString(frame.data) ?? codec ?? "avc1.42E01F";
    const instanceGeneration = generation + 1;
    generation = instanceGeneration;
    try {
      decoder = new VideoDecoder({
        output: (decoded) => {
          if (closed || instanceGeneration !== generation) {
            decoded.close();
            return;
          }
          decodeOutputs += 1;
          // Errors are consecutive-rung evidence, not a lifetime total. A
          // decoder that produces again has proved this instance viable.
          rungErrors = 0;
          decodedFrames += 1;
          pendingFrame?.close();
          pendingFrame = decoded;
          paintRequest ??= requestAnimationFrame(paintLatest);
        },
        error: (error) => dropDecoder(error, instanceGeneration),
      });
      decoder.configure({
        codec,
        optimizeForLatency: true,
        hardwareAcceleration: acceleration,
      });
      decodeOutputs = 0;
      stallEvidence.reset();
      decodedAtLastCheck = decodedFrames;
    } catch (error) {
      dropDecoder(error, instanceGeneration);
    }
  };

  return {
    push(frame) {
      if (closed || fallbackRequested || frame.kind !== "h264") return;

      // Snap an abnormally deep decoder back to a fresh key unit. Twelve is
      // the existing console guard, kept identical across viewer surfaces.
      if (decoder && decoder.state !== "closed" && frame.key && decoder.decodeQueueSize > 12) {
        options.onDebug?.(
          `[video-decoder] queue=${decoder.decodeQueueSize}; snapping to the latest key unit`,
        );
        disposeDecoder();
      }

      if (!decoder || decoder.state === "closed") {
        if (!frame.key) return;
        configureAtKey(frame);
      }
      if (!decoder || decoder.state === "closed") return;

      try {
        decoder.decode(
          new EncodedVideoChunk({
            type: frame.key ? "key" : "delta",
            timestamp: frame.seq,
            data: frame.data,
          }),
        );
        stallEvidence.noteSubmission();
      } catch (error) {
        dropDecoder(error, generation);
        return;
      }

      // Do not use VideoDecoder.flush() as a liveness probe. WebCodecs makes
      // a flushed decoder require a new key chunk, so continuing with the
      // next delta creates the very DataError/reset loop this guard was meant
      // to prevent. The one-second output/cadence check below observes the
      // same twenty-chunk condition after output tasks have had time to run.
    },

    checkStall(inputFps, outputFps) {
      const decodedSinceLastCheck = decodedFrames - decodedAtLastCheck;
      decodedAtLastCheck = decodedFrames;
      const evidence = stallEvidence.sample(decodedSinceLastCheck);
      if (evidence.silentSubmissions >= 20) {
        stallEvidence.reset();
        stepDown("decoder accepted twenty more chunks without output");
        return;
      }
      if (
        !closed &&
        !fallbackRequested &&
        inputFps > 5 &&
        outputFps < inputFps / 4 &&
        (decoder?.decodeQueueSize ?? 0) > 8
      ) {
        stepDown(
          `decoder stalled (input=${inputFps}/s output=${outputFps}/s queue=${decoder?.decodeQueueSize ?? 0})`,
        );
      }
    },

    stats() {
      return {
        path,
        queueDepth: decoder?.decodeQueueSize ?? 0,
        decodeFails,
        decodedFrames,
        paintedFrames,
      };
    },

    close() {
      if (closed) return;
      closed = true;
      disposeDecoder();
      if (paintRequest !== null) cancelAnimationFrame(paintRequest);
      paintRequest = null;
      pendingFrame?.close();
      pendingFrame = null;
    },
  };
}
