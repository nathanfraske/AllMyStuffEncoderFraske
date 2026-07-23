const WEBGL_ERROR_NAMES = new Map<number, string>([
  [0x0500, "INVALID_ENUM"],
  [0x0501, "INVALID_VALUE"],
  [0x0502, "INVALID_OPERATION"],
  [0x0505, "OUT_OF_MEMORY"],
  [0x0506, "INVALID_FRAMEBUFFER_OPERATION"],
  [0x9242, "CONTEXT_LOST_WEBGL"],
]);

export interface WebGlErrorReader {
  readonly NO_ERROR: number;
  getError(): number;
}

export interface WebGlFramebufferReader {
  readonly FRAMEBUFFER: number;
  readonly FRAMEBUFFER_COMPLETE: number;
  checkFramebufferStatus(target: number): number;
}

export function webGlCode(code: number): string {
  return WEBGL_ERROR_NAMES.get(code) ?? `0x${code.toString(16).toUpperCase()}`;
}

/** Fail closed on every WebGL error, not only context loss. */
export function assertWebGlNoError(gl: WebGlErrorReader, stage: string): void {
  const error = gl.getError();
  if (error !== gl.NO_ERROR) {
    throw new Error(`${stage}: WebGL ${webGlCode(error)}`);
  }
}

/** An attached but incomplete render target cannot safely activate the overlay. */
export function assertWebGlFramebufferComplete(
  gl: WebGlFramebufferReader,
  stage: string,
): void {
  const status = gl.checkFramebufferStatus(gl.FRAMEBUFFER);
  if (status !== gl.FRAMEBUFFER_COMPLETE) {
    throw new Error(`${stage}: incomplete framebuffer ${webGlCode(status)}`);
  }
}
