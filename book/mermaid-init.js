// Renders Mermaid diagrams in the palette of the book's current theme.
//
// Left to itself Mermaid renders in its light theme regardless of the page, so
// a diagram on a dark page arrives as a white slab. It also has no way to be
// re-themed in place: rendering replaces the <pre> contents with SVG, so the
// original definition has to be kept if the reader switches theme later.
//
// mdBook emits no event when the theme changes -- it just swaps a class on
// <html> -- so the switch is picked up with a MutationObserver.

const DARK_THEMES = ["navy", "coal", "ayu"];

// Keyed by element, holding the Mermaid source each <pre> was rendered from.
const sources = new WeakMap();

function isDark() {
  const classes = document.documentElement.classList;
  if (DARK_THEMES.some((theme) => classes.contains(theme))) {
    return true;
  }
  // Readers without JavaScript never get a theme class, and neither does the
  // first paint before mdBook's inline script runs.
  const named = ["light", "rust", ...DARK_THEMES].some((t) => classes.contains(t));
  return !named && window.matchMedia("(prefers-color-scheme: dark)").matches;
}

// Mermaid's "base" theme takes an explicit palette, which is the only way to
// match the book's colours rather than approximate them.
function themeVariables(dark) {
  return dark
    ? {
        background: "#0E1014",
        primaryColor: "#1E222A",
        primaryTextColor: "#E7E3DA",
        primaryBorderColor: "#3A3F4A",
        secondaryColor: "#171A20",
        tertiaryColor: "#14161B",
        lineColor: "#8B909B",
        textColor: "#E7E3DA",
        mainBkg: "#1E222A",
        nodeBorder: "#3A3F4A",
        clusterBkg: "#14161B",
        clusterBorder: "#262A33",
        titleColor: "#E0A340",
        edgeLabelBackground: "#0E1014",
      }
    : {
        background: "#FCFBF8",
        primaryColor: "#FFFFFF",
        primaryTextColor: "#16181D",
        primaryBorderColor: "#C9C4B8",
        secondaryColor: "#F7F4EC",
        tertiaryColor: "#F1EFE9",
        lineColor: "#62666F",
        textColor: "#16181D",
        mainBkg: "#FFFFFF",
        nodeBorder: "#C9C4B8",
        clusterBkg: "#F7F5F0",
        clusterBorder: "#E5E1D8",
        titleColor: "#8A5A12",
        edgeLabelBackground: "#FCFBF8",
      };
}

function render() {
  const diagrams = document.querySelectorAll("pre.mermaid");
  if (diagrams.length === 0) {
    return;
  }

  for (const diagram of diagrams) {
    // Keep the definition from the first pass; later passes render from it.
    if (!sources.has(diagram)) {
      sources.set(diagram, diagram.textContent);
    } else {
      diagram.textContent = sources.get(diagram);
      diagram.removeAttribute("data-processed");
      diagram.removeAttribute("data-mermaid-processed");
    }
  }

  const dark = isDark();
  mermaid.initialize({
    startOnLoad: false,
    theme: "base",
    darkMode: dark,
    fontFamily: '"IBM Plex Sans", sans-serif',
    themeVariables: themeVariables(dark),
  });
  // The vendored bundle is mermaid 8.13, whose entry point is init(); run()
  // only arrived in mermaid 10. Prefer run() so a future bundle bump works.
  if (typeof mermaid.run === "function") {
    mermaid.run({ nodes: diagrams });
  } else {
    mermaid.init(undefined, diagrams);
  }
}

render();

new MutationObserver(render).observe(document.documentElement, {
  attributes: true,
  attributeFilter: ["class"],
});
