# redline

Review a spec with an agent, the way suggestion mode works in a word processor.
**The human writes the spec. Agents can only suggest.** Comments, proposed
additions, edits and deletions show inline, and the human accepts or rejects
each one.

redline is three layers:

1. **An event log.** Humans and agents push events (proposed, commented,
   replied, accepted, rejected, resolved, edited). Nothing else is stored. The
   threads you see are the fold of the events.
2. **A CLI.** `redline` is the only program that reads and writes the log. It
   is the contract for every front end.
3. **Front ends.** A Neovim plugin, a standalone TUI, a native app, and the
   agent skill. The plugin and the agent call the CLI; the TUI and the app
   use the same core as a library. An integration for another editor is a small wrapper
   around the same commands.

## Install

```bash
nix run github:zdcthomas/redline -- spec.md      # run the TUI
nix profile install github:zdcthomas/redline     # install the CLI
nix develop                                      # dev shell, then: cargo build
```

The flake has three packages: `redline` (the binary, with the skill and the
plugin under `share/redline/`), `redline-nvim` (the Neovim plugin as a
`vimPlugin`) and `redline-app` (the native app).

## Neovim

Add `nvim/` to the runtime path (or use the `redline-nvim` package), then run
`:Redline` in the spec buffer.

```lua
require('redline').setup({ author = 'zach' })   -- optional
```

Proposals show in the buffer as virtual text: removed text is struck through,
added text is green, commented text has a highlight. You edit the buffer as
usual. New proposals from the agent show while you write.

| Key | Command | Action |
|---|---|---|
| `]r` `[r` | `:Redline next` / `prev` | next / previous thread |
| `<leader>ra` | `:Redline accept` | accept the proposal at the cursor |
| `<leader>rm` | `:Redline modify` | change the proposed text, then accept |
| `<leader>rx` | `:Redline reject` | reject, with a reason for the agent |
| `<leader>rc{motion}` | `:{range}Redline comment` | comment on the text of the motion |
| `<leader>rd{motion}` | `:{range}Redline delete` | suggest a delete of that text |
| `<leader>re{motion}` | `:{range}Redline edit` | suggest an edit of that text |
| `<leader>rr` | `:Redline reply` | reply on the thread |
| `<leader>rR` | `:Redline resolve` | resolve the comment |
| `<leader>rt` | `:Redline threads` | the thread list, as a buffer |
| `<leader>ro` | `:Redline thread` | open the thread as a buffer |
| `<leader>rh` | `:Redline history` | the event history |
| `g?` | `:Redline help` | show the keys for the current buffer |
| `<leader>rs` | `:Redline style [name]` | how a comment shows: `bg` (background), `brackets` (virtual `{` `}`) or `lines` (an underline, and the comment on a colored virtual line below, like LSP virtual lines); also `comment_style` in `setup` |

- **Operators:** `c`, `d` and `e` after the prefix wait for a motion or a text
  object, like `d` and `c` in Vim: `<leader>rcip` comments on the paragraph,
  `<leader>rdiw` suggests a delete of the word. The doubled key (`<leader>rcc`)
  acts on the line, and a visual selection works too. A delete or an edit from
  you is a suggestion: the text does not change until someone accepts it.
- **Thread list buffer:** one thread per line. The thread on the cursor line
  shows as a highlight in the document. `<cr>` opens the thread, `gd` goes to
  the text, `a` `x` `m` `r` `R` act on the line.
- **Thread buffer:** the full thread as Markdown. Write below the marker line
  and `:w` to send a reply. `a` `x` `m` `R` decide, `<cr>` goes to the text,
  `q` goes back to the list.
- An accept changes the buffer, not the file, so `u` undoes it. A save records
  your edit.

## TUI

```bash
redline spec.md
```

| Key | Action |
|---|---|
| `j` `k` `{` `}` `gg` `G` `ctrl-d` `ctrl-u` | move |
| `n` `N` | next / previous thread; `tab` cycles the threads on one line |
| `t` | thread list mode: `j` `k` move, `enter` opens the thread as a page |
| `e` or `enter` | edit the block (or the selected lines) in your editor |
| `o` `O` | new block below / above |
| `E` | edit the whole document |
| `v` | select lines |
| `w` `b` `h` `l` | text cursor: move by word / by character; `esc` goes back to lines |
| `w` … `v` … `c` | select an exact text range, then comment on it. `v` then `w` selects one word. `e` and `d` also act on the range |
| `dd` | delete the block (`d` on a selection) |
| `u` | undo (an undo of an accept opens the proposal again) |
| `a` `m` `x` | accept / modify then accept / reject with a reason |
| `c` `r` `R` | comment / reply / resolve |
| `H` | history |
| `\` `?` `q` | show or hide the threads / help / quit |

The TUI has no text-entry mode for the document. An edit opens your editor
(`$VISUAL`, `$EDITOR`, or `editor` in the config) on only the text you edit:
in a `tmux display-popup` inside tmux, and in an embedded floating terminal
outside tmux.

`~/.redline/config.toml` (all keys optional):

```toml
editor = "nvim"        # default: $VISUAL, then $EDITOR, then vi
popup = "auto"         # auto | tmux | embedded | fullscreen
author = "zach"        # your name in the history; default: $USER
popup_width = 84       # percent
popup_height = 84
```

## Native app

```bash
nix run github:zdcthomas/redline#redline-app -- spec.md
```

The app is for people who do not use a terminal. It looks and works like a
document editor, with no modes and no key commands to learn:

- **Read.** The spec shows as a page. Headings, lists and code show as
  formatted text, not as Markdown marks.
- **Write.** Click a paragraph: it becomes a text box in place. Click outside
  the box, or press Done, to save. "Add a paragraph" is at the end of the page.
- **Comment.** Click in a paragraph, select words if you want (a double-click
  selects one word), then press Comment. The comment goes on the selected
  words, or on the paragraph.
- **Review.** Each suggestion and comment is a card in the right column, with
  Accept, Reject, Reply and Resolve buttons. Reject asks for an optional
  reason, which the agent reads. A suggestion also shows in the text: removed
  text is red and struck through, added text is green.
- **Activity.** The second tab of the column shows the history.
- **Undo.** The Undo button, or Ctrl+Z outside a text box.

The app is in [app/](app/), written with egui. It shows new suggestions from
the agent live.

## Agents

```bash
redline show spec.md                    # text + line numbers + open threads
redline propose edit spec.md --text 'old words' --new 'new words' --why 'reason'
redline propose insert spec.md --after-line 20 --text 'New paragraph.'
redline propose delete spec.md --lines 31 --why 'duplicate'
redline comment spec.md --text 'some phrase' --body 'Is this a hard requirement?'
redline reply spec.md 7 --body '…'
redline wait spec.md                    # blocks until the human acts, prints what they did
```

`redline skill install` writes the agent skill to `~/.claude/skills/redline`.

The decision verbs need `--human`. An agent can pass that flag, so the rule
"agents only suggest" is a convention that the skill states. The log records
the actor of each event.

## Write an integration

An editor integration needs these commands. Each prints JSON with `--json`.
With `--stdin` a command works on the text of the unsaved buffer, which the
editor sends on stdin, and it does not touch the file. Set `REDLINE_ROLE=human`
in the environment, or pass `--human`.

| Command | Use |
|---|---|
| `threads FILE --json --stdin` | The open threads. Each has `range.start` / `range.end` as `{line, col}` (0-based, byte columns) in the buffer text: draw these. |
| `thread FILE ID --json --stdin` | One thread with its replies. |
| `accept FILE ID --json --stdin [--new TEXT]` | Records the decision and returns `edit: {line, remove, insert}`: replace `remove` lines at `line` with the `insert` lines in the buffer. Without `--stdin` it changes the file. |
| `reject FILE ID [--note TEXT]`, `resolve FILE ID`, `reopen FILE ID` | Decisions. |
| `comment FILE --lines A-B --body TEXT --stdin`, `reply FILE ID --body TEXT` | Human comments. |
| `sync FILE` | Call after each save. Records the saved change as the human's edit. |
| `watch FILE` | Runs until stopped. Prints one JSON line for each new event: refresh on each line. |
| `log FILE --json` | The history. |

[nvim/lua/redline/init.lua](nvim/lua/redline/init.lua) is a full example.

## The event log

```
~/.redline/                    # or $REDLINE_HOME
  config.toml
  replica                      # the id of this machine's log
  docs/<name>-<hash>/          # one directory per document path
    meta.json                  # the document path
    events/<replica>.jsonl     # append-only log, one file per replica
    snapshot.md                # the text redline last saw
```

One event:

```json
{"id":"0000000c-93d0c69a","lamport":12,"replica":"93d0c69a","at":"…","actor":"claude","agent":true,
 "type":"proposed","kind":"edit","line":4,"quote":"less than 2 seconds","new":"less than 800 ms","tight":false,"why":"…"}
```

- **Merge.** The log is a grow-only set. Each replica appends only to its own
  file, so a file-sync tool can copy logs between machines with no conflict,
  and `redline merge FILE other.jsonl` adds the events of another log. A merge
  is a set union: the order and the number of merges do not matter.
- **Same result on each replica.** The fold sorts the events by Lamport clock,
  then replica id. Two decisions on one thread: the first in that order wins.
- **Granularity.** Edits in a row by one actor, with no other event between
  them and less than 30 minutes apart, are one event. The new event covers
  the full change and names the event that it `supersedes`; the fold ignores
  superseded events. Three saves give one edit event.
- **Anchors.** A thread points at quoted text plus a line hint. Edit events
  carry the changed lines, so the fold moves the hints with each edit.
- **Limit.** The log is a CRDT. The document text is not: it is a plain file
  with one writer, the human. redline does not merge two diverged copies of
  the spec.
