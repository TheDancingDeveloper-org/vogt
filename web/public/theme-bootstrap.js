// Synchronous head script: apply the shell theme before first paint. Keep this
// dependency-free and same-origin so the engine can enforce script-src 'self'
// without allowing arbitrary inline script or event handlers.
(function () {
  try {
    var themes = {
      dark: 1,
      dim: 1,
      "hc-dark": 1,
      light: 1,
      soft: 1,
      sepia: 1,
      rose: 1,
      "hc-light": 1,
    };
    var selected = null;
    try {
      selected = localStorage.getItem("vogt.appTheme.v1");
    } catch (error) {
      // Storage can be blocked; system preference remains a safe fallback.
    }
    var theme;
    if (selected && themes[selected]) {
      theme = selected;
    } else {
      var dark = true;
      try {
        dark =
          !window.matchMedia || window.matchMedia("(prefers-color-scheme: dark)").matches;
      } catch (error) {
        // A missing/broken matchMedia falls back to the dark shell theme.
      }
      theme = dark ? "dark" : "light";
    }
    document.documentElement.setAttribute("data-theme", theme);
  } catch (error) {
    document.documentElement.setAttribute("data-theme", "dark");
  }
})();
