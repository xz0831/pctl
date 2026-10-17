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

The status line says `Copied N characters` only when the command worked or the terminal is one known to honour OSC 52 (Ghostty, iTerm2, WezTerm, kitty, Alacritty, foot; the list comes from those terminals' documentation, only Ghostty is in daily use with pctl). Otherwise:
- `Sent N characters (the terminal may not copy; Shift+drag does)`
- `This terminal ignores copying: Shift+drag, then ctrl+shift+c` when `VTE_VERSION` is set (a VTE terminal) and no command worked.

Selections over 100,000 characters are refused.

## Notifications

When an agent that was working goes idle or waits for you, pctl writes an OSC 777 notification (and a bell when it waits). At most one per agent every 10 seconds, never for the agent on screen. On a local Linux desktop (not ssh, `DISPLAY` or `WAYLAND_DISPLAY` set) it also runs `notify-send -- pctl "<name>: done"` (or `needs you`), if `notify-send` is installed, so a terminal that ignores OSC 777 still shows a notice.

On macOS pctl does nothing beyond OSC 777. A macOS terminal that ignores OSC 777 would need a call to `osascript -e 'display notification …'` or `terminal-notifier`; this is not built.

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
