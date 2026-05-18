// Dynamic namespace audit page: skip prerendering (no fixed namespace names
// to enumerate at build time). The SPA fallback served by adapter-static
// routes the request to this page at runtime.
export const prerender = false;
export const ssr = false;
