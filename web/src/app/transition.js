// Fade transitions between the app and the auth pages (login/signup/reset): fade #root out, then
// navigate client-side (fading back in) or full-reload (the splash fades the next page in).

const FADE_MS = 150;
const rootNode = () => document.getElementById("root");

// Fade #root out, resolving when done. Exported so callers can await it before their own
// teardown + reload (e.g. resetAndRedirect wiping IndexedDB).
export const fadeOut = () =>
  new Promise((resolve) => {
    const node = rootNode();
    if (!node) {
      resolve();
      return;
    }
    node.style.transition = `opacity ${FADE_MS}ms ease-out`;
    node.style.opacity = "0";
    setTimeout(resolve, FADE_MS);
  });

// Fade #root back in, then strip the inline styles (a lingering `transition` would animate future
// opacity changes). setTimeout, not rAF, so a backgrounded tab can't strand #root at opacity 0.
const fadeInRoot = () => {
  const node = rootNode();
  if (!node) {
    return;
  }
  node.style.opacity = "1";
  setTimeout(() => {
    node.style.transition = "";
    node.style.opacity = "";
  }, FADE_MS);
};

// Fade out, navigate client-side, fade the new page in (app -> login/signup, no reload).
export const fadeNavigate = (navigate, to) => {
  fadeOut().then(() => {
    navigate(to);
    fadeInRoot();
  });
};

// Fade out, then full-reload to `url` (login/signup -> app needs a reload for the per-user DB).
// The splash fades it back in.
export const fadeReload = (url) => {
  fadeOut().then(() => {
    window.location.href = url;
  });
};
