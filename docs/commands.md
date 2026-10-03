# Commands

All commands are entered by pressing `:` in any screen. Commands are context-sensitive — some only work while in a room, others work anywhere.

## Navigation

| Command | Aliases | Context | Action |
|---|---|---|---|
| `:q` | `:quit`, `:leave` | InRoom | Leave the current room, return to RoomChoice |
| `:q` | `:quit`, `:leave` | RoomChoice | Quit the application |
| `:q` | `:quit`, `:leave` | Other screens | Return to RoomChoice |
| `:qq` | `:qa`, `:quit!` | Anywhere | Force quit immediately (no confirmation) |
| `:h` | `:help` | Anywhere | Show the help screen |

## Room Management

| Command | Aliases | Context | Action |
|---|---|---|---|
| `:l` | `:list` | InRoom | Open the room switcher overlay |
| `:s <room>` | `:switch <room>` | InRoom | Switch directly to a room by name |
| `:share` | `:invite` | InRoom | Generate an invite code for the current room |
| `:rename <name>` | — | InRoom | Rename the current room (owner only) |
| `:delete` | — | InRoom | Delete the current room (owner only) |
| `:transfer <user>` | — | InRoom | Transfer room ownership to another user (owner only) |

## Joining

| Command | Aliases | Context | Action |
|---|---|---|---|
| `:j <code>` | `:join <code>` | Anywhere | Join a room using an invite code |
| `:dm <user>` | — | Anywhere | Create or open a direct message with a user |

## Voice Chat

| Command | Aliases | Context | Action |
|---|---|---|---|
| `:vc` | — | InRoom | Join voice chat in the current room (takes no arguments) |
| `:vcl` | — | Anywhere | Leave voice chat, or cancel a pending join or reconnect |
| `:m` | `:mute` | Anywhere | Mute microphone (kept across reconnects) |
| `:um` | `:unmute` | Anywhere | Unmute microphone |

## UI

| Command | Aliases | Context | Action |
|---|---|---|---|
| `:u` | `:users` | InRoom | Toggle the online users overlay |

## Account

| Command | Aliases | Context | Action |
|---|---|---|---|
| `:register` | `:reg` | Anywhere | Start the registration flow (add a new SSH key) |

## Command Parsing

Commands are parsed by splitting the input on whitespace:

```
:rename new-name  → parts = ["rename", "new-name"]
:dm alice   → parts = ["dm", "alice"]
```

The first part determines the command. Remaining parts are arguments. For commands that take a single argument (like `:dm`, `:rename`, `:transfer`), the second part is used directly. For commands that take no arguments (like `:q`, `:help`), remaining parts are ignored.

## Command Mode Display

When `:` is pressed, the footer changes to show the current command input in cyan bold:

```
:dm alice
```

Pressing Enter executes the command. Pressing Esc cancels and returns to the previous screen state.

## Unknown Commands

If a command is not recognized, a status message is displayed:

```
Unknown command: foo. Type :help for list.
```
