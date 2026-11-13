// Which link targets a message may have.

/** `url` if it is an absolute http, https or mailto address; otherwise nothing. */
export function safeUrl(url: string): string | undefined {
  const u = url.trim()
  let parsed: URL
  try {
    parsed = new URL(u)
  } catch {
    return undefined
  }
  return ['http:', 'https:', 'mailto:'].includes(parsed.protocol) ? u : undefined
}
