// Minimal offline app-shell cache with a user-prompted update flow. Bump CACHE
// on a breaking change to force a clean re-cache; old caches are pruned in
// `activate`.
const CACHE = "rustyboi-v1";
const SHELL = [
  "./",
  "./index.html",
  "./worker.js",
  "./manifest.webmanifest",
  "./icon-192.png",
  "./icon-512.png",
  "./apple-touch-icon.png",
  "./pkg/rustyboi_web.js",
  "./pkg/rustyboi_web_bg.wasm",
];

self.addEventListener("install", (e) => {
  // Do NOT skipWaiting here: a new worker parks in `waiting` until the page
  // tells it to activate (the "Update available" prompt), so we never swap code
  // out from under a running game without the user's say-so.
  e.waitUntil(caches.open(CACHE).then((c) => c.addAll(SHELL)));
});

self.addEventListener("activate", (e) => {
  e.waitUntil(
    caches.keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim())
  );
});

// The page posts this when the user accepts the update; take over immediately.
self.addEventListener("message", (e) => {
  if (e.data === "SKIP_WAITING") self.skipWaiting();
});

// Stale-while-revalidate for same-origin GETs: serve the cached copy instantly
// (works fully offline) while fetching a fresh copy in the background, so a
// rebuilt wasm/js is picked up on the next reload without a manual cache bump.
self.addEventListener("fetch", (e) => {
  const req = e.request;
  if (req.method !== "GET" || new URL(req.url).origin !== self.location.origin) return;
  e.respondWith(
    caches.open(CACHE).then((cache) =>
      cache.match(req).then((hit) => {
        const fetching = fetch(req)
          .then((res) => {
            if (res && res.ok) cache.put(req, res.clone());
            return res;
          })
          .catch(() => hit);
        return hit || fetching;
      })
    )
  );
});
