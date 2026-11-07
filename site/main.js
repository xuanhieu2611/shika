// Shika site: theme toggle, sticky nav state, scroll reveal, the live demo
// frame, copy, and the guide's section list. The paintings live in field.js.
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

  // The hero is a native capture. "Try it" swaps in the sample-data prototype,
  // the same 1400x880 window scaled to fit, and hands it the keyboard. Theme
  // changes reach it as a message, so its state survives.
  var frame = document.getElementById('demo-frame');
  var play = document.getElementById('demo-play');
  var iframe = null;
  function themeDemo() {
    if (iframe && iframe.contentWindow) iframe.contentWindow.postMessage({ type: 'shika-theme', theme: resolved() }, location.origin);
  }
  if (frame && play) {
    var fit = function () {
      frame.style.setProperty('--demo-scale', String(frame.clientWidth / 1400));
    };
    fit();
    if ('ResizeObserver' in window) new ResizeObserver(fit).observe(frame);
    else window.addEventListener('resize', fit);

    play.addEventListener('click', function () {
      if (iframe) return;
      iframe = document.createElement('iframe');
      iframe.title = 'Shika prototype with sample data. Use j and k to move, Enter to focus the terminal, Control Q to return to the cards, or Command N for a new agent.';
      iframe.src = 'demo/?embed=1&theme=' + resolved();
      iframe.addEventListener('load', function () {
        frame.classList.add('is-live');
        iframe.focus();
      });
      frame.appendChild(iframe);
    });
  }
  labelToggle();

  // The guide's section list follows the heading nearest the top of the viewport.
  var toc = document.querySelector('.guide-toc');
  if (toc && 'IntersectionObserver' in window) {
    var tocLinks = Array.prototype.filter.call(toc.querySelectorAll('a[href^="#"]'), function (a) {
      return document.getElementById(a.getAttribute('href').slice(1));
    });
    var setToc = function (id) {
      Array.prototype.forEach.call(tocLinks, function (a) {
        if (a.getAttribute('href') === '#' + id) a.setAttribute('aria-current', 'true');
        else a.removeAttribute('aria-current');
      });
    };
    if (tocLinks.length) {
      var fromHash = (location.hash || '').slice(1);
      var known = tocLinks.some(function (a) { return a.getAttribute('href') === '#' + fromHash; });
      setToc(known ? fromHash : tocLinks[0].getAttribute('href').slice(1));
      var tocObserver = new IntersectionObserver(function (entries) {
        entries.forEach(function (entry) {
          if (entry.isIntersecting) setToc(entry.target.id);
        });
      }, { rootMargin: '-15% 0px -75% 0px', threshold: 0 });
      tocLinks.forEach(function (a) {
        var section = document.getElementById(a.getAttribute('href').slice(1));
        if (section) tocObserver.observe(section);
      });
    }
  }

  // Copy a code block without shell prompts.
  document.querySelectorAll('.copy').forEach(function (copy) {
    var block = copy.closest('.code');
    var cmds = block ? block.querySelector('pre') : null;
    var label = copy.querySelector('.copy-label');
    if (!cmds) return;
    copy.addEventListener('click', function () {
      var text = cmds.innerText.split('\n').map(function (l) { return l.replace(/^\$\s*/, ''); }).join('\n').trim();
      var done = function () {
        copy.classList.add('is-done');
        if (label) label.textContent = 'Copied';
        setTimeout(function () {
          copy.classList.remove('is-done');
          if (label) label.textContent = 'Copy';
        }, 1800);
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
  });
})();
