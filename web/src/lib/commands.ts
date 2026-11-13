// Slash-command suggestions for the composer, as the TUI lists them
// (crates/triad/src/tui/commands.rs). The session still checks every
// command; this list is only for the eye and the Tab key. `/quit` is not
// offered; `/switch` and `/s` are handled by the app, since the API refuses
// them and focus is the tab's own.

export interface Command {
  name: string
  args: string
  help: string
}

const c = (name: string, args: string, help: string): Command => ({ name, args, help })

export const COMMANDS: Command[] = [
  c('/switch', '<name>', 'focus another channel'),
  c('/s', '<name>', 'focus another channel'),
  c('/msg', '<name> <text>', 'send to a channel without switching'),
  c('/me', '<action>', 'say what you are doing: * you waves'),
  c('/channels', '', 'list channels'),
  c('/list', '', 'list channels'),
  c('/create', '<name> [--no-bot]', 'create a channel'),
  c('/join', '<invite> [name]', 'join a channel'),
  c('/topic', '<name>', 'open a topic under the focused channel'),
  c('/invite', '[name]', 'print an invite to a channel'),
  c('/members', '', "the focused channel's members"),
  c('/status', '', "the focused channel's counts"),
  c('/add', '<node>', 'add a member'),
  c('/remove', '<node>', 'remove a member'),
  c('/promote', '<node>', 'give a member Manage access'),
  c('/nick', '<name>', 'change your nickname'),
  c('/bot', '', "show the focused channel's bot"),
  c('/bot on', '', 'add your bot to the focused channel'),
  c('/bot off', '', 'remove your bot from the focused channel'),
  c('/bot model', '[vendor/model]', "show or set your bots' model"),
  c('/bot provider-key', '[vendor] <key>', "save the model provider's key"),
  c('/grant', '<node> <server.tool|server.*>', 'let a member use bot tools'),
  c('/revoke', '<node> [pattern]', 'take bot tools back'),
  c('/grants', '', "the focused channel's grants"),
  c('/block', '<node>', 'your bots never answer this node'),
  c('/unblock', '<node>', 'answer this node again'),
  c('/audit', '[N]', "the focused channel's last tool calls"),
  c('/prompt', '', "your bot's system prompt here"),
  c('/memory', '[query]', 'what your bot remembers'),
  c('/forget', '<id>', "delete one of your bot's memories"),
  c('/schedules', '', "your bot's schedules"),
  c('/unschedule', '<id>', "remove one of your bot's schedules"),
]

/** Commands whose first argument is a channel name. */
const TAKES_CHANNEL = ['/switch', '/s', '/msg', '/invite']

export interface Suggestion {
  /** What the input becomes when this is picked. */
  complete: string
  label: string
  help: string
}

/** Suggestions for `text`, given the channel names. Empty unless `text` is a slash command still being typed. */
export function suggest(text: string, channels: string[]): Suggestion[] {
  if (!text.startsWith('/') || text.includes('\n')) return []
  const sp = text.indexOf(' ')
  if (sp >= 0) {
    const cmd = text.slice(0, sp)
    const arg = text.slice(sp + 1)
    if (TAKES_CHANNEL.includes(cmd) && !arg.includes(' ')) {
      return channels
        .filter((n) => n.startsWith(arg) && n !== arg)
        .map((n) => ({ complete: `${cmd} ${n} `, label: `${cmd} ${n}`, help: '' }))
    }
  }
  // The longest command already typed in full, with arguments after it: show its usage only.
  const typed = COMMANDS.filter((x) => x.args !== '' && text.startsWith(`${x.name} `)).reduce<Command | null>(
    (best, x) => (best === null || x.name.length > best.name.length ? x : best),
    null,
  )
  const prefix = COMMANDS.filter((x) => x.name.startsWith(text) && x.name !== text)
  let out: Command[]
  if (typed && typed.name.length + 1 === text.length) {
    // Just past the name: longer forms ("/bot model" after "/bot ") if there are any, else the usage.
    out = prefix.length > 0 ? prefix : [typed]
  } else if (typed) {
    out = [typed]
  } else {
    const exact = COMMANDS.filter((x) => x.name === text && x.args !== '')
    out = [...exact, ...prefix]
  }
  return out.map((x) => ({
    complete: x.args === '' ? x.name : `${x.name} `,
    label: x.args === '' ? x.name : `${x.name} ${x.args}`,
    help: x.help,
  }))
}

const KEY_COMMAND = '/bot provider-key '

/** `text` with a typed provider key hidden, for the screen. */
export function redact(text: string): string {
  if (!text.startsWith(KEY_COMMAND)) return text
  const arg = text.slice(KEY_COMMAND.length)
  const words = arg.split(' ')
  const hide = (k: string) => '•'.repeat([...k].length)
  if (words.length === 2 && words[0] !== '') return `${KEY_COMMAND}${words[0]} ${hide(words[1])}`
  return `${KEY_COMMAND}${hide(arg)}`
}

/** `/switch name` or `/s name`: the channel to focus, handled in the tab. */
export function switchTarget(line: string): string | null {
  const m = /^\/(?:switch|s)\s+#?(\S+)\s*$/.exec(line.trim())
  return m ? m[1] : null
}
