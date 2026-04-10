module.exports = {
  reactStrictMode: true,
  server: {
    port: 3001,
  },
  head: {
    defaultTitle: 'Ring - Lightweight Container Orchestrator',
    titleTemplate: '%s | Ring',
    meta: [
      { name: 'description', content: 'A lightweight container orchestrator. Deploy and manage containerized applications with declarative YAML, a REST API, and zero complexity.' },
      { name: 'viewport', content: 'width=device-width, initial-scale=1' },
      { property: 'og:title', content: 'Ring - Lightweight Container Orchestrator' },
      { property: 'og:description', content: 'Deploy and manage containerized applications with declarative YAML, a REST API, and zero complexity.' },
      { property: 'og:type', content: 'website' },
    ],
    link: [
      { rel: 'icon', type: 'image/svg+xml', href: '/images/ring-logo.svg' },
    ],
  },
};
