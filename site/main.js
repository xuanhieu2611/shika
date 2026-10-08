// Shika landing page: theme toggle, sticky nav state, scroll reveal, the live demo
// frame, and copy. The paintings live in field.js.
(function () {
  var root = document.documentElement;
  var reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  var systemDark = window.matchMedia('(prefers-color-scheme: dark)');

  // Theme. The head script already set it before paint; this keeps it current.
  var toggle = document.getElementById('theme-toggle');
  var resolved = function () { return root.dataset.resolvedTheme === 'dark' ? 'dark' : 'light'; };
  var labelToggle = function () {
    if (toggle) toggle.setAttribute('aria-label', resolved() === 'dark' ? 'Switch to light theme' : 'Switch to dark theme');
  };
  var setTheme = function (t, save) {
    if (save) {
      root.dataset.theme = t;
      try { localStorage.setItem('shika-theme', t); } catch (e) {}
    }
    if (root.dataset.resolvedTheme !== t) root.dataset.resolvedTheme = t;
    labelToggle();
    themeDemo();
  };
  if (toggle) {
    toggle.addEventListener('click', function () { setTheme(resolved() === 'dark' ? 'light' : 'dark', true); });
  }
  systemDark.addEventListener('change', function (e) {
    if (root.dataset.theme !== 'light' && root.dataset.theme !== 'dark') setTheme(e.matches ? 'dark' : 'light', false);
  });

  // Nav gets a frosted background once the page leaves the top.
  var nav = document.getElementById('nav');
  var top = document.getElementById('top');
  if ('IntersectionObserver' in window && nav && top) {
    new IntersectionObserver(function (entries) {
      nav.classList.toggle('is-stuck', !entries[0].isIntersecting);
    }, { rootMargin: '8px 0px 0px 0px' }).observe(top);
  }

  // Sections rise in once as they enter the viewport.
  var reveals = document.querySelectorAll('.reveal');
  if (reduceMotion || !('IntersectionObserver' in window)) {
    reveals.forEach(function (el) { el.classList.add('is-in'); });
  } else {
    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (e) {
        if (e.isIntersecting) { e.target.classList.add('is-in'); io.unobserve(e.target); }
      });
    }, { rootMargin: '0px 0px -10% 0px', threshold: 0.08 });
    reveals.forEach(function (el) { io.observe(el); });
  }

  // The sample-data prototype is 1280x800, scaled to fit its frame.
  // It loads after the page so its runtime does not delay first paint,
  // and it reloads in the page's theme when the theme changes.
  var frame = document.getElementById('demo-frame');
  var iframe = document.getElementById('demo-iframe');
  var started = false;
  function demoSrc() { return iframe.getAttribute('data-src') + '?glass=off&theme=' + resolved(); }
  function themeDemo() {
    if (!started || !iframe) return;
    var next = demoSrc();
    if (iframe.getAttribute('src') === next) return;
    frame.classList.remove('is-live');
    iframe.src = next;
  }
  if (frame && iframe) {
    var fit = function () {
      frame.style.setProperty('--demo-scale', String(frame.clientWidth / 1280));
    };
    fit();
    if ('ResizeObserver' in window) new ResizeObserver(fit).observe(frame);
    else window.addEventListener('resize', fit);

    iframe.addEventListener('load', function () {
      // Give the prototype a moment to render before revealing it over the poster.
      setTimeout(function () { frame.classList.add('is-live'); }, 350);
    });
    var start = function () { started = true; iframe.src = demoSrc(); };
    if (document.readyState === 'complete') start();
    else window.addEventListener('load', start);
  }
  labelToggle();

  // Copy the install commands without the prompts.
  var copy = document.getElementById('copy');
  var cmds = document.getElementById('install-cmds');
  var label = document.getElementById('copy-label');
  if (copy && cmds) {
    copy.addEventListener('click', function () {
      var text = cmds.innerText.split('\n').map(function (l) { return l.replace(/^\$\s*/, ''); }).join('\n').trim();
      var done = function () {
        copy.classList.add('is-done');
        label.textContent = 'Copied';
        setTimeout(function () { copy.classList.remove('is-done'); label.textContent = 'Copy'; }, 1800);
      };
      if (navigator.clipboard && window.isSecureContext) {
        navigator.clipboard.writeText(text).then(done, function () {});
      } else {
        var ta = document.createElement('textarea');
        ta.value = text; ta.setAttribute('readonly', ''); ta.style.position = 'absolute'; ta.style.left = '-9999px';
        document.body.appendChild(ta); ta.select();
        try { document.execCommand('copy'); done(); } catch (e) {}
        document.body.removeChild(ta);
      }
    });
  }
})();
