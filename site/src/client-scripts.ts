export const THEME_BOOTSTRAP_SCRIPT = String.raw`(function() {
  var savedTheme = localStorage.getItem('visor-theme');
  var prefersDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
  document.documentElement.setAttribute(
    'data-theme',
    savedTheme || (prefersDark ? 'dark' : 'light')
  );
})();`

export const INTERACTION_SCRIPT = String.raw`(function() {
  var root = document.documentElement;
  var toggle = document.getElementById('theme-toggle');
  if (toggle) {
    toggle.addEventListener('click', function() {
      var current = root.getAttribute('data-theme');
      var next = current === 'dark' ? 'light' : 'dark';
      root.setAttribute('data-theme', next);
      localStorage.setItem('visor-theme', next);
    });
  }

  var hamburger = document.getElementById('nav-hamburger');
  var navLinks = document.getElementById('nav-links');
  if (hamburger && navLinks) {
    hamburger.addEventListener('click', function() {
      navLinks.classList.toggle('open');
    });
  }

  var revealNodes = document.querySelectorAll('.reveal');
  if (!('IntersectionObserver' in window)) {
    revealNodes.forEach(function(node) {
      node.classList.add('revealed');
    });
    return;
  }

  var observer = new IntersectionObserver(
    function(entries) {
      entries.forEach(function(entry) {
        if (entry.isIntersecting) {
          entry.target.classList.add('revealed');
          observer.unobserve(entry.target);
        }
      });
    },
    { threshold: 0.1 }
  );

  revealNodes.forEach(function(node) {
    observer.observe(node);
  });
})();`
