(function () {
  try {
    var THEMES = {
      dark: 1, dim: 1, "hc-dark": 1,
      light: 1, soft: 1, sepia: 1, rose: 1, "hc-light": 1,
    };
    var sel = null;
    try {
      sel = localStorage.getItem("vogt.appTheme.v1");
    } catch (e) { /* storage blocked */ }
    var theme;
    if (sel && THEMES[sel]) {
      theme = sel;
    } else {
      var dark = true;
      try {
        dark = !window.matchMedia
          || window.matchMedia("(prefers-color-scheme: dark)").matches;
      } catch (e) { /* matchMedia blocked */ }
      theme = dark ? "dark" : "light";
    }
    document.documentElement.setAttribute("data-theme", theme);
  } catch (e) {
    document.documentElement.setAttribute("data-theme", "dark");
  }
})();
