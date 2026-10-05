// Shika landing page: sticky nav state, scroll reveal, the live demo frame, and copy.
(function () {
  var reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

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

  // The demo is the designer's prototype at 1280x800, scaled to fit its frame.
  // It loads after the page so its runtime does not delay first paint.
  var frame = document.getElementById('demo-frame');
  var iframe = document.getElementById('demo-iframe');
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
    var start = function () { iframe.src = iframe.getAttribute('data-src'); };
    if (document.readyState === 'complete') start();
    else window.addEventListener('load', start);
  }

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
