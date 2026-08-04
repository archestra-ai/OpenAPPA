import Link from "next/link";

import { AppaMark } from "@/components/AppaMark";
import { Logo } from "@/components/Logo";

export function Header() {
  return (
    <header className="site-header">
      <Link href="/" className="wordmark">
        <AppaMark size={52} />
        <Logo height={15} />
      </Link>
      <nav>
        <Link href="/">Docs</Link>
        <a href="https://github.com/archestra-ai/OpenAPPA" target="_blank" rel="noreferrer">
          GitHub
        </a>
      </nav>
    </header>
  );
}
