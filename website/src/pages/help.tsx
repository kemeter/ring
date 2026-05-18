import Head from 'aplos/head';
import { Link } from 'aplos/navigation';
import '@/styles/components/help.css';

const SITE_URL = 'https://kemeter.github.io/ring';
const HELP_TITLE = 'Help & Support — Ring';
const HELP_DESCRIPTION =
  'Get help with Ring: community support on GitHub, documentation, bug and security reporting, plus commercial support by Alpacode.';

export const meta = {
  title: HELP_TITLE,
  description: HELP_DESCRIPTION,
  canonical: `${SITE_URL}/help`,
  keywords: ['ring support', 'workload orchestrator help', 'commercial support', 'alpacode'],
  og: {
    title: HELP_TITLE,
    description: HELP_DESCRIPTION,
    type: 'website',
    url: `${SITE_URL}/help`,
  },
  twitter: {
    card: 'summary_large_image',
    title: HELP_TITLE,
    description: HELP_DESCRIPTION,
  },
};

export default function HelpPage() {
  return (
    <>
      <Head>
        <title>Help & Support - Ring</title>
      </Head>

      {/* Top: Help & Support + 3 cards */}
      <section className="help">
        <div className="container">
          <div className="help-head">
            <span className="help-eyebrow">Ring</span>
            <h1>Help &amp; Support</h1>
            <p>Find answers, read the docs, or get help from the community.</p>
          </div>
          <div className="help-grid">
            <article className="help-card">
              <div className="help-card-ico" aria-hidden="true">
                <svg width="20" height="20" viewBox="0 0 24 24" fill="currentColor">
                  <path d="M12 0C5.37 0 0 5.37 0 12c0 5.31 3.435 9.795 8.205 11.385.6.105.825-.255.825-.57 0-.285-.015-1.23-.015-2.235-3.015.555-3.795-.735-4.035-1.41-.135-.345-.72-1.41-1.23-1.695-.42-.225-1.02-.78-.015-.795.945-.015 1.62.87 1.845 1.23 1.08 1.815 2.805 1.305 3.495.99.105-.78.42-1.305.765-1.605-2.67-.3-5.46-1.335-5.46-5.925 0-1.305.465-2.385 1.23-3.225-.12-.3-.54-1.53.12-3.18 0 0 1.005-.315 3.3 1.23.96-.27 1.98-.405 3-.405s2.04.135 3 .405c2.295-1.56 3.3-1.23 3.3-1.23.66 1.65.24 2.88.12 3.18.765.84 1.23 1.905 1.23 3.225 0 4.605-2.805 5.625-5.475 5.925.435.375.81 1.095.81 2.22 0 1.605-.015 2.895-.015 3.3 0 .315.225.69.825.57A12.02 12.02 0 0 0 24 12c0-6.63-5.37-12-12-12z" />
                </svg>
              </div>
              <h3>Community support</h3>
              <p>Ask questions and learn from other users. The fastest way to get unblocked for everyday issues.</p>
              <div className="help-card-actions">
                <a
                  className="help-lnk"
                  href="https://github.com/kemeter/ring/discussions"
                  target="_blank"
                  rel="noopener noreferrer"
                >
                  GitHub Discussions →
                </a>
              </div>
            </article>

            <article className="help-card">
              <div className="help-card-ico" aria-hidden="true">
                <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                  <path d="M4 19.5A2.5 2.5 0 0 1 6.5 17H20M6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15A2.5 2.5 0 0 1 6.5 2z" />
                </svg>
              </div>
              <h3>Documentation</h3>
              <p>Getting started guides, how-tos and the full API reference. Read the docs first.</p>
              <div className="help-card-actions">
                <Link className="help-lnk" to="/documentation">
                  Open the docs →
                </Link>
              </div>
            </article>

            <article className="help-card">
              <div className="help-card-ico" aria-hidden="true">
                <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                  <path d="m8 2 1.88 1.88M14.12 3.88 16 2M12 20c-3.3 0-6-2.7-6-6v-3a4 4 0 0 1 4-4h4a4 4 0 0 1 4 4v3c0 3.3-2.7 6-6 6zM12 20v-9" />
                </svg>
              </div>
              <h3>Report a bug / security</h3>
              <p>Open a GitHub issue, or report a security vulnerability privately before disclosure.</p>
              <div className="help-card-actions">
                <a
                  className="help-lnk"
                  href="https://github.com/kemeter/ring/issues/new"
                  target="_blank"
                  rel="noopener noreferrer"
                >
                  Open an issue →
                </a>
              </div>
            </article>
          </div>
        </div>
      </section>

      {/* Bottom: Alpacode commercial support */}
      <div className="commercial">
        <section className="csupport-hero">
          <div className="container csupport-hero-inner">
            <h2>Need help in production?</h2>
            <p>
              <a
                className="csupport-inline-link"
                href="https://alpacode.fr"
                target="_blank"
                rel="noopener noreferrer"
              >
                Alpacode
              </a>{' '}
              provides commercial support for Ring.
            </p>
            <a
              className="btn btn-on-accent"
              href="https://alpacode.fr"
              target="_blank"
              rel="noopener noreferrer"
            >
              Contact Alpacode
            </a>
          </div>
        </section>

        <section className="svcs">
          <div className="container">
            <div className="svcs-grid">
              <article className="svc">
                <div className="svc-ico" aria-hidden="true">
                  <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                    <path d="M16 3h5v5M4 20 21 3M21 16v5h-5M15 15l6 6M4 4l5 5" />
                  </svg>
                </div>
                <h3>Migration</h3>
                <p>Move to Ring from any orchestration setup or infrastructure, with full team training and a zero-downtime cutover plan.</p>
              </article>

              <article className="svc">
                <div className="svc-ico" aria-hidden="true">
                  <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                    <path d="M22 12h-4l-3 9L9 3l-3 9H2" />
                  </svg>
                </div>
                <h3>Managed hosting</h3>
                <p>We run it in production: monitoring, security hardening, scaling and cloud platform management.</p>
              </article>

              <article className="svc">
                <div className="svc-ico" aria-hidden="true">
                  <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                    <circle cx="11" cy="11" r="8" />
                    <path d="m21 21-4.3-4.3" />
                  </svg>
                </div>
                <h3>Audit &amp; consulting</h3>
                <p>Architecture review, performance optimization, DevOps consulting and emergency rescue missions.</p>
              </article>

              <article className="svc">
                <div className="svc-ico" aria-hidden="true">
                  <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                    <path d="m18 16 4-4-4-4M6 8l-4 4 4 4M14.5 4l-5 16" />
                  </svg>
                </div>
                <h3>Custom development</h3>
                <p>Feature development, integrations and sponsored open-source work on Ring itself.</p>
              </article>

              <article className="svc">
                <div className="svc-ico" aria-hidden="true">
                  <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                    <path d="M12 2 4 5v6c0 5.55 3.84 10.74 8 12 4.16-1.26 8-6.45 8-12V5l-8-3z" />
                    <path d="m9 12 2 2 4-4" />
                  </svg>
                </div>
                <h3>SLA support</h3>
                <p>Guaranteed response times, an escalation path and a direct line to the maintainers.</p>
              </article>

              <article className="svc">
                <div className="svc-ico" aria-hidden="true">
                  <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                    <path d="M2 3h6a4 4 0 0 1 4 4v14a3 3 0 0 0-3-3H2zM22 3h-6a4 4 0 0 0-4 4v14a3 3 0 0 1 3-3h7z" />
                  </svg>
                </div>
                <h3>Training</h3>
                <p>Hands-on workshops for your ops and dev teams, tailored to your stack.</p>
              </article>
            </div>
          </div>
        </section>

        <section className="cta-band">
          <div className="container">
            <h2>Let&apos;s talk about your infrastructure</h2>
            <p>Tell us what you run. We&apos;ll tell you how we can help.</p>
            <a
              className="btn btn-primary"
              href="https://alpacode.fr"
              target="_blank"
              rel="noopener noreferrer"
            >
              Contact Alpacode
            </a>
          </div>
        </section>
      </div>
    </>
  );
}
