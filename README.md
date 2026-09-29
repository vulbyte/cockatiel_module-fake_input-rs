# fake-input (Rust)

Cockatiel module — **viewers spend points to trigger a fake keyboard/mouse
input on the streamer's own machine**. A way for chat to gently troll the
streamer: `!input left-click`, `!input hold-w`, `!input jump`.

## How it works

- Registers the `!input` command with the engine (routed straight to this
  module).
- On `!input <name>`, checks the viewer's CURRENT score against the module's
  `price`, deducts it, and performs the whitelisted input locally via
  [enigo](https://crates.io/crates/enigo).
- **Safety:** only the input NAMES listed in `config.json`'s `inputs` map can
  ever be fired — the module never parses raw keys from chat, so a viewer
  cannot inject an arbitrary key. An unknown name gets a chat reply telling
  them it isn't enabled.
- On macOS the process needs **Accessibility permission** (System Settings →
  Privacy & Security → Accessibility) for synthetic events to take effect.
  Grant it once.

## Command

```
!input <name>
```

## Config (`config.json` → `module_specific`)

| key | default | meaning |
| --- | --- | --- |
| `price` | `10000` | score cost per input (deducted from the viewer's current score) |
| `inputs` | (see below) | whitelist of allowed inputs: name → spec |
| `command_flag` | `!` | the command prefix |
| `reconnect_base_secs` / `reconnect_max_secs` | `1` / `30` | engine reconnect backoff |

### Input specs

```json
"inputs": {
  "left-click":    { "kind": "mouse", "button": "left" },
  "hold-w":        { "kind": "key",   "char": "w", "hold_ms": 1500 },
  "jump":          { "kind": "key",   "char": " " },
  "nudge-mouse":   { "kind": "mousemove", "x": 50, "y": 0 }
}
```

- `key`: `char` is the key (a single character; `" "` = space, letters, etc.);
  `hold_ms` (optional) holds it for that long instead of a quick tap.
- `mouse`: `button` is `left` / `right` / `middle` — a click at the current
  cursor position.
- `mousemove`: relative cursor move by `x` / `y` pixels.

The defaults set the module to **authority user, price 10,000, no rank bar** —
any viewer can spend 10,000 score on a whitelisted input. The streamer edits
`inputs` (and `price`) in `config.json` to control exactly what chat can do.

## Build

```
cargo build --release
```

The manifest ships per-OS/arch `binary` routes; the supervisor runs the
prebuilt binary directly.