// A chat message's text as Markdown (GFM: code, lists, tables,
// strikethrough, autolinks). The token lives in this page, so this is a
// security surface, and it is kept narrow:
//
// - react-markdown builds React elements, never HTML strings; there is no
//   dangerouslySetInnerHTML. Raw HTML in a message stays text (react-markdown's
//   default turns `raw` nodes into text; rehype-raw is not used).
// - Only the elements below are rendered; anything else is unwrapped to its
//   text.
// - Link targets pass `safeUrl`: absolute http, https and mailto only. A
//   link with any other target (javascript:, data:, relative, …) is shown as
//   its text with no link. Links open in a new tab with
//   rel="noopener noreferrer".
// - Images are never loaded: an image is shown as a link to its address.
// - No syntax highlighting.
import Markdown, { type Components } from 'react-markdown'
import remarkGfm from 'remark-gfm'
import { safeUrl } from '@/lib/safe-url'

const ALLOWED = [
  'p', 'br', 'strong', 'em', 'del', 'code', 'pre', 'a', 'img', 'ul', 'ol', 'li',
  'blockquote', 'hr', 'table', 'thead', 'tbody', 'tr', 'th', 'td',
  'h1', 'h2', 'h3', 'h4', 'h5', 'h6',
]

function Link({ href, children }: { href?: string; children?: React.ReactNode }) {
  if (!href) return <span>{children}</span>
  return (
    <a href={href} target="_blank" rel="noopener noreferrer" className="text-primary underline underline-offset-2 break-all">
      {children}
    </a>
  )
}

const heading = ({ children }: { children?: React.ReactNode }) => <p className="font-semibold">{children}</p>

const components: Components = {
  a: ({ href, children }) => <Link href={href}>{children}</Link>,
  // Never loaded: the address, as a link (if it is a safe one).
  img: ({ src, alt }) => {
    const href = typeof src === 'string' ? src : undefined
    const label = `image${alt ? `: ${alt}` : ''}`
    return href ? <Link href={href}>[{label}]</Link> : <span>[{label}]</span>
  },
  p: ({ children }) => <p className="whitespace-pre-wrap">{children}</p>,
  pre: ({ children }) => (
    <pre className="my-1 max-w-full overflow-x-auto rounded-md bg-muted px-2 py-1.5 font-mono text-xs [&>code]:bg-transparent [&>code]:p-0">
      {children}
    </pre>
  ),
  code: ({ children }) => <code className="rounded bg-muted px-1 py-px font-mono text-[0.85em]">{children}</code>,
  ul: ({ children }) => <ul className="my-0.5 list-disc pl-5">{children}</ul>,
  ol: ({ children }) => <ol className="my-0.5 list-decimal pl-5">{children}</ol>,
  li: ({ children }) => <li>{children}</li>,
  blockquote: ({ children }) => <blockquote className="my-0.5 border-l-2 pl-2 text-muted-foreground">{children}</blockquote>,
  hr: () => <hr className="my-1" />,
  table: ({ children }) => (
    <div className="my-1 max-w-full overflow-x-auto">
      <table className="border-collapse text-xs">{children}</table>
    </div>
  ),
  th: ({ children }) => <th className="border px-2 py-0.5 text-left font-semibold">{children}</th>,
  td: ({ children }) => <td className="border px-2 py-0.5">{children}</td>,
  h1: heading,
  h2: heading,
  h3: heading,
  h4: heading,
  h5: heading,
  h6: heading,
}

/**
 * The text of a message. Inline: the first paragraph runs on from the
 * author's label, later blocks follow below.
 */
export function MessageText({ text }: { text: string }) {
  return (
    <div className="contents [&>p:first-child]:inline">
      <Markdown
        remarkPlugins={[remarkGfm]}
        allowedElements={ALLOWED}
        unwrapDisallowed
        urlTransform={(url) => safeUrl(url)}
        components={components}
      >
        {text}
      </Markdown>
    </div>
  )
}
