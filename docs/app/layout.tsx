import { RootProvider } from 'fumadocs-ui/provider/next';
import './global.css';
import { Inter } from 'next/font/google';
import type { Metadata } from 'next';

const inter = Inter({
  subsets: ['latin'],
});

export const metadata: Metadata = {
  title: {
    default: 'Cersei',
    template: '%s | Cersei',
  },
  description: 'The Rust SDK for building coding agents. Tool execution, LLM streaming, graph memory, sub-agent orchestration, MCP — as composable library functions.',
  metadataBase: new URL('https://cersei.dev'),
  openGraph: {
    type: 'website',
    siteName: 'Cersei',
  },
  other: {
    'github:repo': 'https://github.com/pacifio/cersei',
  },
};

export default function Layout({ children }: LayoutProps<'/'>) {
  return (
    <html lang="en" className={inter.className} suppressHydrationWarning>
      <body className="flex flex-col min-h-screen">
        <RootProvider>{children}</RootProvider>
      </body>
    </html>
  );
}
