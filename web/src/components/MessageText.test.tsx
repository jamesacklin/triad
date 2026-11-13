import { renderToStaticMarkup } from 'react-dom/server'
import { describe, expect, it } from 'vitest'
import { safeUrl } from '@/lib/safe-url'
import { MessageText } from './MessageText'

const html = (text: string) => renderToStaticMarkup(<MessageText text={text} />)
/** The element tags in the output, without our own wrapper. */
const tags = (out: string) => (out.match(/<[a-zA-Z][^>]*>/g) ?? []).slice(1)
const names = (out: string) => tags(out).map((t) => /^<([a-zA-Z0-9]+)/.exec(t)![1].toLowerCase())

describe('MessageText: hostile input', () => {
  it('raw HTML stays text: no script, no element, no event handler', () => {
    const out = html('hi <script>alert(1)</script> <img src=x onerror="alert(2)"> <b onclick="x()">b</b>')
    expect(names(out)).toEqual(['p'])
    expect(tags(out).some((t) => / on[a-z]+=/i.test(t))).toBe(false)
    // Shown, escaped, as the text it is.
    expect(out).toContain('&lt;script&gt;alert(1)&lt;/script&gt;')
    expect(out).toContain('&lt;img src=x onerror=&quot;alert(2)&quot;&gt;')
  })

  it('a block of HTML stays text too', () => {
    const out = html('<div onmouseover="steal()">\n<iframe src="https://evil.test"></iframe>\n</div>')
    expect(names(out)).not.toContain('div')
    expect(names(out)).not.toContain('iframe')
    expect(tags(out).some((t) => / on[a-z]+=/i.test(t))).toBe(false)
    expect(out).toContain('&lt;iframe')
  })

  it('javascript:, data: and other targets give no link; the text remains', () => {
    for (const target of [
      'javascript:alert(1)',
      'JaVaScRiPt:alert(1)',
      ' javascript:alert(1)',
      'data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==',
      'vbscript:msgbox(1)',
      'file:///etc/passwd',
      '/relative',
      '#frag',
      'irc://x',
    ]) {
      const out = html(`[click](${target})`)
      // The target is named in a failure.
      expect({ target, linked: out.includes('href'), text: out.includes('click') }).toEqual({ target, linked: false, text: true })
    }
    const auto = html('<javascript:alert(1)>')
    expect(auto).not.toContain('href')
  })

  it('http, https and mailto links open in a new tab without opener or referrer', () => {
    for (const target of ['https://example.com/a?b=1', 'http://example.com', 'mailto:sam@example.com']) {
      const out = html(`[x](${target})`)
      expect(out).toContain(`href="${target.replace('&', '&amp;')}"`)
      expect(out).toContain('target="_blank"')
      expect(out).toContain('rel="noopener noreferrer"')
    }
  })

  it('images are not loaded: shown as a link to their address, or as text', () => {
    const out = html('![a cat](https://example.com/cat.png)')
    expect(out).not.toMatch(/<img/i)
    expect(out).toContain('href="https://example.com/cat.png"')
    expect(out).toContain('[image: a cat]')
    const bad = html('![x](javascript:alert(1))')
    expect(bad).not.toMatch(/<img|href/i)
    expect(bad).toContain('[image: x]')
  })

  it('a link title or reference cannot smuggle attributes', () => {
    const out = html('[x](https://example.com "a\\" onclick=\\"y")\n\n[r][1]\n\n[1]: javascript:alert(1)')
    expect(tags(out).some((t) => / on[a-z]+=/i.test(t))).toBe(false)
    expect(out.match(/href=/g)).toHaveLength(1)
  })
})

describe('MessageText: GFM basics', () => {
  it('code blocks (no highlighting), inline code, lists, tables, strikethrough, autolinks', () => {
    const out = html(
      [
        'see `x = 1` and ~~old~~',
        '',
        '```js',
        'let a = "<b>"',
        '```',
        '',
        '- one',
        '- two',
        '',
        '1. first',
        '',
        '| a | b |',
        '|---|---|',
        '| 1 | 2 |',
        '',
        'visit https://example.com now',
      ].join('\n'),
    )
    expect(out).toMatch(/<code[^>]*>x = 1<\/code>/)
    expect(out).toMatch(/<del>old<\/del>/)
    expect(out).toMatch(/<pre[^>]*><code[^>]*>let a = &quot;&lt;b&gt;&quot;\n<\/code><\/pre>/)
    expect(out).not.toMatch(/language-|hljs|token/)
    expect(out).toMatch(/<ul[^>]*>\n?<li>one<\/li>\n?<li>two<\/li>\n?<\/ul>/)
    expect(out).toContain('<ol')
    expect(out).toContain('<table')
    expect(out).toMatch(/<td[^>]*>2<\/td>/)
    expect(out).toContain('href="https://example.com"')
  })

  it('keeps single line breaks, as plain text did', () => {
    const out = html('line one\nline two')
    expect(out).toContain('line one\nline two')
    expect(out).toContain('whitespace-pre-wrap')
  })

  it('task-list checkboxes and footnote markup are dropped to text', () => {
    const out = html('- [x] done\n\nnote[^1]\n\n[^1]: the note')
    expect(out).not.toMatch(/<input|<section|<sup/i)
    expect(out).toContain('done')
  })
})

describe('safeUrl', () => {
  it('allows only absolute http, https and mailto', () => {
    expect(safeUrl('https://a.test')).toBe('https://a.test')
    expect(safeUrl('MAILTO:x@a.test')).toBe('MAILTO:x@a.test')
    expect(safeUrl('javascript:alert(1)')).toBeUndefined()
    expect(safeUrl('java\tscript:alert(1)')).toBeUndefined()
    expect(safeUrl('//evil.test')).toBeUndefined()
    expect(safeUrl('data:,x')).toBeUndefined()
  })
})
