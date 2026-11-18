// Applies the stored or system theme before first paint (same rule as src/shell/theme.ts).
(function () {
  var theme = null;
  try {
    theme = localStorage.getItem('postern.theme.v1');
  } catch (e) {}
  if (theme !== 'dark' && theme !== 'light') {
    theme = window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
  }
  document.documentElement.dataset.theme = theme;
  var meta = document.querySelector('meta[name="theme-color"]');
  if (meta) meta.setAttribute('content', theme === 'light' ? '#f2f0e9' : '#0b0e0f');
})();
