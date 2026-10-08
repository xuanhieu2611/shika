// Shika landing page: paintings dithered in one ink.
//
// Each canvas.field redraws its data-src painting with Atkinson dithering on a 2px
// grid, like an early Mac screen, in the theme's --field-ink at --field-alpha, so the
// art reads as texture and the copy stays on top. On light pages the painting's dark
// parts take ink; on dark pages its light parts do, so the picture keeps its own
// light either way.
//
// Modes (data-mode):
//   frame  The painting covers the canvas and thins out toward the copy.
//   scene  A landscape standing on the element named by data-anchor: the image row
//          data-ground="fraction,px" lands that many px below the element's top edge.
//          It reaches up under the nav, thins out toward the copy, and fades out
//          below the ground line.
// data-clear selects the copy to keep clear (matched inside the canvas's parent).
// data-levels sets the tone percentiles mapped to empty and full (default 0.25,0.985).
//
// Trail: with a mouse, moving the pointer turns the dither back into the painting, in
// its own colors at --paint-alpha, along the path it takes. The path fades over a few
// seconds, so a still pointer soon shows nothing. Not over the demo, and never where
// the dither is empty, such as behind the copy.
//
// Motion: each field develops once, from the bottom up, when it first scrolls into
// view. Nothing animates under prefers-reduced-motion, and there is no trail.
(function () {
  var root = document.documentElement;
  var canvases = document.querySelectorAll('canvas.field');
  if (!canvases.length || !HTMLCanvasElement.prototype.getContext) return;

  var reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  var hasMouse = window.matchMedia('(hover: hover) and (pointer: fine)').matches;
  // One dither cell, in CSS px.
  var CELL = 2;
  // Trail brush radius in CSS px, and the trail mask's downscale.
  var BRUSH = 72, MASK = 4;
  // The trail fades by FADE every FADE_EVERY frames and is cleared after LINGER
  // frames without movement (about 3 seconds to fade, 4 to clear at 60 fps).
  // Fading in larger, rarer steps keeps 8-bit alpha from stalling at a faint residue.
  var FADE = 0.05, FADE_EVERY = 4, LINGER = 240;

  function rng(seed) {
    return function () {
      seed |= 0; seed = (seed + 0x6d2b79f5) | 0;
      var t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
      t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
  }
  function smooth(a, b, x) {
    var t = Math.min(1, Math.max(0, (x - a) / (b - a)));
    return t * t * (3 - 2 * t);
  }
  function hexRgb(hex, fallback) {
    var m = /^#?([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex);
    return m ? [parseInt(m[1], 16), parseInt(m[2], 16), parseInt(m[3], 16)] : fallback;
  }

  function Field(canvas, index) {
    var a = function (name) { return canvas.getAttribute(name); };
    this.canvas = canvas;
    this.ctx = canvas.getContext('2d');
    this.mode = a('data-mode') || 'frame';
    this.src = a('data-src') || '';
    this.clearSel = a('data-clear') || '';
    this.anchorSel = a('data-anchor') || '';
    var lv = (a('data-levels') || '0.25,0.985').split(',');
    this.levels = [parseFloat(lv[0]), parseFloat(lv[1])];
    this.seed = 1309 + index * 7919;
    this.img = null;
    this.layer = null;
    this.visible = true;
    this.revealed = reduceMotion;
  }

  // The copy to keep clear, as a padded ellipse in px.
  Field.prototype.clearing = function () {
    if (!this.clearSel) return null;
    var a = { left: Infinity, top: Infinity, right: -Infinity, bottom: -Infinity };
    Array.prototype.forEach.call(this.canvas.parentElement.querySelectorAll(this.clearSel), function (el) {
      var q = el.getBoundingClientRect();
      if (!q.width) return;
      a.left = Math.min(a.left, q.left); a.top = Math.min(a.top, q.top);
      a.right = Math.max(a.right, q.right); a.bottom = Math.max(a.bottom, q.bottom);
    });
    if (a.left === Infinity) return null;
    var b = this.canvas.getBoundingClientRect();
    return {
      cx: (a.left + a.right) / 2 - b.left,
      cy: (a.top + a.bottom) / 2 - b.top,
      rx: (a.right - a.left) / 2 + 40,
      ry: (a.bottom - a.top) / 2 + 40
    };
  };

  // Where the image goes, in px: [x, y, width, height].
  Field.prototype.place = function () {
    var w = this.w, h = this.h, iw = this.img.naturalWidth, ih = this.img.naturalHeight;
    var anchor = this.mode === 'scene' && this.canvas.parentElement.querySelector(this.anchorSel);
    if (!anchor) {
      var c = Math.max(w / iw, h / ih);
      return [(w - iw * c) / 2, (h - ih * c) / 2, iw * c, ih * c];
    }
    var g = (this.canvas.getAttribute('data-ground') || '0.7,0').split(',');
    var fy = parseFloat(g[0]), gy = anchor.getBoundingClientRect().top - this.canvas.getBoundingClientRect().top + parseFloat(g[1]);
    // Wide enough for the page, and tall enough to reach up under the nav.
    var s = Math.max(w / iw, (gy - 40) / (fy * ih));
    this.ground = gy;
    return [(w - iw * s) / 2, gy - fy * ih * s, iw * s, ih * s];
  };

  // How much of the picture shows at a point in px, 0 to 1.
  Field.prototype.weight = function (x, y, clear) {
    var h = this.h, e = 2;
    if (clear) {
      var ex = (x - clear.cx) / clear.rx, ey = (y - clear.cy) / clear.ry;
      e = Math.sqrt(ex * ex + ey * ey);
    }
    if (this.mode === 'scene') {
      // Thin toward the copy and under the nav; fade out below the ground, where the
      // demo covers it, and before the image ends.
      var end = Math.min(this.ground + 360, this.box[1] + this.box[3]);
      return smooth(0.9, 1.45, e) * smooth(64, 150, y) * (1 - smooth(end - 220, end, y));
    }
    // Thin toward the copy, under the nav, and at the bottom edge.
    return smooth(0.95, 1.4, e) * smooth(64, 160, y) * (1 - smooth(h - 140, h - 10, y));
  };

  // Tone per cell, 0 (empty) to 1 (full ink), for a grid of cw x ch px cells.
  Field.prototype.sample = function (cw, ch) {
    var w = this.w, h = this.h;
    var cols = Math.ceil(w / cw), rows = Math.ceil(h / ch);
    var c = document.createElement('canvas');
    c.width = cols; c.height = rows;
    var g = c.getContext('2d', { willReadFrequently: true });
    g.imageSmoothingQuality = 'high';
    // Placed in px, then scaled down to one pixel per cell.
    var b = this.box = this.place();
    g.drawImage(this.img, b[0] / cw, b[1] / ch, b[2] / cw, b[3] / ch);
    var px = g.getImageData(0, 0, cols, rows).data;

    var n = cols * rows, v = new Float32Array(n), inked = [];
    for (var i = 0; i < n; i++) {
      var lum = (0.2126 * px[i * 4] + 0.7152 * px[i * 4 + 1] + 0.0722 * px[i * 4 + 2]) / 255;
      var a = px[i * 4 + 3] / 255;
      v[i] = a * (this.dark ? lum : 1 - lum);
      if (v[i] > 0.02) inked.push(v[i]);
    }
    // A painting is mostly mid-tones, so stretch its range to the percentiles.
    var lo = 0, hi = 1;
    if (inked.length) {
      inked.sort(function (p, q) { return p - q; });
      lo = inked[Math.floor(this.levels[0] * (inked.length - 1))];
      hi = inked[Math.floor(this.levels[1] * (inked.length - 1))];
      if (hi - lo < 0.05) hi = lo + 0.05;
    }
    var clear = this.clearing();
    for (var y = 0; y < rows; y++) {
      for (var x = 0; x < cols; x++) {
        var k = y * cols + x;
        var t = Math.min(1, Math.max(0, (v[k] - lo) / (hi - lo)));
        // Light pages ink the shadows only, so trunks and the deer read as shapes.
        if (!this.dark) t = Math.pow(t, 1.6);
        v[k] = t * this.weight((x + 0.5) * cw, (y + 0.5) * ch, clear);
      }
    }
    return { cols: cols, rows: rows, t: v };
  };

  // Draw the finished picture, opaque, into g (CSS px coordinates).
  Field.prototype.dither = function (g) {
    var S = this.sample(CELL, CELL), cols = S.cols, rows = S.rows, t = S.t;
    // Atkinson: pass on three quarters of the error, which keeps the highlights open.
    var out = new ImageData(cols, rows), d = out.data, ink = this.rgb;
    for (var y = 0; y < rows; y++) {
      for (var x = 0; x < cols; x++) {
        var i = y * cols + x, old = t[i], on = old > 0.5, err = (old - (on ? 1 : 0)) / 8;
        if (on) { d[i * 4] = ink[0]; d[i * 4 + 1] = ink[1]; d[i * 4 + 2] = ink[2]; d[i * 4 + 3] = 255; }
        if (x + 1 < cols) t[i + 1] += err;
        if (x + 2 < cols) t[i + 2] += err;
        if (y + 1 < rows) {
          if (x > 0) t[i + cols - 1] += err;
          t[i + cols] += err;
          if (x + 1 < cols) t[i + cols + 1] += err;
        }
        if (y + 2 < rows) t[i + cols * 2] += err;
      }
    }
    var c = document.createElement('canvas');
    c.width = cols; c.height = rows;
    c.getContext('2d').putImageData(out, 0, 0);
    g.imageSmoothingEnabled = false;
    g.drawImage(c, 0, 0, cols * CELL, rows * CELL);
  };

  // Render the picture into an offscreen layer at device resolution.
  Field.prototype.build = function () {
    var w = this.canvas.clientWidth, h = this.canvas.clientHeight;
    if (!w || !h || !this.img) return false;
    var dpr = this.dpr = Math.min(window.devicePixelRatio || 1, 2);
    this.w = w; this.h = h;
    this.canvas.width = Math.round(w * dpr);
    this.canvas.height = Math.round(h * dpr);

    var cs = getComputedStyle(root);
    this.ink = cs.getPropertyValue('--field-ink').trim() || '#6f8466';
    this.rgb = hexRgb(this.ink, [111, 132, 102]);
    this.alpha = parseFloat(cs.getPropertyValue('--field-alpha')) || 0.5;
    this.paintAlpha = parseFloat(cs.getPropertyValue('--paint-alpha')) || 1;
    this.dark = root.dataset.resolvedTheme === 'dark';

    var layer = document.createElement('canvas');
    layer.width = this.canvas.width; layer.height = this.canvas.height;
    var g = layer.getContext('2d');
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    this.dither(g);
    this.layer = layer;
    if (this.trail) this.buildColor();

    // Reveal order on a coarse grid: the bottom first, then upward, with scatter.
    var r = rng(this.seed + 1);
    var mw = this.mw = Math.ceil(w / 12), mh = this.mh = Math.ceil(h / 12);
    var order = this.order = new Float32Array(mw * mh);
    for (var i = 0; i < order.length; i++) order[i] = 0.55 * (1 - Math.floor(i / mw) / mh) + 0.45 * r();
    return true;
  };

  // The painting in color, placed like the dither and faded by the same weights, so
  // the trail never shows color where the dither is empty.
  Field.prototype.buildColor = function () {
    var w = this.w, h = this.h, b = this.box, clear = this.clearing();
    var c = document.createElement('canvas');
    c.width = this.canvas.width; c.height = this.canvas.height;
    var g = c.getContext('2d');
    g.imageSmoothingQuality = 'high';
    g.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    g.drawImage(this.img, b[0], b[1], b[2], b[3]);
    // A coarse weight map, scaled up smoothly, is plenty for a soft fade.
    var mw = Math.ceil(w / 4), mh = Math.ceil(h / 4);
    var m = document.createElement('canvas');
    m.width = mw; m.height = mh;
    var mg = m.getContext('2d'), md = mg.createImageData(mw, mh);
    for (var y = 0; y < mh; y++) {
      for (var x = 0; x < mw; x++) md.data[(y * mw + x) * 4 + 3] = Math.round(this.weight(x * 4 + 2, y * 4 + 2, clear) * 255);
    }
    mg.putImageData(md, 0, 0);
    g.setTransform(1, 0, 0, 1, 0, 0);
    g.globalCompositeOperation = 'destination-in';
    g.drawImage(m, 0, 0, c.width, c.height);
    this.color = c;
    // Where the pointer has been, at a quarter of the resolution; scaled up smoothly.
    this.mask = document.createElement('canvas');
    this.mask.width = Math.ceil(w / MASK); this.mask.height = Math.ceil(h / MASK);
    this.trail.idle = LINGER;
  };

  // Draw the layer with every block whose reveal threshold is below p.
  Field.prototype.paint = function (p) {
    var ctx = this.ctx, W = this.canvas.width, H = this.canvas.height;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, W, H);
    if (!this.layer || p <= 0) return;
    ctx.globalAlpha = this.alpha;
    ctx.drawImage(this.layer, 0, 0);
    ctx.globalAlpha = 1;
    if (p >= 1.1) { this.paintTrail(); return; }
    var m = document.createElement('canvas');
    m.width = this.mw; m.height = this.mh;
    var mg = m.getContext('2d'), md = mg.createImageData(this.mw, this.mh);
    for (var i = 0; i < this.order.length; i++) md.data[i * 4 + 3] = Math.round(smooth(this.order[i], this.order[i] + 0.1, p) * 255);
    mg.putImageData(md, 0, 0);
    ctx.globalCompositeOperation = 'destination-in';
    ctx.drawImage(m, 0, 0, W, H);
    ctx.globalCompositeOperation = 'source-over';
  };

  // The trail: carve it out of the dither, then fill it with the painting.
  Field.prototype.paintTrail = function () {
    var T = this.trail;
    if (!T || !this.color || T.idle >= LINGER) return;
    var ctx = this.ctx, W = this.canvas.width, H = this.canvas.height;
    ctx.globalCompositeOperation = 'destination-out';
    ctx.drawImage(this.mask, 0, 0, W, H);
    ctx.globalAlpha = 1;
    ctx.globalCompositeOperation = 'source-over';
    var t = this.trailCanvas || (this.trailCanvas = document.createElement('canvas'));
    if (t.width !== W || t.height !== H) { t.width = W; t.height = H; }
    var tg = t.getContext('2d');
    tg.globalCompositeOperation = 'copy';
    tg.drawImage(this.color, 0, 0);
    tg.globalCompositeOperation = 'destination-in';
    tg.drawImage(this.mask, 0, 0, W, H);
    ctx.globalAlpha = this.paintAlpha;
    ctx.drawImage(t, 0, 0);
    ctx.globalAlpha = 1;
  };

  // Brush the trail from a to b (px). Ink follows the distance moved, so slow and
  // fast strokes leave the same trail and a still pointer leaves none.
  Field.prototype.stroke = function (a, b) {
    if (!this.mask) return;
    var g = this.mask.getContext('2d'), r = BRUSH / MASK, step = BRUSH * 0.3;
    var dx = b[0] - a[0], dy = b[1] - a[1], dist = Math.sqrt(dx * dx + dy * dy);
    if (dist < 0.5) return;
    var n = Math.ceil(dist / step), k = 0.8 * Math.min(1, dist / n / step);
    for (var i = 1; i <= n; i++) {
      var x = (a[0] + dx * i / n) / MASK, y = (a[1] + dy * i / n) / MASK;
      var grad = g.createRadialGradient(x, y, 0, x, y, r);
      grad.addColorStop(0, 'rgba(0,0,0,' + k + ')');
      grad.addColorStop(0.55, 'rgba(0,0,0,' + k * 0.85 + ')');
      grad.addColorStop(1, 'rgba(0,0,0,0)');
      g.fillStyle = grad;
      g.fillRect(x - r, y - r, r * 2, r * 2);
    }
    this.trail.idle = 0;
    this.trail.dirty = true;
    this.runTrail();
  };

  // Fade the trail a little each frame until it is gone.
  Field.prototype.runTrail = function () {
    var self = this, T = this.trail;
    if (T.raf) return;
    function frame() {
      var g = self.mask.getContext('2d');
      T.idle++;
      if (T.idle >= LINGER) {
        g.clearRect(0, 0, self.mask.width, self.mask.height);
        T.dirty = true;
      } else if (T.idle % FADE_EVERY === 0) {
        g.globalCompositeOperation = 'destination-out';
        g.fillStyle = 'rgba(0,0,0,' + FADE + ')';
        g.fillRect(0, 0, self.mask.width, self.mask.height);
        g.globalCompositeOperation = 'source-over';
        T.dirty = true;
      }
      if (T.dirty && !self.revealing) self.paint(self.revealed ? 2 : 0);
      T.dirty = false;
      T.raf = T.idle < LINGER ? requestAnimationFrame(frame) : 0;
    }
    T.raf = requestAnimationFrame(frame);
  };

  Field.prototype.watchPointer = function () {
    var self = this, host = this.canvas.parentElement;
    var T = this.trail = { last: null, idle: LINGER, raf: 0, dirty: false };
    if (this.layer) this.buildColor();
    host.addEventListener('pointermove', function (e) {
      if (e.pointerType && e.pointerType !== 'mouse') return;
      // Not over the demo, which takes its own pointer events.
      if (e.target.closest && e.target.closest('.demo')) { T.last = null; return; }
      var b = self.canvas.getBoundingClientRect(), p = [e.clientX - b.left, e.clientY - b.top];
      if (T.last) self.stroke(T.last, p);
      T.last = p;
    });
    host.addEventListener('pointerleave', function () { T.last = null; });
    // After a scroll the last point no longer sits under the pointer.
    window.addEventListener('scroll', function () { T.last = null; }, { passive: true });
  };

  Field.prototype.reveal = function () {
    var self = this, start = performance.now(), dur = 1600;
    this.revealing = true;
    function frame(now) {
      var t = Math.min(1, (now - start) / dur);
      self.paint(t < 1 ? (1 - Math.pow(1 - t, 3)) * 1.1 : 2);
      if (t < 1) self.raf = requestAnimationFrame(frame);
      else { self.revealing = false; self.revealed = true; }
    }
    this.raf = requestAnimationFrame(frame);
  };

  Field.prototype.refresh = function () {
    cancelAnimationFrame(this.raf);
    this.revealing = false;
    if (!this.build()) return;
    if (!this.revealed && this.visible) this.reveal();
    else this.paint(this.revealed ? 2 : 0);
  };

  function loadImage(src) {
    return new Promise(function (resolve) {
      if (!src) return resolve(null);
      var img = new Image();
      img.decoding = 'async';
      img.onload = function () { resolve(img); };
      img.onerror = function () { resolve(null); };
      img.src = src;
    });
  }

  var fields = Array.prototype.map.call(canvases, function (c, i) { return new Field(c, i); });
  var refreshAll = function () { fields.forEach(function (f) { f.refresh(); }); };

  Promise.all(fields.map(function (f) { return loadImage(f.src).then(function (img) { f.img = img; }); })).then(function () {
    fields.forEach(function (f) {
      // Each field develops the first time it scrolls into view.
      f.visible = false;
      f.refresh();
      if ('IntersectionObserver' in window) {
        new IntersectionObserver(function (entries) {
          f.visible = entries[0].isIntersecting;
          if (f.visible && !f.revealed && !f.revealing && f.layer) f.reveal();
        }, { rootMargin: '0px 0px -10% 0px' }).observe(f.canvas);
      } else if (!f.revealed) {
        f.visible = true; f.reveal();
      }
      if (hasMouse && !reduceMotion) f.watchPointer();
    });

    // Rebuild on width changes; the picture depends on the width.
    var timer = 0, lastW = root.clientWidth;
    var onResize = function () {
      var w = root.clientWidth;
      if (w === lastW) return;
      lastW = w;
      clearTimeout(timer);
      timer = setTimeout(refreshAll, 160);
    };
    if ('ResizeObserver' in window) new ResizeObserver(onResize).observe(root);
    else window.addEventListener('resize', onResize);

    // The theme changes the ink and which tones take it.
    new MutationObserver(refreshAll).observe(root, { attributes: true, attributeFilter: ['data-resolved-theme'] });
  });
})();
