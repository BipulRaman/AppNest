/* ━━━ AppNest landing page ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ */
(function () {
  'use strict';

  // ─── Theme toggle ────────────────────────────────────────────
  var THEME_KEY = 'appnest-theme';
  var root = document.documentElement;
  var btnTheme = document.getElementById('btnTheme');

  function setTheme(t) {
    root.setAttribute('data-theme', t);
    try { localStorage.setItem(THEME_KEY, t); } catch (e) {}
  }

  if (btnTheme) {
    btnTheme.addEventListener('click', function () {
      var current = root.getAttribute('data-theme') === 'dark' ? 'dark' : 'light';
      setTheme(current === 'dark' ? 'light' : 'dark');
    });
  }

  // Sync with system preference if user hasn't chosen
  var mql = window.matchMedia('(prefers-color-scheme: dark)');
  if (mql && typeof mql.addEventListener === 'function') {
    mql.addEventListener('change', function (e) {
      try {
        if (!localStorage.getItem(THEME_KEY)) {
          setTheme(e.matches ? 'dark' : 'light');
        }
      } catch (err) {}
    });
  }

  // ─── Mobile nav toggle ───────────────────────────────────────
  var btnNav = document.getElementById('btnNav');
  var nav = document.getElementById('siteNav');

  if (btnNav && nav) {
    btnNav.addEventListener('click', function () {
      var open = nav.classList.toggle('open');
      btnNav.setAttribute('aria-expanded', open ? 'true' : 'false');
    });

    nav.addEventListener('click', function (e) {
      if (e.target.tagName === 'A') {
        nav.classList.remove('open');
        btnNav.setAttribute('aria-expanded', 'false');
      }
    });

    document.addEventListener('click', function (e) {
      if (!nav.contains(e.target) && !btnNav.contains(e.target)) {
        if (nav.classList.contains('open')) {
          nav.classList.remove('open');
          btnNav.setAttribute('aria-expanded', 'false');
        }
      }
    });
  }

  // ─── FAQ: close other open items when one opens (accordion) ─
  var faqGroup = document.querySelectorAll('.faq details');
  faqGroup.forEach(function (d) {
    d.addEventListener('toggle', function () {
      if (d.open) {
        faqGroup.forEach(function (other) {
          if (other !== d) other.open = false;
        });
      }
    });
  });

  // ─── Active nav link on scroll ───────────────────────────────
  var navLinks = document.querySelectorAll('.site-nav a[href^="#"]');
  var sections = [];
  navLinks.forEach(function (a) {
    var id = a.getAttribute('href').slice(1);
    var el = document.getElementById(id);
    if (el) sections.push({ id: id, el: el, link: a });
  });

  if ('IntersectionObserver' in window && sections.length) {
    var observer = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        var match = sections.find(function (s) { return s.el === entry.target; });
        if (!match) return;
        if (entry.isIntersecting) {
          navLinks.forEach(function (l) { l.classList.remove('active'); });
          match.link.classList.add('active');
        }
      });
    }, { rootMargin: '-40% 0px -55% 0px', threshold: 0 });

    sections.forEach(function (s) { observer.observe(s.el); });
  }
})();
