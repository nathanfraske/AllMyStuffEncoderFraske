import {
  assertWebGlFramebufferComplete,
  assertWebGlNoError,
} from "./webgl-health";

// FSR1-style spatial upscaling for the video stage: a WebGL2 port of the
// two-pass AMD FidelityFX Super Resolution 1.0 pipeline — EASU
// (edge-adaptive spatial upsampling) followed by RCAS (robust
// contrast-adaptive sharpening). Ported from AMD's MIT-licensed
// FidelityFX-FSR reference (github.com/GPUOpen-Effects/FidelityFX-FSR);
// simplified where WebGL2 lacks textureGather, faithful in structure.
//
// Why: a stream decoded below the display size used to be stretched by
// the browser's bilinear filter — soft edges, smeared text. EASU rebuilds
// edges directionally and RCAS restores local contrast, for well under a
// millisecond of GPU at 4K on anything with a real GPU. The renderer
// samples the existing 2D paint canvas (one GPU-GPU upload per frame) and
// presents on an overlay canvas, so the decode pipeline doesn't change at
// all — kill the overlay and the plain path is exactly what it was.

const VS = `#version 300 es
precision highp float;
out vec2 uv;
void main() {
  vec2 p = vec2((gl_VertexID << 1) & 2, gl_VertexID & 2);
  uv = p;
  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}`;

// EASU — directional analysis on the luma of the 12-tap neighborhood,
// then an anisotropic lanczos-ish accumulation with deringing.
const EASU_FS = `#version 300 es
precision highp float;
uniform sampler2D src;
uniform vec2 srcSize;
uniform vec2 dstSize;
in vec2 uv;
out vec4 outColor;

float lum(vec3 c) { return dot(c, vec3(0.299, 0.587, 0.114)); }

vec3 tap(vec2 p) {
  return texture(src, (p + 0.5) / srcSize).rgb;
}

// Accumulate one tap with the rotated/scaled kernel.
void acc(inout vec3 color, inout float weight, vec2 off, vec2 dir, vec2 len, vec3 c) {
  vec2 v = vec2(dot(off, dir), dot(off, vec2(-dir.y, dir.x)));
  v *= len;
  float d2 = min(dot(v, v), 4.0);
  // 25/16 * (2/5 d2 - 1)^2 - (25/16 - 1), windowed by (1/4 d2 - 1)^2 —
  // AMD's polynomial approximation of the negative-lobe kernel.
  float wB = 0.4 * d2 - 1.0;
  float wA = 0.25 * d2 - 1.0;
  float w = (25.0 / 16.0 * wB * wB - (25.0 / 16.0 - 1.0)) * (wA * wA);
  color += c * w;
  weight += w;
}

void main() {
  vec2 pp = uv * dstSize * (srcSize / dstSize) - 0.5;
  vec2 fp = floor(pp);
  vec2 f = pp - fp;

  //  b c
  // e f g h
  // i j k l
  //  n o
  vec3 cb = tap(fp + vec2(0.0, -1.0));
  vec3 cc = tap(fp + vec2(1.0, -1.0));
  vec3 ce = tap(fp + vec2(-1.0, 0.0));
  vec3 cf = tap(fp + vec2(0.0, 0.0));
  vec3 cg = tap(fp + vec2(1.0, 0.0));
  vec3 ch = tap(fp + vec2(2.0, 0.0));
  vec3 ci = tap(fp + vec2(-1.0, 1.0));
  vec3 cj = tap(fp + vec2(0.0, 1.0));
  vec3 ck = tap(fp + vec2(1.0, 1.0));
  vec3 cl = tap(fp + vec2(2.0, 1.0));
  vec3 cn = tap(fp + vec2(0.0, 2.0));
  vec3 co = tap(fp + vec2(1.0, 2.0));

  float lb = lum(cb), lc = lum(cc), le = lum(ce), lf = lum(cf);
  float lg = lum(cg), lh = lum(ch), li = lum(ci), lj = lum(cj);
  float lk = lum(ck), ll = lum(cl), ln = lum(cn), lo = lum(co);

  // Direction from the interpolated cross-gradients of the inner quad.
  float dirX = (lg - le) * (1.0 - f.x) + (lh - lf) * f.x;
  float dirYtop = (lj - lb) * (1.0 - f.x) + (lk - lc) * f.x;
  float dirYbot = (ln - lf) * (1.0 - f.x) + (lo - lg) * f.x;
  float dirY = dirYtop * (1.0 - f.y) + dirYbot * f.y;
  vec2 dir = vec2(dirX, dirY);
  float dl = dot(dir, dir);
  if (dl < 1.0 / 32768.0) { dir = vec2(1.0, 0.0); dl = 1.0; }
  dir *= inversesqrt(dl);

  // Edge length metric -> kernel stretch (AMD: feature strength from the
  // inner quad's local contrast).
  float mn4 = min(min(lf, lg), min(lj, lk));
  float mx4 = max(max(lf, lg), max(lj, lk));
  float contrast = clamp(abs(mx4 - mn4) * 4.0, 0.0, 1.0);
  float stretch = 1.0 + contrast; // 1 (soft) .. 2 (hard edge)
  vec2 len = vec2(1.0 / stretch, stretch);

  vec3 color = vec3(0.0);
  float weight = 0.0;
  acc(color, weight, vec2(0.0, -1.0) - f, dir, len, cb);
  acc(color, weight, vec2(1.0, -1.0) - f, dir, len, cc);
  acc(color, weight, vec2(-1.0, 0.0) - f, dir, len, ce);
  acc(color, weight, vec2(0.0, 0.0) - f, dir, len, cf);
  acc(color, weight, vec2(1.0, 0.0) - f, dir, len, cg);
  acc(color, weight, vec2(2.0, 0.0) - f, dir, len, ch);
  acc(color, weight, vec2(-1.0, 1.0) - f, dir, len, ci);
  acc(color, weight, vec2(0.0, 1.0) - f, dir, len, cj);
  acc(color, weight, vec2(1.0, 1.0) - f, dir, len, ck);
  acc(color, weight, vec2(2.0, 1.0) - f, dir, len, cl);
  acc(color, weight, vec2(0.0, 2.0) - f, dir, len, cn);
  acc(color, weight, vec2(1.0, 2.0) - f, dir, len, co);
  vec3 c = color / max(weight, 1e-4);

  // Deringing: clamp to the inner quad's range.
  vec3 lo4 = min(min(cf, cg), min(cj, ck));
  vec3 hi4 = max(max(cf, cg), max(cj, ck));
  outColor = vec4(clamp(c, lo4, hi4), 1.0);
}`;

// RCAS — 5-tap cross sharpening bounded by the local min/max so it can't
// ring; sharpness is AMD's attenuation-style knob folded to a constant.
const RCAS_FS = `#version 300 es
precision highp float;
uniform sampler2D src;
uniform vec2 srcSize;
uniform float sharpness;
in vec2 uv;
out vec4 outColor;
void main() {
  vec2 p = uv * srcSize - 0.5;
  vec2 ip = floor(p) + 0.5;
  vec3 e = texture(src, ip / srcSize).rgb;
  vec3 b = texture(src, (ip + vec2(0.0, -1.0)) / srcSize).rgb;
  vec3 d = texture(src, (ip + vec2(-1.0, 0.0)) / srcSize).rgb;
  vec3 f = texture(src, (ip + vec2(1.0, 0.0)) / srcSize).rgb;
  vec3 h = texture(src, (ip + vec2(0.0, 1.0)) / srcSize).rgb;
  vec3 mn = min(min(b, d), min(f, h));
  vec3 mx = max(max(b, d), max(f, h));
  // Text-adaptive sharpness: glyph strokes live in high local-contrast
  // neighborhoods — push those toward full sharpening while smooth
  // gradients (photos, video) ease off, so text crisps up without
  // frying continuous tone.
  float range = max(mx.r - mn.r, max(mx.g - mn.g, mx.b - mn.b));
  float textiness = smoothstep(0.04, 0.30, range);
  float sh = sharpness * mix(0.55, 1.2, textiness);
  // Per-channel limiter: how far we may sharpen before exceeding the
  // local range (AMD's noise-safe lobe).
  vec3 hitLo = mn / (4.0 * mx + 1e-4);
  vec3 hitHi = (1.0 - mx) / (4.0 * mn - 4.0 + 1e-4);
  vec3 lobeRGB = max(-hitLo, hitHi);
  float lobe = max(-0.1875, min(max(lobeRGB.r, max(lobeRGB.g, lobeRGB.b)), 0.0)) * sh;
  float rcp = 1.0 / (4.0 * lobe + 1.0);
  outColor = vec4(((b + d + f + h) * lobe + e) * rcp, 1.0);
}`;

function compile(gl: WebGL2RenderingContext, type: number, src: string): WebGLShader {
  const s = gl.createShader(type);
  if (!s) throw new Error("WebGL could not allocate an FSR shader");
  gl.shaderSource(s, src);
  gl.compileShader(s);
  if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) {
    const detail = gl.getShaderInfoLog(s) ?? "shader compile failed";
    gl.deleteShader(s);
    throw new Error(detail);
  }
  return s;
}

function program(gl: WebGL2RenderingContext, fs: string): WebGLProgram {
  let vertex: WebGLShader | null = null;
  let fragment: WebGLShader | null = null;
  let p: WebGLProgram | null = null;
  try {
    vertex = compile(gl, gl.VERTEX_SHADER, VS);
    fragment = compile(gl, gl.FRAGMENT_SHADER, fs);
    p = gl.createProgram();
    if (!p) throw new Error("WebGL could not allocate an FSR program");
    gl.attachShader(p, vertex);
    gl.attachShader(p, fragment);
    gl.linkProgram(p);
    if (!gl.getProgramParameter(p, gl.LINK_STATUS)) {
      const detail = gl.getProgramInfoLog(p) ?? "program link failed";
      gl.deleteProgram(p);
      p = null;
      throw new Error(detail);
    }
    assertWebGlNoError(gl, "FSR program setup");
    return p;
  } catch (error) {
    if (p) gl.deleteProgram(p);
    throw error;
  } finally {
    if (vertex) gl.deleteShader(vertex);
    if (fragment) gl.deleteShader(fragment);
  }
}

function texture(gl: WebGL2RenderingContext, name: string): WebGLTexture {
  const value = gl.createTexture();
  if (!value) throw new Error(`WebGL could not allocate the FSR ${name} texture`);
  return value;
}

function uniform(
  gl: WebGL2RenderingContext,
  p: WebGLProgram,
  name: string,
): WebGLUniformLocation {
  const value = gl.getUniformLocation(p, name);
  if (!value) throw new Error(`FSR shader is missing the ${name} uniform`);
  return value;
}

/** The two-pass FSR1-style upscaler bound to one overlay canvas. */
export class Fsr1 {
  private gl: WebGL2RenderingContext;
  private easu: WebGLProgram;
  private rcas: WebGLProgram;
  private easuSrc: WebGLUniformLocation;
  private easuSrcSize: WebGLUniformLocation;
  private easuDstSize: WebGLUniformLocation;
  private rcasSrc: WebGLUniformLocation;
  private rcasSrcSize: WebGLUniformLocation;
  private rcasSharpness: WebGLUniformLocation;
  private srcTex: WebGLTexture;
  private midTex: WebGLTexture;
  private fbo: WebGLFramebuffer;
  private srcSize = [0, 0];
  private midSize = [0, 0];
  private lost = false;
  private readonly contextLost = (event: Event) => {
    // Prevent the browser's implicit restore. Program/texture state is not
    // trustworthy after a loss, so the caller falls back to the base canvas
    // and may construct a fresh engine on a later explicit retry.
    event.preventDefault();
    this.lost = true;
    this.onUnavailable?.("WebGL2 context lost");
  };

  constructor(
    private canvas: HTMLCanvasElement,
    private onUnavailable?: (reason: string) => void,
  ) {
    const gl = canvas.getContext("webgl2", {
      alpha: false,
      antialias: false,
      depth: false,
      stencil: false,
      preserveDrawingBuffer: false,
      // If WebGL would fall back to a major software path, normal canvas
      // scaling is safer. A CPU FSR pass competes directly with decode and
      // remote input on the machines that most need the fallback.
      failIfMajorPerformanceCaveat: true,
    });
    if (!gl) throw new Error("WebGL2 unavailable");
    if (gl.isContextLost()) throw new Error("WebGL2 context is unavailable");
    canvas.addEventListener("webglcontextlost", this.contextLost, false);
    let easu: WebGLProgram | null = null;
    let rcas: WebGLProgram | null = null;
    let srcTex: WebGLTexture | null = null;
    let midTex: WebGLTexture | null = null;
    let fbo: WebGLFramebuffer | null = null;
    try {
      easu = program(gl, EASU_FS);
      rcas = program(gl, RCAS_FS);
      const easuSrc = uniform(gl, easu, "src");
      const easuSrcSize = uniform(gl, easu, "srcSize");
      const easuDstSize = uniform(gl, easu, "dstSize");
      const rcasSrc = uniform(gl, rcas, "src");
      const rcasSrcSize = uniform(gl, rcas, "srcSize");
      const rcasSharpness = uniform(gl, rcas, "sharpness");
      srcTex = texture(gl, "source");
      midTex = texture(gl, "intermediate");
      fbo = gl.createFramebuffer();
      if (!fbo) throw new Error("WebGL could not allocate the FSR framebuffer");
      for (const t of [srcTex, midTex]) {
        gl.bindTexture(gl.TEXTURE_2D, t);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      }
      assertWebGlNoError(gl, "FSR activation");
      this.gl = gl;
      this.easu = easu;
      this.rcas = rcas;
      this.easuSrc = easuSrc;
      this.easuSrcSize = easuSrcSize;
      this.easuDstSize = easuDstSize;
      this.rcasSrc = rcasSrc;
      this.rcasSrcSize = rcasSrcSize;
      this.rcasSharpness = rcasSharpness;
      this.srcTex = srcTex;
      this.midTex = midTex;
      this.fbo = fbo;
    } catch (error) {
      if (fbo) gl.deleteFramebuffer(fbo);
      if (srcTex) gl.deleteTexture(srcTex);
      if (midTex) gl.deleteTexture(midTex);
      if (easu) gl.deleteProgram(easu);
      if (rcas) gl.deleteProgram(rcas);
      canvas.removeEventListener("webglcontextlost", this.contextLost, false);
      gl.getExtension("WEBGL_lose_context")?.loseContext();
      throw error;
    }
  }

  private assertNoError(stage: string) {
    try {
      assertWebGlNoError(this.gl, stage);
    } catch (error) {
      if (this.gl.isContextLost()) this.lost = true;
      throw error;
    }
  }

  /** Upscale `source` to `dw`×`dh` device pixels. */
  render(source: TexImageSource, sw: number, sh: number, dw: number, dh: number) {
    const gl = this.gl;
    if (this.lost || gl.isContextLost()) {
      throw new Error("WebGL2 context is unavailable");
    }
    if (
      !Number.isInteger(sw) ||
      !Number.isInteger(sh) ||
      !Number.isInteger(dw) ||
      !Number.isInteger(dh) ||
      sw <= 0 ||
      sh <= 0 ||
      dw <= 0 ||
      dh <= 0
    ) {
      throw new Error(`Invalid FSR dimensions ${sw}x${sh} -> ${dw}x${dh}`);
    }
    if (this.canvas.width !== dw) this.canvas.width = dw;
    if (this.canvas.height !== dh) this.canvas.height = dh;

    gl.bindTexture(gl.TEXTURE_2D, this.srcTex);
    gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, true);
    if (this.srcSize[0] !== sw || this.srcSize[1] !== sh) {
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, gl.RGBA, gl.UNSIGNED_BYTE, source);
      this.srcSize = [sw, sh];
    } else {
      // Preserve texture storage across frames. The old texImage2D call
      // re-specified the full allocation for every decoded picture.
      gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, gl.RGBA, gl.UNSIGNED_BYTE, source);
    }
    gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, false);

    let midResized = false;
    if (this.midSize[0] !== dw || this.midSize[1] !== dh) {
      gl.bindTexture(gl.TEXTURE_2D, this.midTex);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, dw, dh, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
      this.midSize = [dw, dh];
      midResized = true;
    }
    this.assertNoError("FSR texture upload");

    // Pass 1: EASU src -> mid.
    gl.bindFramebuffer(gl.FRAMEBUFFER, this.fbo);
    if (midResized) {
      gl.framebufferTexture2D(
        gl.FRAMEBUFFER,
        gl.COLOR_ATTACHMENT0,
        gl.TEXTURE_2D,
        this.midTex,
        0,
      );
      assertWebGlFramebufferComplete(gl, "FSR EASU target");
      this.assertNoError("FSR EASU target");
    }
    gl.viewport(0, 0, dw, dh);
    gl.useProgram(this.easu);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.srcTex);
    gl.uniform1i(this.easuSrc, 0);
    gl.uniform2f(this.easuSrcSize, sw, sh);
    gl.uniform2f(this.easuDstSize, dw, dh);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    this.assertNoError("FSR EASU pass");

    // Pass 2: RCAS mid -> screen.
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.viewport(0, 0, dw, dh);
    gl.useProgram(this.rcas);
    gl.bindTexture(gl.TEXTURE_2D, this.midTex);
    gl.uniform1i(this.rcasSrc, 0);
    gl.uniform2f(this.rcasSrcSize, dw, dh);
    gl.uniform1f(this.rcasSharpness, 0.87);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    this.assertNoError("FSR RCAS pass");
  }

  dispose() {
    this.canvas.removeEventListener("webglcontextlost", this.contextLost, false);
    if (!this.lost) {
      this.gl.deleteFramebuffer(this.fbo);
      this.gl.deleteTexture(this.srcTex);
      this.gl.deleteTexture(this.midTex);
      this.gl.deleteProgram(this.easu);
      this.gl.deleteProgram(this.rcas);
    }
    const lose = this.gl.getExtension("WEBGL_lose_context");
    lose?.loseContext();
    this.lost = true;
  }
}
