import { useState } from 'react';
import { NavLink } from 'aplos/navigation';
import '@/styles/components/sidebar.css';

const sections = [
  {
    title: 'Getting Started',
    links: [
      { to: '/documentation', label: 'Overview' },
      { to: '/documentation/installation', label: 'Installation' },
      { to: '/documentation/getting-started', label: 'Getting Started' },
      { to: '/documentation/getting-started/first-deployment', label: 'First Deployment' },
      { to: '/documentation/getting-started/managing-deployments', label: 'Managing Deployments' },
    ],
  },
  {
    title: 'Guides',
    links: [
      { to: '/documentation/examples', label: 'Examples' },
    ],
  },
  {
    title: 'Reference',
    links: [
      { to: '/documentation/reference', label: 'CLI Reference' },
      { to: '/documentation/api-reference', label: 'API Reference' },
    ],
  },
  {
    title: 'Help',
    links: [
      { to: '/documentation/faq', label: 'FAQ' },
    ],
  },
];

export default function DocSidebar() {
  const [open, setOpen] = useState(false);

  return (
    <aside className="doc-sidebar">
      <button
        type="button"
        className="sidebar-mobile-toggle"
        onClick={() => setOpen(!open)}
      >
        <span>Documentation</span>
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
          <path d={open ? 'M18 15l-6-6-6 6' : 'M6 9l6 6 6-6'} />
        </svg>
      </button>
      <nav className={`sidebar-nav ${open ? 'open' : ''}`}>
        {sections.map((section) => (
          <div key={section.title} className="sidebar-section">
            <div className="sidebar-title">{section.title}</div>
            {section.links.map((link) => (
              <NavLink
                key={link.to}
                to={link.to}
                end
                onClick={() => setOpen(false)}
              >
                {link.label}
              </NavLink>
            ))}
          </div>
        ))}
      </nav>
    </aside>
  );
}
