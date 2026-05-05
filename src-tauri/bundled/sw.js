// Demo cache: stash heavy ML assets so 2nd+ visits skip the 16 MB download.
// Bump VERSION when models or runtime change to invalidate old cached blobs.
const VERSION = 'demo-cache-v12-zerocopy'

// Anything matching one of these is served cache-first (long-lived assets).
// HTML, JSON and other small/changing files fall through to network-first.
const HEAVY_PATTERNS = [
  /\/decoder\.(onnx|ort)$/,
  /\/encoder\.(onnx|ort)$/,
  /ort-wasm-.*\.(wasm|mjs)$/,
  /cdn\.jsdelivr\.net\/.*onnxruntime-web.*\/dist\//,
]

self.addEventListener('install', e => {
  // Take over immediately on first load — no need to wait for tab close.
  self.skipWaiting()
})

self.addEventListener('activate', e => {
  e.waitUntil(
    caches.keys().then(keys =>
      Promise.all(keys.filter(k => k !== VERSION).map(k => caches.delete(k)))
    ).then(() => self.clients.claim())
  )
})

self.addEventListener('fetch', e => {
  const url = e.request.url
  const heavy = HEAVY_PATTERNS.some(p => p.test(url))
  if (!heavy) return  // let the browser handle normally (HTML, JSON, etc.)

  e.respondWith((async () => {
    const cache = await caches.open(VERSION)
    const cached = await cache.match(e.request)
    if (cached) return cached
    const resp = await fetch(e.request)
    if (resp.ok) cache.put(e.request, resp.clone())
    return resp
  })())
})
