import { Link } from 'aplos/navigation';
import ThemeToggle from './ThemeToggle';
import '@/styles/components/footer.css';

export default function Footer() {
  return (
    <footer className="site-footer">
      <div className="footer-inner">
        <span>&copy; {new Date().getFullYear()} Ring by <a href="https://kemeter.io" target="_blank" rel="noopener noreferrer">kemeter</a>. MIT License.</span>
        <div className="footer-links">
          <Link to="/documentation">Documentation</Link>
          <a
            href="https://github.com/kemeter/ring"
            target="_blank"
            rel="noopener noreferrer"
          >
            GitHub
          </a>
          <a
            href="https://github.com/kemeter/ring/blob/main/LICENSE"
            target="_blank"
            rel="noopener noreferrer"
          >
            License
          </a>
          <ThemeToggle />
        </div>
      </div>
    </footer>
  );
}
