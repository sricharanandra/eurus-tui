# UI

The Eurus TUI renders a full-screen terminal interface with a header, main content area (split into a voice sidebar and chat area), and a footer. All rendering is done through `ratatui` with `crossterm` as the backend.

## Layout

The screen is divided into three vertical chunks:

```
+----------------------------------------------------------+
| HEADER (1 line)                                          |
| "eurus" [#general] 5 online                              |
+----------------------------------------------------------+
|                                                          |
|  MAIN CONTENT (flexible height)                          |
|  +------------+-----------------------------------------+|
|  | VOICE      |  CHAT AREA                              ||
|  | SIDEBAR    |  (dark gray background, 2-char padding) ||
|  | (25 cols)  |                                         ||
|  |            |  January 24, 2026                       ||
|  | ● Connected|  ─────────────────────────              ||
|  | 🎤 Live    |                                         ||
|  | TX: Quiet  |  > alice                                ||
|  |            |    hey everyone                         ||
|  | Users:      |                        2:34 PM         ||
|  | • alice     |                                         ||
|  | • bob       |  > bob                                ||
|  |            |    hi!                                  ||
|  |            |                        2:35 PM         ||
|  |            |                                         ||
|  |            |  ┌──────────────────────────────────┐  ||
|  |            |  │ FLOATING INPUT BOX               │  ||
|  |            |  │ (centered, 95% width, 4 lines)   │  ||
|  |            |  │ [Insert] type your message...    │  ||
|  |            |  └──────────────────────────────────┘  ||
|  +------------+-----------------------------------------+|
|                                                          |
+----------------------------------------------------------+
| FOOTER (1 line)                                          |
| Connected to voice.                                      |
+----------------------------------------------------------+
```

## Header

Single line at the top. When in a room:

- **"eurus"** — Blue background, white text badge
- **Room name** — Dark gray, e.g., `[#general]`
- **Online count** — Gray, e.g., `5 online`

On other screens: `"eurus - private messaging"`

## Chat Area

The main content area has a dark gray background (`Rgb(25, 25, 25)`) with 2-character horizontal padding.

### Messages

Messages are rendered in groups by sender:

```
> alice                          2:34 PM
   hey everyone, how's it going?
   second line of the message

> bob                            2:35 PM
   pretty good! working on the
   new feature today
```

- **Sender** — Cyan, bold, prefixed with `>`
- **Timestamp** — Dark gray, right-aligned on the same line as the sender
- **Content** — White, indented 3 spaces, word-wrapped to available width
- **Grouping** — Consecutive messages from the same user show the header only on the first message

### Date Separators

When messages span multiple dates, a centered separator is inserted:

```
          ─── January 24, 2026 ───
```

### System Messages

System messages (join/leave notifications, errors) are rendered in magenta with a `!` prefix:

```
! alice joined the room
```

### Typing Indicator

When users are typing, a yellow italic indicator appears at the bottom of the chat area:

```
alice is typing...
alice and bob are typing...
```

Typing indicators expire after a few seconds if no new typing event is received.

## Floating Input Box

The message input is rendered as a floating overlay, not docked to the bottom:

- **Position** — Centered horizontally, near the bottom of the chat area
- **Width** — 95% of the main content area width
- **Height** — 4 lines
- **Background** — Medium gray (`Rgb(45, 45, 45)`)
- **Border** — Gray border with a title showing the current Vim mode
- **Title** — `[Normal]` (cyan) or `[Insert]` (green)

The floating design provides visual separation between the input and the message history, making it clear which area is active.

## Voice Sidebar

A 25-column panel on the left side of the chat area (InRoom screen only):

### Connection Status

```
● Connected    (green dot, green text)
● Connecting   (yellow dot, yellow text)
● Disconnected (gray dot, gray text)
```

### Microphone Status

```
🎤 Live        (green, when unmuted and transmitting)
🎤 Muted       (gray, when muted)
```

### TX Activity

```
TX: Quiet      (gray, no audio detected)
TX: Live       (green, audio being captured, 900ms timeout)
```

The TX indicator shows real-time microphone activity. It turns green when audio is detected and reverts to gray after 900ms of silence.

### Users in Voice

```
Users in voice:
  🔊 alice
  🔊 bob
```

Speaker icons indicate users currently receiving audio.

## Overlays

### User List (`:users`)

Centered box, 40 characters wide. Shows:

- Online users in the current room
- Typing indicators next to usernames
- Total count

### Room Switcher (`:list`)

Centered box, 50% width, 60% height. Shows:

- All rooms the user is a member of
- Current room marked with `*`
- Navigable with j/k or arrow keys
- Enter to switch, Esc to close

### Emoji Picker

Floating box positioned above the input box, 40 characters wide. Shows:

- Matching emoji shortcodes (prefix match on `:shortcode`)
- Up to 10 results
- Selected item highlighted
- Navigate with Up/Down, select with Enter/Tab, close with Esc

Triggered by typing `:` in Insert mode. Any key that doesn't form a valid shortcode closes the picker.

### Help (`:help`)

Full content area with comprehensive key and command reference. Scrollable with j/k or PageUp/PageDown.

## Vim Mode

### Normal Mode Bindings

| Key | Action |
|---|---|
| `i` | Enter Insert mode |
| `a` | Enter Insert mode, cursor forward |
| `A` | Enter Insert mode, end of line |
| `I` | Enter Insert mode, start of line |
| `o` | Enter Insert mode, new line below |
| `O` | Enter Insert mode, new line above |
| `h` / `←` | Cursor left |
| `j` / `↓` | Cursor down |
| `k` / `↑` | Cursor up |
| `l` / `→` | Cursor right |
| `w` | Word forward |
| `b` | Word back |
| `0` | Line start |
| `$` | Line end |
| `gg` | Go to top |
| `G` | Go to bottom |
| `dd` | Delete line |
| `x` | Delete character |
| `yy` | Yank line to clipboard |
| `p` | Paste from clipboard |
| `u` | Undo |
| `Ctrl+r` | Redo |
| `Enter` | Send message |
| `:` | Enter command mode |
| `Esc` | Close user list overlay |

### Insert Mode Bindings

| Key | Action |
|---|---|
| `Esc` | Return to Normal mode |
| `Enter` | Send message |
| `Shift+Enter` | Insert newline (multi-line mode) |
| `:` | Start emoji picker |
| Other | Pass to text input, trigger typing indicator |

## Color Scheme

| Element | Color | Style |
|---|---|---|
| Background | `Rgb(0, 0, 0)` | Black |
| Chat area | `Rgb(25, 25, 25)` | Dark gray |
| Input box | `Rgb(45, 45, 45)` | Medium gray |
| Header badge | Blue bg, white text | Bold |
| Usernames | Cyan | Bold |
| Timestamps | Dark gray | Normal |
| System messages | Magenta | Normal |
| Typing indicator | Yellow | Italic |
| Voice connected | Green | Normal |
| Voice connecting | Yellow | Normal |
| Voice disconnected | Gray | Normal |
| Command input | Cyan | Bold |
| Errors | Red | Bold |
