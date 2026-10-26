/**
 * WebGL renderer for the HiPS layer: each tile is a texture and its mesh a
 * batch of textured triangles, so a fine mesh costs the GPU, not the main
 * thread. `hipsLayer.ts` falls back to the 2D canvas path when WebGL is not
 * available.
 */

/** Mesh point [x, y] in device px, or null when it does not project. */
export type MeshPoint = [number, number] | null;

export interface GlTileRenderer {
  /** True between `webglcontextlost` and `webglcontextrestored`: nothing can be drawn. */
  readonly lost: boolean;
  /** Sizes the viewport to the canvas and clears it to black. */
  begin(width: number, height: number): void;
  /** Draws one tile's (n + 1)² mesh points; returns the triangles drawn. */
  drawTile(bmp: ImageBitmap, n: number, pts: MeshPoint[], opacity: number): number;
  /** Deletes the textures of every bitmap not in `live`. */
  retain(live: Set<ImageBitmap>): void;
  dispose(): void;
}

const VERTEX = `
attribute vec2 a_pos;
attribute vec2 a_uv;
uniform vec2 u_size;
varying vec2 v_uv;
void main() {
  v_uv = a_uv;
  gl_Position = vec4(a_pos.x / u_size.x * 2.0 - 1.0, 1.0 - a_pos.y / u_size.y * 2.0, 0.0, 1.0);
}`;

const FRAGMENT = `
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
uniform sampler2D u_tex;
uniform float u_opacity;
varying vec2 v_uv;
void main() {
  gl_FragColor = vec4(texture2D(u_tex, v_uv).rgb * u_opacity, 1.0);
}`;

/**
 * Interleaved [x, y, u, v] vertices of an n×n mesh, two triangles per cell
 * split along the anti-diagonal as the 2D path splits them (see
 * `drawTile` in `hipsLayer.ts`). Mesh point (a, b) has texture point
 * u = b / n (column), v = a / n (row). A triangle with a point that does not
 * project is left out.
 */
export function meshVertices(n: number, pts: MeshPoint[]): Float32Array {
  const out = new Float32Array(n * n * 6 * 4);
  let k = 0;
  const at = (a: number, b: number) => pts[a * (n + 1) + b];
  const push = (a: number, b: number) => {
    const p = at(a, b)!;
    out[k++] = p[0];
    out[k++] = p[1];
    out[k++] = b / n;
    out[k++] = a / n;
  };
  for (let a = 0; a < n; a++) {
    for (let b = 0; b < n; b++) {
      if (at(a, b) && at(a + 1, b) && at(a, b + 1)) {
        push(a, b);
        push(a + 1, b);
        push(a, b + 1);
      }
      if (at(a + 1, b + 1) && at(a, b + 1) && at(a + 1, b)) {
        push(a + 1, b + 1);
        push(a, b + 1);
        push(a + 1, b);
      }
    }
  }
  return out.subarray(0, k);
}

function compile(gl: WebGLRenderingContext, type: number, source: string): WebGLShader | null {
  const shader = gl.createShader(type);
  if (!shader) return null;
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    console.error('[SkyChart] HiPS shader failed to compile:', gl.getShaderInfoLog(shader));
    gl.deleteShader(shader);
    return null;
  }
  return shader;
}

interface Program {
  program: WebGLProgram;
  buffer: WebGLBuffer;
  aPos: number;
  aUv: number;
  uSize: WebGLUniformLocation | null;
  uOpacity: WebGLUniformLocation | null;
  uTex: WebGLUniformLocation | null;
}

function link(gl: WebGLRenderingContext): Program | null {
  const vs = compile(gl, gl.VERTEX_SHADER, VERTEX);
  const fs = compile(gl, gl.FRAGMENT_SHADER, FRAGMENT);
  const program = gl.createProgram();
  const buffer = gl.createBuffer();
  if (!vs || !fs || !program || !buffer) return null;
  gl.attachShader(program, vs);
  gl.attachShader(program, fs);
  gl.linkProgram(program);
  gl.deleteShader(vs);
  gl.deleteShader(fs);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    console.error('[SkyChart] HiPS shader program failed to link:', gl.getProgramInfoLog(program));
    return null;
  }
  return {
    program,
    buffer,
    aPos: gl.getAttribLocation(program, 'a_pos'),
    aUv: gl.getAttribLocation(program, 'a_uv'),
    uSize: gl.getUniformLocation(program, 'u_size'),
    uOpacity: gl.getUniformLocation(program, 'u_opacity'),
    uTex: gl.getUniformLocation(program, 'u_tex'),
  };
}

/**
 * A renderer on `canvas`, or null when WebGL is not available (the canvas is
 * then untouched and can still take a 2D context). `onRestored` runs after a
 * lost context comes back, so the caller can repaint.
 */
export function createGlRenderer(canvas: HTMLCanvasElement, onRestored: () => void): GlTileRenderer | null {
  let gl: WebGLRenderingContext | null = null;
  try {
    gl = canvas.getContext('webgl', {
      alpha: false, antialias: false, depth: false, stencil: false, premultipliedAlpha: false,
    });
  } catch (err) {
    console.error('[SkyChart] WebGL unavailable for the HiPS layer:', err);
  }
  if (!gl) return null;
  const ctx = gl;
  let prog = link(ctx);
  if (!prog) return null;
  const textures = new Map<ImageBitmap, WebGLTexture>();
  let lost = false;

  const onLost = (e: Event) => {
    e.preventDefault();
    lost = true;
    textures.clear();
    console.error('[SkyChart] HiPS WebGL context lost');
  };
  const onRestore = () => {
    prog = link(ctx);
    lost = !prog;
    if (prog) onRestored();
  };
  canvas.addEventListener('webglcontextlost', onLost);
  canvas.addEventListener('webglcontextrestored', onRestore);

  const texture = (bmp: ImageBitmap): WebGLTexture | null => {
    let t = textures.get(bmp);
    if (t) return t;
    t = ctx.createTexture();
    if (!t) return null;
    ctx.bindTexture(ctx.TEXTURE_2D, t);
    ctx.texImage2D(ctx.TEXTURE_2D, 0, ctx.RGBA, ctx.RGBA, ctx.UNSIGNED_BYTE, bmp);
    ctx.texParameteri(ctx.TEXTURE_2D, ctx.TEXTURE_MIN_FILTER, ctx.LINEAR);
    ctx.texParameteri(ctx.TEXTURE_2D, ctx.TEXTURE_MAG_FILTER, ctx.LINEAR);
    ctx.texParameteri(ctx.TEXTURE_2D, ctx.TEXTURE_WRAP_S, ctx.CLAMP_TO_EDGE);
    ctx.texParameteri(ctx.TEXTURE_2D, ctx.TEXTURE_WRAP_T, ctx.CLAMP_TO_EDGE);
    textures.set(bmp, t);
    return t;
  };

  return {
    get lost() {
      return lost;
    },
    begin(width, height) {
      if (lost || !prog) return;
      ctx.viewport(0, 0, width, height);
      ctx.clearColor(0, 0, 0, 1);
      ctx.clear(ctx.COLOR_BUFFER_BIT);
      ctx.useProgram(prog.program);
      ctx.uniform2f(prog.uSize, width, height);
      ctx.uniform1i(prog.uTex, 0);
      ctx.activeTexture(ctx.TEXTURE0);
      ctx.bindBuffer(ctx.ARRAY_BUFFER, prog.buffer);
      ctx.enableVertexAttribArray(prog.aPos);
      ctx.enableVertexAttribArray(prog.aUv);
      ctx.vertexAttribPointer(prog.aPos, 2, ctx.FLOAT, false, 16, 0);
      ctx.vertexAttribPointer(prog.aUv, 2, ctx.FLOAT, false, 16, 8);
    },
    drawTile(bmp, n, pts, opacity) {
      if (lost || !prog) return 0;
      const vertices = meshVertices(n, pts);
      if (!vertices.length) return 0;
      const t = texture(bmp);
      if (!t) return 0;
      ctx.bindTexture(ctx.TEXTURE_2D, t);
      ctx.uniform1f(prog.uOpacity, opacity);
      ctx.bufferData(ctx.ARRAY_BUFFER, vertices, ctx.DYNAMIC_DRAW);
      ctx.drawArrays(ctx.TRIANGLES, 0, vertices.length / 4);
      return vertices.length / 12;
    },
    retain(live) {
      for (const [bmp, t] of textures) {
        if (live.has(bmp)) continue;
        if (!lost) ctx.deleteTexture(t);
        textures.delete(bmp);
      }
    },
    dispose() {
      canvas.removeEventListener('webglcontextlost', onLost);
      canvas.removeEventListener('webglcontextrestored', onRestore);
      if (!lost) {
        for (const t of textures.values()) ctx.deleteTexture(t);
        if (prog) {
          ctx.deleteBuffer(prog.buffer);
          ctx.deleteProgram(prog.program);
        }
      }
      textures.clear();
      prog = null;
    },
  };
}
