import { useState } from 'react';
import { NavLink } from 'aplos/navigation';
import { getAllDocs, docToUrl, humanize } from '@/lib/docs';
import '@/styles/components/sidebar.css';

interface SidebarLink {
  to: string;
  label: string;
}

interface SidebarSection {
  title: string;
  links: SidebarLink[];
}

function buildSections(): SidebarSection[] {
  const rootLinks: SidebarLink[] = [];
  const sectionsByFolder = new Map<string, SidebarLink[]>();

  for (const doc of getAllDocs()) {
    const link: SidebarLink = { to: docToUrl(doc.slug), label: doc.title };
    if (doc.segments.length <= 1) {
      rootLinks.push(link);
    } else {
      const folder = doc.segments[0];
      if (!sectionsByFolder.has(folder)) {
        sectionsByFolder.set(folder, []);
      }
      sectionsByFolder.get(folder)!.push(link);
    }
  }

  const sections: SidebarSection[] = [];
  if (rootLinks.length > 0) {
    sections.push({ title: 'Introduction', links: rootLinks });
  }
  for (const [folder, links] of sectionsByFolder) {
    sections.push({ title: humanize(folder), links });
  }
  return sections;
}

const sections = buildSections();

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
                end={link.to === '/documentation'}
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
