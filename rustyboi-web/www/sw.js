// Minimal offline app-shell cache with a user-prompted update flow. Bump CACHE
// on a breaking change to force a clean re-cache; old caches are pruned in
// `activate`.
const CACHE = "rustyboi-v4";
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

// Network-first for same-origin GETs: always serve fresh code when online, so a
// rebuilt wasm/js is picked up on a SINGLE reload (stale-while-revalidate was one
// reload behind, which broke the dev loop). The cache is an offline fallback,
// refreshed on every successful fetch.
self.addEventListener("fetch", (e) => {
  const req = e.request;
  if (req.method !== "GET" || new URL(req.url).origin !== self.location.origin) return;
  e.respondWith(
    fetch(req)
      .then((res) => {
        if (res && res.ok) {
          const copy = res.clone();
          caches.open(CACHE).then((c) => c.put(req, copy));
        }
        return res;
      })
      .catch(() => caches.open(CACHE).then((c) => c.match(req)))
  );
});
