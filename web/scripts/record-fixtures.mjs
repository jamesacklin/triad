// Records the frames a real `triad serve` sends, for the store's tests.
//
//   node scripts/record-fixtures.mjs ../target/debug/triad
//
// Two throwaway identities: alice runs `serve` (mock bot) and this script
// drives her over the socket; bob runs `join --json` and is driven on
// stdin. The frames alice's client receives are written, one per line, to
// src/test/fixtures/session.jsonl. Invites are redacted (they carry
// addresses); the token is never written.
import { spawn } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { createHash, randomBytes } from 'node:crypto'
import { connect as tcp } from 'node:net'
import { createInterface } from 'node:readline'

const triad = process.argv[2]
if (!triad) throw new Error('usage: record-fixtures.mjs <path to triad>')
const out = new URL('../src/test/fixtures/session.jsonl', import.meta.url)
const root = mkdtempSync(join(tmpdir(), 'triad-fixtures-'))
const env = { ...process.env, TRIAD_AGENT_BRAIN: 'mock' }
const procs = []
// Node's WebSocket sends an Origin, as a browser does; serve must allow it.
const ORIGIN = 'http://fixtures.localhost'

function start(args, onLine) {
  const p = spawn(triad, args, { env, stdio: ['pipe', 'pipe', 'pipe'] })
  procs.push(p)
  createInterface({ input: p.stdout }).on('line', onLine)
  createInterface({ input: p.stderr }).on('line', (l) => onLine(l, true))
  return p
}

/**
 * A minimal WebSocket client (text frames only). Node's built-in WebSocket
 * (undici) refuses serve's handshake: the 101 says `Connection: upgrade,
 * close` and undici wants exactly `upgrade`. Browsers accept it. See
 * FINDINGS.md.
 */
function connect(url, origin, onText) {
  const u = new URL(url)
  const key = randomBytes(16).toString('base64')
  const sock = tcp(Number(u.port), u.hostname)
  sock.write(
    `GET ${u.pathname} HTTP/1.1\r\nHost: ${u.host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n` +
      `Sec-WebSocket-Key: ${key}\r\nSec-WebSocket-Version: 13\r\nOrigin: ${origin}\r\n\r\n`,
  )
  const accept = createHash('sha1').update(key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64')
  const frame = (op, payload) => {
    const mask = randomBytes(4)
    const n = payload.length
    const head = n < 126 ? Buffer.from([0x80 | op, 0x80 | n]) : n < 65536
      ? Buffer.from([0x80 | op, 0x80 | 126, n >> 8, n & 255])
      : Buffer.concat([Buffer.from([0x80 | op, 0x80 | 127]), (() => { const b = Buffer.alloc(8); b.writeBigUInt64BE(BigInt(n)); return b })()])
    const body = Buffer.from(payload.map((b, i) => b ^ mask[i % 4]))
    sock.write(Buffer.concat([head, mask, body]))
  }
  return new Promise((resolve, reject) => {
    let buf = Buffer.alloc(0)
    let open = false
    let text = []
    sock.on('error', reject)
    sock.on('close', () => console.error('socket closed'))
    sock.on('data', (d) => {
      buf = Buffer.concat([buf, d])
      if (!open) {
        const end = buf.indexOf('\r\n\r\n')
        if (end < 0) return
        const head = buf.subarray(0, end).toString()
        if (!head.startsWith('HTTP/1.1 101') || !head.includes(accept)) return reject(new Error(head.split('\r\n')[0]))
        buf = buf.subarray(end + 4)
        open = true
        resolve({ send: (s) => frame(1, [...Buffer.from(s)]), close: () => (frame(8, [3, 232]), sock.end()) })
      }
      for (;;) {
        if (buf.length < 2) return
        const op = buf[0] & 15
        const fin = buf[0] & 128
        let n = buf[1] & 127
        let at = 2
        if (n === 126) { if (buf.length < 4) return; n = buf.readUInt16BE(2); at = 4 }
        else if (n === 127) { if (buf.length < 10) return; n = Number(buf.readBigUInt64BE(2)); at = 10 }
        if (buf.length < at + n) return
        const payload = buf.subarray(at, at + n)
        buf = buf.subarray(at + n)
        if (op === 9) frame(10, [...payload])
        else if (op === 1 || op === 0) {
          text.push(payload)
          if (fin) { onText(Buffer.concat(text).toString()); text = [] }
        } else if (op === 8) console.error('close frame', payload.length >= 2 ? payload.readUInt16BE(0) : '')
      }
    })
  })
}

const frames = []
const waiters = []
function waitFor(pred, what, ms = 20_000) {
  console.error(`waiting for ${what}`)
  const hit = frames.find(pred)
  if (hit) return Promise.resolve(hit)
  return new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error(`timed out waiting for ${what}`)), ms)
    waiters.push({ pred, resolve: (f) => (clearTimeout(t), resolve(f)) })
  })
}

try {
  const alice = join(root, 'alice')
  const bob = join(root, 'bob')
  let listening
  const serving = new Promise((resolve) => {
    start(['--data-dir', alice, '--nickname', 'alice', 'serve', '--listen', '127.0.0.1:0', '--allow-origin', ORIGIN], (l) => {
      if (process.env.DEBUG) console.error('alice:', l)
      const m = /listening\s+(ws:\/\/\S+)/.exec(l)
      if (m) resolve(m[1])
    })
  })
  listening = await serving
  const token = readFileSync(join(alice, 'api-token'), 'utf8').trim()

  const ws = await connect(listening, ORIGIN, (text) => {
    const f = JSON.parse(text)
    frames.push(f)
    for (const w of [...waiters]) {
      if (w.pred(f)) {
        waiters.splice(waiters.indexOf(w), 1)
        w.resolve(f)
      }
    }
  })
  let id = 0
  const req = (body) => {
    const r = ++id
    ws.send(JSON.stringify({ id: r, ...body }))
    return waitFor((f) => f.event === 'done' && f.re === r, `done for ${body.op} ${body.line ?? ''}`)
  }
  ws.send(JSON.stringify({ op: 'auth', token }))
  const ready = await waitFor((f) => f.event === 'ready', 'ready')
  await req({ op: 'history', channel: ready.channel })

  // A channel, and bob in it.
  await req({ op: 'command', line: '/create garden' })
  await req({ op: 'command', channel: 'garden', line: '/invite garden' })
  const invite = frames.find((f) => f.event === 'invite').invite
  const bobLines = []
  const bobP = start(['--data-dir', bob, '--nickname', 'bob', 'join', '--json', invite], (l) => {
    bobLines.push(l)
    if (process.env.DEBUG) console.error('bob:', l.slice(0, 200))
  })
  const ask = await waitFor((f) => f.event === 'join_request', 'join_request', 60_000)
  await req({ op: 'answer', channel: ask.channel, member: ask.member, accept: true })
  await waitFor((f) => f.event === 'joined' && f.member === ask.member, 'bob joined', 60_000)
  // Bob's session needs a moment to open the space before he can write.
  await new Promise((r) => setTimeout(r, 3000))
  bobP.stdin.write('hello from bob\n')
  await waitFor((f) => f.event === 'message' && !f.mine, "bob's message", 60_000)

  // Alice writes, acts, and asks her bot.
  await req({ op: 'send', channel: 'garden', text: 'hi bob\nsecond line' })
  await waitFor((f) => f.event === 'message' && f.mine && f.text.startsWith('hi bob'), 'own message')
  await req({ op: 'send', channel: 'garden', text: 'waves', action: true })
  await req({ op: 'command', channel: 'garden', line: '/bot on' })
  await req({ op: 'send', channel: 'garden', text: '@bot what time is it?' })
  await waitFor((f) => f.event === 'job' && f.state === 'done', 'job done', 60_000)
  await waitFor((f) => f.event === 'message' && f.bot, 'bot reply', 60_000)

  for (const line of ['/members', '/channels', '/status', '/bot', '/bot model', '/grants', '/audit', '/memory', '/prompt', '/schedules', '/nosuch', '/nick alice2']) {
    await req({ op: 'command', channel: 'garden', line })
  }
  await req({ op: 'command', channel: 'garden', line: '/msg nowhere hi' })
  await req({ op: 'history', channel: 'garden', limit: 2 })
  await req({ op: 'snapshot' })
  ws.close()

  const redacted = frames.map((f) => JSON.stringify(f).replaceAll(invite, 'triad1-REDACTED'))
  writeFileSync(out, redacted.join('\n') + '\n')
  console.error(`wrote ${frames.length} frames to ${out.pathname}`)
} finally {
  for (const p of procs) p.kill()
  rmSync(root, { recursive: true, force: true })
}
