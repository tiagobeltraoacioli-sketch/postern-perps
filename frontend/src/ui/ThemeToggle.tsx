import { Moon, Sun } from 'lucide-react';
import { useTheme } from './theme';

export function ThemeToggle() {
  const [theme, toggle] = useTheme();
  const next = theme === 'dark' ? 'light' : 'dark';
  return (
    <button type="button" className="icon-button" aria-label={`Use the ${next} theme`} title={`Use the ${next} theme`} onClick={toggle}>
      {theme === 'dark' ? <Sun size={18} aria-hidden="true" /> : <Moon size={18} aria-hidden="true" />}
    </button>
  );
}
