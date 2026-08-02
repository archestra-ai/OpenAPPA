import { Header } from "@/components/Header";

export default function SiteLayout({ children }: { children: React.ReactNode }) {
  return (
    <>
      <Header />
      {children}
      <footer className="site-footer">
        <span>© {new Date().getFullYear()} OpenAPPA</span>
        <a href="https://github.com/archestra-ai/OpenAPPA" target="_blank" rel="noreferrer">
          GitHub
        </a>
      </footer>
    </>
  );
}
