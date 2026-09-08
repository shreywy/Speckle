/* Speckle editor — adjustments, filters, crop and straighten.
 *
 * All pixel work happens here in WebGL, and the same pipeline that draws the
 * live preview produces the JPEG that gets saved. That is deliberate: if the
 * preview and the export ran through different code the two would drift, and
 * you would only find out after overwriting a photo. */

(function () {
  'use strict';

  const VERT = `#version 300 es
  in vec2 a; out vec2 vUV;
  void main(){ vUV = a; gl_Position = vec4(a * 2.0 - 1.0, 0.0, 1.0); }`;

  const FRAG = `#version 300 es
  precision highp float;
  in vec2 vUV; out vec4 outColor;
  uniform sampler2D uTex;
  uniform vec2  uTexel;
  uniform vec4  uCrop;      // x, y, w, h in rotated-normalised space
  uniform vec2  uRotDims;   // rotated pixel dimensions, for aspect-correct rotation
  uniform float uAngle;     // straighten, radians
  uniform float uZoom;      // scale that keeps straightened corners filled
  uniform int   uRot;       // quarter turns, 0-3
  uniform vec2  uFlip;      // +1 or -1 per axis

  uniform float uExposure, uBrilliance, uHighlights, uShadows, uContrast;
  uniform float uBrightness, uBlack, uSaturation, uVibrance;
  uniform float uWarmth, uTint, uSharpen, uVignette, uGrain, uMono, uFade;

  vec2 srcUV(vec2 o){
    vec2 c = uCrop.xy + uCrop.zw * 0.5;
    vec2 p = uCrop.xy + o * uCrop.zw;
    vec2 d = (p - c) * uRotDims;
    float s = sin(uAngle), co = cos(uAngle);
    vec2 r = vec2(d.x * co - d.y * s, d.x * s + d.y * co) / uZoom;
    p = c + r / uRotDims;

    vec2 q = p;
    if (uRot == 1)      q = vec2(p.y, 1.0 - p.x);
    else if (uRot == 2) q = vec2(1.0 - p.x, 1.0 - p.y);
    else if (uRot == 3) q = vec2(1.0 - p.y, p.x);
    if (uFlip.x < 0.0) q.x = 1.0 - q.x;
    if (uFlip.y < 0.0) q.y = 1.0 - q.y;
    return q;
  }

  float luma(vec3 c){ return dot(c, vec3(0.2126, 0.7152, 0.0722)); }

  vec3 sampleAt(vec2 uv){
    vec3 c = texture(uTex, clamp(uv, vec2(0.0), vec2(1.0))).rgb;
    if (uSharpen > 0.001){
      vec3 blur = texture(uTex, uv + vec2( uTexel.x, 0.0)).rgb
                + texture(uTex, uv + vec2(-uTexel.x, 0.0)).rgb
                + texture(uTex, uv + vec2(0.0,  uTexel.y)).rgb
                + texture(uTex, uv + vec2(0.0, -uTexel.y)).rgb;
      c = c + (c - blur * 0.25) * uSharpen * 1.6;
    }
    return c;
  }

  void main(){
    vec2 uv = srcUV(vUV);
    vec3 c = sampleAt(uv);

    // Exposure is a light-linear multiply; everything after it is perceptual.
    c *= pow(2.0, uExposure);

    // Highlights and shadows act through a luminance mask so mid-tones hold.
    float l = luma(c);
    float hiMask = smoothstep(0.5, 1.0, l);
    float loMask = 1.0 - smoothstep(0.0, 0.5, l);
    c += uHighlights * hiMask * (1.0 - c) * 0.9;
    c += uShadows    * loMask * 0.55;

    // Brilliance lifts shadows and pulls highlights at once, the way Apple's does.
    c += uBrilliance * (loMask * 0.35 - hiMask * 0.22);

    c += uBrightness * 0.4;
    c = (c - uBlack * 0.25) / max(1.0 - uBlack * 0.25, 0.05);
    c = (c - 0.5) * (1.0 + uContrast) + 0.5;

    // Warmth and tint as opposed channel pushes.
    c.r += uWarmth * 0.12;  c.b -= uWarmth * 0.12;
    c.g += uTint   * 0.10;  c.r -= uTint   * 0.04; c.b -= uTint * 0.04;

    float l2 = luma(c);
    c = mix(vec3(l2), c, 1.0 + uSaturation);

    // Vibrance protects colours that are already saturated.
    float mx = max(c.r, max(c.g, c.b)), mn = min(c.r, min(c.g, c.b));
    float sat = mx - mn;
    c = mix(vec3(luma(c)), c, 1.0 + uVibrance * (1.0 - sat) * 1.4);

    if (uMono > 0.001) c = mix(c, vec3(luma(c)), uMono);
    if (uFade  > 0.001) c = mix(c, c * 0.86 + 0.14, uFade);

    if (uVignette > 0.001){
      vec2 d = (vUV - 0.5) * vec2(1.0, 1.0);
      float v = 1.0 - uVignette * smoothstep(0.25, 0.85, dot(d, d) * 2.0);
      c *= v;
    }
    if (uGrain > 0.001){
      float n = fract(sin(dot(vUV * uRotDims, vec2(12.9898, 78.233))) * 43758.5453);
      c += (n - 0.5) * uGrain * 0.14;
    }

    outColor = vec4(clamp(c, 0.0, 1.0), 1.0);
  }`;

  // Neutral values. Anything not listed is zero.
  const ADJUSTMENTS = [
    { k: 'exposure',   label: 'Exposure',    min: -2,  max: 2,   step: 0.01 },
    { k: 'brilliance', label: 'Brilliance',  min: -1,  max: 1,   step: 0.01 },
    { k: 'highlights', label: 'Highlights',  min: -1,  max: 1,   step: 0.01 },
    { k: 'shadows',    label: 'Shadows',     min: -1,  max: 1,   step: 0.01 },
    { k: 'contrast',   label: 'Contrast',    min: -1,  max: 1,   step: 0.01 },
    { k: 'brightness', label: 'Brightness',  min: -1,  max: 1,   step: 0.01 },
    { k: 'black',      label: 'Black point', min: -1,  max: 1,   step: 0.01 },
    { k: 'saturation', label: 'Saturation',  min: -1,  max: 1,   step: 0.01 },
    { k: 'vibrance',   label: 'Vibrance',    min: -1,  max: 1,   step: 0.01 },
    { k: 'warmth',     label: 'Warmth',      min: -1,  max: 1,   step: 0.01 },
    { k: 'tint',       label: 'Tint',        min: -1,  max: 1,   step: 0.01 },
    { k: 'sharpen',    label: 'Sharpness',   min: 0,   max: 1,   step: 0.01 },
    { k: 'vignette',   label: 'Vignette',    min: 0,   max: 1,   step: 0.01 },
    { k: 'grain',      label: 'Grain',       min: 0,   max: 1,   step: 0.01 },
  ];

  const PRESETS = [
    { name: 'Original',  v: {} },
    { name: 'Vivid',     v: { saturation: .22, contrast: .16, vibrance: .3 } },
    { name: 'Vivid Warm',v: { saturation: .2, contrast: .14, warmth: .22, vibrance: .26 } },
    { name: 'Vivid Cool',v: { saturation: .2, contrast: .14, warmth: -.22, vibrance: .26 } },
    { name: 'Dramatic',  v: { contrast: .4, shadows: -.22, highlights: -.18, saturation: -.08, black: .12 } },
    { name: 'Dramatic Warm', v: { contrast: .38, warmth: .26, shadows: -.2, black: .1 } },
    { name: 'Dramatic Cool', v: { contrast: .38, warmth: -.26, shadows: -.2, black: .1 } },
    { name: 'Mono',      v: { mono: 1, contrast: .16 } },
    { name: 'Silvertone',v: { mono: 1, contrast: .3, brightness: .06, black: .1 } },
    { name: 'Noir',      v: { mono: 1, contrast: .55, black: .3, vignette: .3 } },
    { name: 'Faded',     v: { fade: .7, contrast: -.12, saturation: -.18 } },
    { name: 'Warm Film', v: { warmth: .3, fade: .35, grain: .22, contrast: .1, saturation: -.06 } },
  ];

  const RATIOS = [
    { name: 'Free',  r: 0 }, { name: 'Original', r: -1 }, { name: '1:1', r: 1 },
    { name: '4:3',   r: 4 / 3 }, { name: '3:2', r: 3 / 2 }, { name: '16:9', r: 16 / 9 },
    { name: '3:4',   r: 3 / 4 }, { name: '2:3', r: 2 / 3 }, { name: '9:16', r: 9 / 16 },
  ];

  const NEUTRAL = () => {
    const o = { mono: 0, fade: 0 };
    ADJUSTMENTS.forEach(a => (o[a.k] = 0));
    return o;
  };

  class Editor {
    constructor() {
      this.el = null;
      this.gl = null;
      this.state = null;
    }

    async open(item, onSaved) {
      this.item = item;
      this.onSaved = onSaved;
      this.state = {
        adj: NEUTRAL(),
        preset: 0,
        rot: 0,
        flipH: false,
        flipV: false,
        angle: 0,
        crop: { x: 0, y: 0, w: 1, h: 1 },
        ratio: 0,
        tab: 'adjust',
        dirty: false,
      };

      this.mount();
      this.status('Loading full resolution…');
      try {
        this.img = await loadImage('/api/full/' + item.id);
      } catch (e) {
        this.status('Could not load this image for editing.');
        return;
      }
      this.initGL();
      this.buildPanel();
      this.render();
      this.status('');
    }

    // ------------------------------------------------------------ chrome --

    mount() {
      const el = document.createElement('div');
      el.className = 'editor';
      el.innerHTML = `
        <div class="ed-top">
          <button class="btn" data-act="cancel">${I.close(17)} Cancel</button>
          <div class="sep"></div>
          <button class="btn icon" data-act="rot-l" title="Rotate left">${I.rotL(17)}</button>
          <button class="btn icon" data-act="rot-r" title="Rotate right">${I.rotR(17)}</button>
          <button class="btn icon" data-act="flip-h" title="Flip horizontally">${I.flip(17)}</button>
          <div class="sep"></div>
          <span class="mono" style="font-size:11.5px;color:var(--text-3)" id="edName"></span>
          <span class="grow"></span>
          <span class="mono" style="font-size:11.5px;color:var(--text-3)" id="edStatus"></span>
          <button class="btn outline" data-act="reset">Reset</button>
          <button class="btn outline" data-act="save-copy">Save a copy</button>
          <button class="btn solid" data-act="save-over">Save over original</button>
        </div>
        <div class="ed-body">
          <div class="ed-stage" id="edStage">
            <canvas id="edCanvas"></canvas>
            <div class="crop-overlay hidden" id="cropOverlay"><div class="crop-box" id="cropBox">
              <div class="thirds"></div>
              <div class="h tl"></div><div class="h tr"></div><div class="h bl"></div><div class="h br"></div>
            </div></div>
          </div>
          <aside class="ed-side">
            <div class="ed-tabs">
              <button data-tab="adjust" aria-pressed="true">Adjust</button>
              <button data-tab="filters" aria-pressed="false">Filters</button>
              <button data-tab="crop" aria-pressed="false">Crop</button>
            </div>
            <div class="ed-panel" id="edPanel"></div>
          </aside>
        </div>`;
      document.body.appendChild(el);
      this.el = el;
      this.canvas = el.querySelector('#edCanvas');
      el.querySelector('#edName').textContent = this.item.name;

      el.addEventListener('click', e => {
        const t = e.target.closest('[data-act],[data-tab]');
        if (!t) return;
        if (t.dataset.tab) return this.setTab(t.dataset.tab);
        this.action(t.dataset.act);
      });
      this._key = e => {
        if (e.key === 'Escape') { e.stopPropagation(); this.action('cancel'); }
      };
      window.addEventListener('keydown', this._key, true);
      this._resize = () => this.render();
      window.addEventListener('resize', this._resize);
      this.setupCropDrag();
    }

    status(t) { const s = this.el && this.el.querySelector('#edStatus'); if (s) s.textContent = t; }

    close() {
      window.removeEventListener('keydown', this._key, true);
      window.removeEventListener('resize', this._resize);
      if (this.el) this.el.remove();
      this.el = null;
      if (this.gl) { const e = this.gl.getExtension('WEBGL_lose_context'); if (e) e.loseContext(); }
      this.gl = null;
    }

    // ---------------------------------------------------------- webgl ----

    initGL() {
      const gl = this.canvas.getContext('webgl2', { preserveDrawingBuffer: true, antialias: false });
      if (!gl) throw new Error('WebGL2 unavailable');
      this.gl = gl;

      const compile = (type, src) => {
        const s = gl.createShader(type);
        gl.shaderSource(s, src); gl.compileShader(s);
        if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s));
        return s;
      };
      const p = gl.createProgram();
      gl.attachShader(p, compile(gl.VERTEX_SHADER, VERT));
      gl.attachShader(p, compile(gl.FRAGMENT_SHADER, FRAG));
      gl.linkProgram(p);
      if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(p));
      gl.useProgram(p);
      this.prog = p;

      const buf = gl.createBuffer();
      gl.bindBuffer(gl.ARRAY_BUFFER, buf);
      gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([0, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 1]), gl.STATIC_DRAW);
      const loc = gl.getAttribLocation(p, 'a');
      gl.enableVertexAttribArray(loc);
      gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);

      const tex = gl.createTexture();
      gl.bindTexture(gl.TEXTURE_2D, tex);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
      gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, true);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, this.img);
      this.u = name => gl.getUniformLocation(p, name);
    }

    rotDims() {
      const swap = this.state.rot % 2 === 1;
      return swap ? [this.img.height, this.img.width] : [this.img.width, this.img.height];
    }

    outputSize(full) {
      const [rw, rh] = this.rotDims();
      const c = this.state.crop;
      let w = Math.max(8, Math.round(rw * c.w));
      let h = Math.max(8, Math.round(rh * c.h));
      if (full) return [w, h];
      // Preview at whatever the stage can actually show, capped for speed.
      const stage = this.el.querySelector('#edStage');
      const availW = stage.clientWidth - 44, availH = stage.clientHeight - 44;
      const s = Math.min(1, availW / w, availH / h);
      return [Math.max(8, Math.round(w * s)), Math.max(8, Math.round(h * s))];
    }

    straightenZoom() {
      const a = Math.abs(this.state.angle);
      if (a < 0.0001) return 1;
      const [rw, rh] = this.rotDims();
      const w = rw * this.state.crop.w, h = rh * this.state.crop.h;
      const c = Math.cos(a), s = Math.sin(a);
      return Math.max((w * c + h * s) / w, (h * c + w * s) / h);
    }

    render(full) {
      if (!this.gl) return;
      const gl = this.gl, st = this.state, a = st.adj;
      const [w, h] = this.outputSize(full);
      this.canvas.width = w; this.canvas.height = h;
      gl.viewport(0, 0, w, h);

      const [rw, rh] = this.rotDims();
      gl.uniform2f(this.u('uTexel'), 1 / this.img.width, 1 / this.img.height);
      gl.uniform4f(this.u('uCrop'), st.crop.x, st.crop.y, st.crop.w, st.crop.h);
      gl.uniform2f(this.u('uRotDims'), rw, rh);
      gl.uniform1f(this.u('uAngle'), st.angle);
      gl.uniform1f(this.u('uZoom'), this.straightenZoom());
      gl.uniform1i(this.u('uRot'), st.rot);
      gl.uniform2f(this.u('uFlip'), st.flipH ? -1 : 1, st.flipV ? -1 : 1);
      for (const k of ['exposure', 'brilliance', 'highlights', 'shadows', 'contrast',
        'brightness', 'black', 'saturation', 'vibrance', 'warmth', 'tint',
        'sharpen', 'vignette', 'grain', 'mono', 'fade']) {
        gl.uniform1f(this.u('u' + k[0].toUpperCase() + k.slice(1)), a[k] || 0);
      }
      gl.drawArrays(gl.TRIANGLES, 0, 6);
      this.positionCropBox();
    }

    // ---------------------------------------------------------- panels ---

    setTab(tab) {
      this.state.tab = tab;
      this.el.querySelectorAll('.ed-tabs button').forEach(b =>
        b.setAttribute('aria-pressed', String(b.dataset.tab === tab)));
      this.el.querySelector('#cropOverlay').classList.toggle('hidden', tab !== 'crop');
      this.buildPanel();
      this.render();
    }

    buildPanel() {
      const p = this.el.querySelector('#edPanel');
      const st = this.state;

      if (st.tab === 'adjust') {
        // Every slider is paired with an exact value you can type, plus arrows
        // that nudge it by one unit — a slider alone cannot hit "+12" reliably.
        p.innerHTML = ADJUSTMENTS.map(ad => {
          const v = st.adj[ad.k] || 0;
          return `<div class="adj">
            <div class="row"><label for="ad_${ad.k}">${ad.label}</label>
              <span class="num">
                <input type="number" id="n_${ad.k}" data-num="${ad.k}"
                       min="${disp(ad.min, ad)}" max="${disp(ad.max, ad)}" step="${dstep(ad)}"
                       value="${disp(v, ad)}" aria-label="${ad.label} value">
                <span class="stack">
                  <button class="stp" data-step="${ad.k}:1" aria-label="Increase ${ad.label}">${CH.up}</button>
                  <button class="stp" data-step="${ad.k}:-1" aria-label="Decrease ${ad.label}">${CH.down}</button>
                </span>
              </span></div>
            <input type="range" id="ad_${ad.k}" data-adj="${ad.k}"
                   min="${ad.min}" max="${ad.max}" step="${ad.step}" value="${v}">
          </div>`;
        }).join('');

        const apply = (k, value) => {
          const ad = ADJUSTMENTS.find(x => x.k === k);
          st.adj[k] = clamp(value, ad.min, ad.max);
          st.dirty = true;
          const slider = p.querySelector('#ad_' + k);
          const num = p.querySelector('#n_' + k);
          if (slider) slider.value = st.adj[k];
          if (num && document.activeElement !== num) num.value = disp(st.adj[k], ad);
          this.render();
        };

        p.oninput = e => {
          if (e.target.dataset.adj) return apply(e.target.dataset.adj, parseFloat(e.target.value));
          if (e.target.dataset.num) {
            const k = e.target.dataset.num;
            const ad = ADJUSTMENTS.find(x => x.k === k);
            const raw = parseFloat(e.target.value);
            if (!Number.isNaN(raw)) apply(k, undisp(raw, ad));
          }
        };
        p.onclick = e => {
          const b = e.target.closest('[data-step]');
          if (!b) return;
          const [k, dir] = b.dataset.step.split(':');
          const ad = ADJUSTMENTS.find(x => x.k === k);
          apply(k, undisp(disp(st.adj[k] || 0, ad) + (+dir) * dstep(ad), ad));
        };
        // Double-clicking a control returns it to neutral, as it does in Photos.
        p.ondblclick = e => {
          const k = e.target.dataset.adj || e.target.dataset.num;
          if (k) apply(k, 0);
        };

      } else if (st.tab === 'filters') {
        const thumb = `/api/thumb/${this.item.id}`;
        p.innerHTML = `<div class="presets">${PRESETS.map((pr, i) => `
          <button class="preset" data-preset="${i}" aria-pressed="${i === st.preset}">
            <div class="pv" style="background-image:url('${thumb}');filter:${cssApprox(pr.v)}"></div>
            <span>${pr.name}</span>
          </button>`).join('')}</div>
          <p style="font-size:11.5px;color:var(--text-3);margin:14px 0 0">
            Picking a filter replaces the sliders on the Adjust tab, which you can then tweak by hand.</p>`;
        p.onclick = e => {
          const b = e.target.closest('[data-preset]');
          if (!b) return;
          st.preset = +b.dataset.preset;
          st.adj = Object.assign(NEUTRAL(), PRESETS[st.preset].v);
          st.dirty = st.preset !== 0;
          this.buildPanel();
          this.render();
        };

      } else {
        const [rw, rh] = this.rotDims();
        const cur = Math.round(rw * st.crop.w) + ' × ' + Math.round(rh * st.crop.h);
        p.innerHTML = `
          <div class="field"><label>Aspect ratio</label>
            <div class="ratios">${RATIOS.map((r, i) =>
              `<button data-ratio="${i}" aria-pressed="${st.ratio === i}">${r.name}</button>`).join('')}</div>
          </div>
          <div class="adj">
            <div class="row"><label for="strA">Straighten</label>
              <span class="v ${st.angle ? 'set' : ''}" id="v_ang">${(st.angle * 180 / Math.PI).toFixed(1)}°</span></div>
            <input type="range" id="strA" min="-0.2618" max="0.2618" step="0.0017" value="${st.angle}">
          </div>
          <div class="field" style="margin-top:14px">
            <label>Output</label>
            <div class="urlbox" style="margin:0"><span>${cur} px</span></div>
          </div>
          <button class="btn outline" data-act="crop-reset" style="width:100%;justify-content:center;margin-top:10px">Reset crop</button>
          <p style="font-size:11.5px;color:var(--text-3);margin:14px 0 0">
            Drag inside the picture to move the crop, or grab a corner to resize it.</p>`;
        p.oninput = e => {
          if (e.target.id !== 'strA') return;
          st.angle = parseFloat(e.target.value);
          st.dirty = true;
          p.querySelector('#v_ang').textContent = (st.angle * 180 / Math.PI).toFixed(1) + '°';
          p.querySelector('#v_ang').classList.toggle('set', !!st.angle);
          this.render();
        };
        p.onclick = e => {
          const r = e.target.closest('[data-ratio]');
          if (r) { this.setRatio(+r.dataset.ratio); return; }
          if (e.target.closest('[data-act="crop-reset"]')) {
            st.crop = { x: 0, y: 0, w: 1, h: 1 }; st.angle = 0; st.ratio = 0;
            this.buildPanel(); this.render();
          }
        };
      }
    }

    setRatio(i) {
      const st = this.state;
      st.ratio = i;
      const spec = RATIOS[i];
      const [rw, rh] = this.rotDims();
      let target = spec.r;
      if (target === -1) target = this.img.width / this.img.height * (st.rot % 2 ? -1 : 1);
      if (target === -1 || spec.r === -1) target = rw / rh;
      if (spec.r === 0) { this.buildPanel(); return; }

      // Largest centred rectangle of that ratio that still fits the frame.
      const frameAR = rw / rh;
      let w, h;
      if (target > frameAR) { w = 1; h = (frameAR / target); }
      else { h = 1; w = (target / frameAR); }
      st.crop = { x: (1 - w) / 2, y: (1 - h) / 2, w, h };
      st.dirty = true;
      this.buildPanel();
      this.render();
    }

    // ------------------------------------------------------- crop drag ---

    positionCropBox() {
      if (!this.el) return;
      const box = this.el.querySelector('#cropBox');
      const ov = this.el.querySelector('#cropOverlay');
      const cr = this.canvas.getBoundingClientRect();
      const sr = this.el.querySelector('#edStage').getBoundingClientRect();
      ov.style.left = (cr.left - sr.left) + 'px';
      ov.style.top = (cr.top - sr.top) + 'px';
      ov.style.width = cr.width + 'px';
      ov.style.height = cr.height + 'px';
      // The canvas already shows only the cropped region, so the box fills it.
      box.style.left = '0px'; box.style.top = '0px';
      box.style.width = cr.width + 'px'; box.style.height = cr.height + 'px';
    }

    setupCropDrag() {
      const stage = this.el.querySelector('#edStage');
      let drag = null;

      const down = e => {
        if (this.state.tab !== 'crop') return;
        const h = e.target.classList.contains('h') ? [...e.target.classList].find(c => ['tl', 'tr', 'bl', 'br'].includes(c)) : null;
        const cr = this.canvas.getBoundingClientRect();
        drag = { h, x: e.clientX, y: e.clientY, crop: { ...this.state.crop }, cw: cr.width, ch: cr.height };
        stage.setPointerCapture(e.pointerId);
        e.preventDefault();
      };
      const move = e => {
        if (!drag) return;
        const st = this.state;
        const [rw, rh] = this.rotDims();
        // Pixels moved, converted to a fraction of the whole (rotated) frame.
        const dx = (e.clientX - drag.x) / drag.cw * drag.crop.w;
        const dy = (e.clientY - drag.y) / drag.ch * drag.crop.h;
        const c = { ...drag.crop };

        if (!drag.h) {
          c.x = clamp(c.x + dx, 0, 1 - c.w);
          c.y = clamp(c.y + dy, 0, 1 - c.h);
        } else {
          const ar = RATIOS[st.ratio].r;
          const lockAR = ar > 0 ? ar : (RATIOS[st.ratio].r === -1 ? rw / rh : 0);
          if (drag.h.includes('l')) { const nx = clamp(c.x + dx, 0, c.x + c.w - .04); c.w += c.x - nx; c.x = nx; }
          else { c.w = clamp(c.w + dx, .04, 1 - c.x); }
          if (drag.h.includes('t')) { const ny = clamp(c.y + dy, 0, c.y + c.h - .04); c.h += c.y - ny; c.y = ny; }
          else { c.h = clamp(c.h + dy, .04, 1 - c.y); }
          if (lockAR) {
            // Keep the requested ratio by driving height from width.
            const frameAR = rw / rh;
            c.h = clamp(c.w * frameAR / lockAR, .04, 1 - c.y);
            c.w = c.h * lockAR / frameAR;
          }
        }
        st.crop = c;
        st.dirty = true;
        this.render();
      };
      const up = e => { if (drag) { drag = null; try { stage.releasePointerCapture(e.pointerId); } catch (_) {} this.buildPanel(); } };

      stage.addEventListener('pointerdown', down);
      stage.addEventListener('pointermove', move);
      stage.addEventListener('pointerup', up);
      stage.addEventListener('pointercancel', up);
    }

    // --------------------------------------------------------- actions ---

    async action(a) {
      const st = this.state;
      switch (a) {
        case 'cancel':
          if (st.dirty && !confirm('Discard these edits?')) return;
          this.close();
          break;
        case 'rot-l': st.rot = (st.rot + 3) % 4; st.crop = { x: 0, y: 0, w: 1, h: 1 }; st.dirty = true; this.buildPanel(); this.render(); break;
        case 'rot-r': st.rot = (st.rot + 1) % 4; st.crop = { x: 0, y: 0, w: 1, h: 1 }; st.dirty = true; this.buildPanel(); this.render(); break;
        case 'flip-h': st.flipH = !st.flipH; st.dirty = true; this.render(); break;
        case 'reset':
          this.state = Object.assign(this.state, {
            adj: NEUTRAL(), preset: 0, rot: 0, flipH: false, flipV: false,
            angle: 0, crop: { x: 0, y: 0, w: 1, h: 1 }, ratio: 0, dirty: false,
          });
          this.buildPanel(); this.render();
          break;
        case 'save-copy': await this.save(false); break;
        case 'save-over': await this.save(true); break;
      }
    }

    async save(overwrite) {
      if (overwrite && !confirm(
        'Save over the original?\n\nThe file you have now will be moved to the bin, so this is reversible until you empty it.')) return;
      this.status('Rendering at full size…');
      await new Promise(r => setTimeout(r, 16));
      this.render(true);

      const blob = await new Promise(res => this.canvas.toBlob(res, 'image/jpeg', 0.93));
      if (!blob) { this.status('Rendering failed.'); return; }
      this.status('Saving ' + fmtBytes(blob.size) + '…');

      try {
        const r = await fetch(`/api/edit/${this.item.id}?mode=${overwrite ? 'overwrite' : 'copy'}`, {
          method: 'POST',
          headers: { 'Content-Type': 'image/jpeg' },
          body: blob,
        });
        const j = await r.json();
        if (!r.ok) throw new Error(j.error || 'save failed');
        this.status('');
        this.close();
        if (this.onSaved) this.onSaved(j.id, overwrite);
      } catch (e) {
        this.status('Save failed: ' + e.message);
      }
      this.render(false);
    }
  }

  // ------------------------------------------------------------ helpers ---

  function clamp(v, a, b) { return Math.min(b, Math.max(a, v)); }

  /* Sliders work in the shader's own units; people think in stops for exposure
     and in -100..100 for everything else. These convert between the two. */
  const isStops = ad => ad.k === 'exposure';
  function disp(v, ad) {
    return isStops(ad) ? Math.round((v || 0) * 100) / 100 : Math.round((v || 0) * 100);
  }
  function undisp(v, ad) { return isStops(ad) ? v : v / 100; }
  function dstep(ad) { return isStops(ad) ? 0.05 : 1; }

  const CH = {
    up: '<svg width="9" height="9" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3.2" stroke-linecap="round" stroke-linejoin="round"><path d="M6 15l6-6 6 6"/></svg>',
    down: '<svg width="9" height="9" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3.2" stroke-linecap="round" stroke-linejoin="round"><path d="M6 9l6 6 6-6"/></svg>',
  };
  function fmtBytes(b) {
    if (b > 1048576) return (b / 1048576).toFixed(1) + ' MB';
    return Math.round(b / 1024) + ' KB';
  }
  function loadImage(src) {
    return new Promise((res, rej) => {
      const i = new Image();
      i.crossOrigin = 'anonymous';
      i.onload = () => res(i);
      i.onerror = () => rej(new Error('image load failed'));
      i.src = src;
    });
  }
  /* A rough CSS stand-in so the filter thumbnails look right without spinning
     up twelve WebGL contexts. Only the tiles use it; the real render never does. */
  function cssApprox(v) {
    const f = [];
    if (v.mono) f.push(`grayscale(${v.mono})`);
    if (v.contrast) f.push(`contrast(${1 + v.contrast})`);
    if (v.saturation) f.push(`saturate(${1 + v.saturation})`);
    if (v.brightness) f.push(`brightness(${1 + v.brightness * .4})`);
    if (v.warmth) f.push(`sepia(${Math.max(0, v.warmth) * .5}) hue-rotate(${v.warmth < 0 ? 20 : 0}deg)`);
    if (v.fade) f.push(`opacity(${1 - v.fade * .12}) contrast(${1 - v.fade * .18})`);
    return f.join(' ') || 'none';
  }

  // Minimal icon set; the main app has its own.
  const I = {
    close: s => svg(s, '<path d="M6 6l12 12M18 6 6 18"/>'),
    rotL: s => svg(s, '<path d="M4 11a8 8 0 1 0 2.4-5.7"/><path d="M4 4v5h5"/>'),
    rotR: s => svg(s, '<path d="M20 11a8 8 0 1 1-2.4-5.7"/><path d="M20 4v5h-5"/>'),
    flip: s => svg(s, '<path d="M12 3v18M7 8 3 12l4 4zM17 8l4 4-4 4"/>'),
  };
  function svg(s, body) {
    return `<svg width="${s}" height="${s}" viewBox="0 0 24 24" fill="none" stroke="currentColor"
      stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">${body}</svg>`;
  }

  window.SpeckleEditor = new Editor();
})();
