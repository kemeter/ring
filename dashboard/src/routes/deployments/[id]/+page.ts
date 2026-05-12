// Dynamic deployment-ID page: skip prerendering (no fixed ids to enumerate
// at build time). The SPA fallback served by adapter-static will route the
// request to this page at runtime.
export const prerender = false;
export const ssr = false;
