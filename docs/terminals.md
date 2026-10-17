# Terminals

The pctl app draws in your terminal and uses a few things the terminal may or may not do. This page says what each terminal does and what pctl does about it. It lists only what was tried.

## What is whose

| | The terminal | pctl |
|---|---|---|
| Drawing, colours, wide characters | yes | uses 24-bit colour |
| Copy to the clipboard | OSC 52, if it supports it | writes OSC 52, and also runs a clipboard command on a local desktop |
| Desktop notifications | OSC 777, if it supports it | writes OSC 777, and also runs `notify-send` on a local Linux desktop |
| Selection by `Shift`+drag, `ctrl+shift+c` | yes (the terminal's own) | never takes `Shift`+mouse |
| Korean and other input methods | draws the composing text | keeps the real cursor on the agent's cursor, so the composing text appears there |
| Right button, some keys | may keep them | double-click and the `ctrl+b` prefix cover them |

## Copying

A drag in the agent's terminal copies the text. pctl does two things:

1. It writes an OSC 52 sequence, which most terminals turn into a clipboard write.
2. Unless you are on ssh (`SSH_CONNECTION` is set; that clipboard is not yours), it runs a command that puts the text on this computer's clipboard:
   - macOS: `pbcopy`
   - Linux with `WAYLAND_DISPLAY`: `wl-copy`
   - Linux with `DISPLAY`: `xclip -selection clipboard -i`, else `xsel -ib`

   The command gets two seconds.

The status line says `Copied N characters` only when the command worked or the terminal is one whose documentation says an OSC 52 write copies by default (Ghostty, WezTerm, kitty, Alacritty; only Ghostty is in daily use with pctl). iTerm2 is not on that list: it asks you to allow OSC 52 first (on macOS `pbcopy` covers it). Otherwise:
- `Sent N characters (the terminal may not copy; Shift+drag does)`
- `This terminal ignores copying: Shift+drag, then ctrl+shift+c` when `VTE_VERSION` is set (a VTE terminal) and no command worked.

Selections over 100,000 characters are refused.

## Notifications

When an agent that was working goes idle or waits for you, pctl writes an OSC 777 notification (and a bell when it waits). At most one per agent every 10 seconds, never for the agent on screen. On a local Linux desktop (not ssh, `DISPLAY` or `WAYLAND_DISPLAY` set) it also runs `notify-send -- pctl "<name>: done"` (or `needs you`), if `notify-send` is installed, so a terminal that ignores OSC 777 still shows a notice.

The Linux call applies to any terminal, so it also helps Alacritty, which ignores OSC 9 and OSC 777 (from its documentation, not tried). On macOS pctl does nothing beyond OSC 777. A macOS terminal that ignores OSC 777 (iTerm2 v3.6.11 does, from its source) would need a call to `osascript -e 'display notification …'` or `terminal-notifier`; this is not built.

## Ghostty

- Honours OSC 52 and shows OSC 777 notifications, so nothing extra is needed. This is the terminal pctl is used in every day; it was not re-measured for this page.

## MATE Terminal (VTE 0.64.2)

Tried on Rocky Linux 9 with MATE Terminal 1.26.1, default settings, under an X display driven by `xdotool`. Other VTE versions were not tried.

| | What happens | What pctl does |
|---|---|---|
| Right-click | MATE's own menu opens; pctl never sees it | double-click an agent, or `ctrl+b m`, for the agent menu |
| OSC 52 (copy) | ignored; the clipboard does not change | runs `xclip`, `xsel` or `wl-copy`; else tells you to use `Shift`+drag |
| OSC 777 and OSC 9 (notifications) | ignored | runs `notify-send` |
| Bell | no urgency hint on the window; the sound was not heard in the test | still sent when an agent waits |
| `Shift`+drag | the terminal's own selection; works over the agent pane and the sidebar; `ctrl+shift+c` then copies it | not taken |
| Wheel, click, double-click, sidebar drag | work | — |
| Paste (`ctrl+shift+v`) | works; a new line arrives as Enter, so a pasted line is submitted | — |
| Korean input (ibus-hangul) | works; the composing syllable is drawn at the terminal cursor, which pctl puts on the agent's cursor | — |

Keys MATE Terminal takes before pctl or the agent sees them:

- `ctrl+shift+c`, `ctrl+shift+v`: copy, paste.
- `F1` (help), `F10` (menu bar), `F11` (full screen).
- `alt+f`, `alt+e`, `alt+v`, `alt+s`, `alt+t`: menu bar mnemonics. `alt+f` is "forward one word" in many line editors, so it does not reach the agent. The setting `org.mate.terminal.global use-mnemonics` is `true` by default and is the likely fix; this was **not tested**.
- With two or more tabs: `alt+1`…`alt+9` and `ctrl+page up` / `ctrl+page down` switch tabs. With one tab they pass through.
- `ctrl+shift+t`, `ctrl+shift+n`, `ctrl+shift+f`, `ctrl+shift+page up/down`, and the zoom keys (`ctrl+minus`, `ctrl+shift+plus`, `ctrl+0`).

`ctrl+b`, the pctl prefix, reaches pctl; `ctrl+b ctrl+b` sends a literal `ctrl+b` to the agent.

## Other terminals: from docs, not measured

Everything in this table comes from the terminals' documentation and source at the versions named, collected for this project; none of it was tried with pctl. "Not tried" is not "does not work": an empty or conditional cell means check before relying on it. The multiplexer between the terminal and the app (tmux) must also let each feature through.

| Terminal (version read) | OSC 52 write | OSC 777 / OSC 9 notice | `Shift`+mouse selects locally | IME composing text | Things to know |
|---|---|---|---|---|---|
| Ghostty v1.3.1 | yes, on by default | both | yes (`mouse-shift-capture`) | drawn at the cursor | OSC 52 read asks first |
| iTerm2 v3.6.11 | only after you allow it (off by default) | 9 yes, **777 no** | **`Option`**, not `Shift` | drawn at the cursor | notifications need the OS allowed too |
| Apple Terminal (macOS Tahoe 26) | not documented | not documented | not documented; `View > Allow Mouse Reporting` off instead | not documented | truecolor announced for 26 |
| WezTerm 20240203 | yes | both | yes (`Shift`) | yes | OSC 52 read not supported |
| kitty v0.49.2 | yes, on by default | both | yes (`Shift`+left) | yes | OSC 52 read asks first |
| Alacritty v0.17.0 | yes, only while the window has focus | **neither** | yes (`Shift`) | yes | |
| GNOME Terminal (VTE 0.82.2 and 0.85.0) | **no** | **no** (a limited, off-by-default OSC 777 form exists, for "Command completed" only; OSC 9 only for progress) | yes | drawn at the cursor | same engine as MATE Terminal |
| MATE Terminal (VTE 0.82.2 and 0.85.0) | **no** | **no** (as above) | yes | drawn at the cursor | measured on VTE 0.64.2 above: same results |
| Konsole (26.11.70 development) | yes | 777 yes, 9 only progress | yes | yes | |
| Windows Terminal (development source) | yes | 777 only if switched on (`compatibility.allowOSC777`), 9 no | yes (`Shift`) | yes | only as the window an ssh session shows in |
| xterm #411 | possible, but blocked by default (`allowWindowOps`) | neither | yes (`shiftEscape`) | via XIM, build-dependent | |

The mouse reporting pctl needs (SGR 1006), bracketed paste and the wheel-to-arrow-keys fallback are listed as supported (some only under a setting or mode) for all of these except where Apple Terminal is "not documented". IME rows say a composing-text path exists in the source, not that a given Korean input method places its candidate window correctly.
