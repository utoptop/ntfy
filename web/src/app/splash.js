// Fades out and removes the static splash (see web/index.html), called once the app has mounted and
// its data is ready. Idempotent.

// Minimum time the splash stays up, so it doesn't flash-and-vanish on warm-cache loads.
const MIN_VISIBLE_MS = 1000;

// Hide in two phases: fade the logo out, then fade the background away to reveal the app.
// APP_FADE_MS must match the #splash opacity transition in index.html.
const LOGO_FADE_MS = 300;
const APP_FADE_MS = 100;

let removed = false;

const fadeOutAndRemove = () => {
  const splash = document.getElementById("splash");
  if (!splash) {
    return;
  }

  // Phase 1: freeze the pulse at its current opacity (else stopping the animation snaps to full),
  // then fade the logo to 0.
  const img = splash.querySelector("img");
  if (img) {
    const current = getComputedStyle(img).opacity;
    img.style.opacity = current;
    img.style.animation = "none";
    img.getBoundingClientRect(); // force reflow so the fade starts from `current`
    img.style.transition = `opacity ${LOGO_FADE_MS}ms ease-out`;
    img.style.opacity = "0";
  }

  // Phase 2: lift the background to fade the app in, then remove the node.
  setTimeout(() => {
    splash.classList.add("splash-hidden");
    const remove = () => splash.remove();
    // Ignore the logo's bubbling transitionend; only the background's own fade should remove it.
    const onEnd = (event) => {
      if (event.target === splash) {
        splash.removeEventListener("transitionend", onEnd);
        remove();
      }
    };
    splash.addEventListener("transitionend", onEnd);
    setTimeout(remove, APP_FADE_MS + 100); // fallback if transitionend never fires
  }, LOGO_FADE_MS);
};

const hideSplash = () => {
  if (removed) {
    return;
  }
  removed = true;
  // performance.now() ~= how long the splash has been visible; hold until MIN_VISIBLE_MS elapses.
  const remaining = Math.max(0, MIN_VISIBLE_MS - performance.now());
  setTimeout(fadeOutAndRemove, remaining);
};

export default hideSplash;
