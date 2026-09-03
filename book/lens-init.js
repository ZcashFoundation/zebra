// Loads the KnowledgeLens documentation assistant, on request only.
//
// mdBook's `additional-js` emits a bare <script src>, with no way to set the
// data-* attributes Lens reads its config from, so the widget is loaded from a
// script we build here instead. Keep this file listed in book.toml.
//
// Nothing is requested from the vendor until a reader opts in. Left to itself
// the bundle boots on page load, mints a persistent id in localStorage and
// beacons every page view to the vendor's analytics endpoint, which is not a
// reasonable default for Zebra's readers. So we suppress the automatic boot,
// load the script only when someone asks for it, and start it by hand.

const LENS_CDN = "https://cdn.knowledgelens.ai/lens.js";
const LENS_OPT_IN_KEY = "zebra.lens.opt-in";

// The bundle skips its own boot when this is set, letting us call Lens.boot()
// ourselves below. Set before the script can load, not just before we inject.
window.__lensManualBoot = true;

function configureLens(script) {
  script.src = LENS_CDN;
  script.dataset.api = "https://api.knowledgelens.ai";
  // Not a credential, despite the vendor calling it an "embed secret": this
  // file is served to every visitor, so the id below is public by design.
  // Access is controlled by the allowed-origins list in the Lens dashboard,
  // not by keeping this string hidden. Do not move it into a CI secret -- that
  // would protect nothing, and a substitution that silently no-ops would leave
  // the published book querying the vendor's `demo_kb` default instead.
  script.dataset.kb = "9d9c4ced-405e-4d2f-8fdd-64a8c8010568";
  script.dataset.mode = "floating";
  script.dataset.theme = "dark-lens";
  // Show the launcher on load, and open straight into chat instead of making
  // the reader drag-select a region of the page first.
  script.dataset.activator = "true";
  script.dataset.defaultSelection = "false";
  script.dataset.activatorIcon = "robot";
  script.dataset.draggable = "true";
}

function rememberOptIn() {
  // First-party only: this never leaves the browser, and is not the vendor's
  // `lens.distinctId.v1` identifier, which we avoid creating at all.
  try {
    localStorage.setItem(LENS_OPT_IN_KEY, "1");
  } catch {
    // Private browsing, or storage disabled. The reader just opts in again.
  }
}

function hasOptedIn() {
  try {
    return localStorage.getItem(LENS_OPT_IN_KEY) === "1";
  } catch {
    return false;
  }
}

// `boot` must be called with an argument: the bundle initialises its analytics
// and persistent id only on the no-argument path, so passing a config object
// -- even an empty one, with the real config coming from the data-* attributes
// -- leaves the tracking uninitialised while the widget itself works normally.
function bootLens(openAfterBoot) {
  window.Lens.boot({});
  if (openAfterBoot) {
    window.LensAI.open();
  }
}

function loadLens(openAfterBoot, { onSuccess, onFailure } = {}) {
  const script = document.createElement("script");
  configureLens(script);
  script.addEventListener("load", () => {
    // Manual boot relies on the vendor's internals, and the CDN URL is
    // unversioned, so treat a changed bundle as a load failure rather than
    // leaving the reader with a button stuck on "Loading…".
    try {
      bootLens(openAfterBoot);
    } catch {
      onFailure?.();
      return;
    }
    onSuccess?.();
  });
  script.addEventListener("error", () => onFailure?.());
  document.body.append(script);
}

function renderOptInButton() {
  const button = document.createElement("button");
  button.type = "button";
  button.id = "lens-opt-in";
  button.textContent = "Ask the docs";
  button.title = "Loads the KnowledgeLens assistant from a third-party service";
  button.setAttribute(
    "aria-label",
    "Ask the docs assistant. Loads the KnowledgeLens assistant from a third-party service.",
  );

  button.addEventListener("click", () => {
    button.disabled = true;
    button.textContent = "Loading…";
    rememberOptIn();
    loadLens(true, {
      // The vendor renders its own launcher once booted, so ours steps aside.
      onSuccess: () => button.remove(),
      onFailure: () => {
        button.disabled = false;
        button.textContent = "Ask the docs";
      },
    });
  });

  document.body.append(button);
}

if (hasOptedIn()) {
  loadLens(false);
} else {
  renderOptInButton();
}
