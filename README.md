# redline

A keyboard-driven TUI spec editor. **The human writes the spec. Agents can only
suggest.** It works like the suggestion mode of a word processor: comments,
proposed additions, edits and deletions show inline, and you accept or reject
each one. Every proposal, comment and decision goes into a history in
`~/.redline/`.

```
 ▌redline   spec.md  ~/work/payments                                   3 open · 4 decided
  3    ## Goals                                       │ THREADS  3 open
  4                                                   │
  5 ▌~ …returns a result in l̶e̶s̶s̶ ̶t̶h̶a̶n̶ ̶2̶ ̶s̶e̶c̶o̶n̶d̶s̶less    │ ▎ #1 ~ edit  claude · 2m        L5
       than 800 ms at p99.                            │ ▎ − less than 2 seconds
  6 ▏● It must be idempotent: a retry with the same   │ ▎ + less than 800 ms at p99
       key must not charge twice.                     │ ▎ A latency target needs a percentile.
                                                      │ ▎ a accept · x reject · m modify · r reply
 NORMAL   L5/24  a accept · x reject · m modify · r reply · n next thread
```

## Install

```bash
nix run github:zdcthomas/redline -- spec.md      # run
nix profile install github:zdcthomas/redline     # install
nix develop                                      # dev shell, then: cargo build
```

## Use

```bash
redline spec.md
```

| Key | Action |
|---|---|
| `j` `k` `{` `}` `gg` `G` `ctrl-d` `ctrl-u` | move |
| `n` `N` | next / previous thread; `tab` cycles the threads on one line |
| `e` or `enter` | edit the block (or the selected lines) in your editor |
| `o` `O` | new block below / above |
| `E` | edit the whole document |
| `v` | select lines |
| `dd` | delete the block (`d` on a selection) |
| `u` | undo (an undo of an accept opens the proposal again) |
| `a` | accept the proposal |
| `m` | modify the proposed text in your editor, then accept |
| `x` | reject the proposal; you can give a reason, and the agent reads it |
| `c` | comment on the block or selection |
| `r` `R` | reply on a thread / resolve a comment |
| `H` | history of changes and decisions |
| `\` | show / hide the thread sidebar |
| `?` `q` | help / quit |

### Your editor

redline has no text-entry mode of its own for the document. An edit opens
**your editor** (`$VISUAL`, `$EDITOR`, or `editor` in the config) with your
normal configuration, on only the text you edit, in a floating window:

- in tmux: a `tmux display-popup`
- outside tmux: a terminal that redline embeds in a floating pane

Save and quit to apply the change. Quit with an error (`:cq` in vim) to cancel.

`~/.redline/config.toml` (all keys optional):

```toml
editor = "nvim"        # default: $VISUAL, then $EDITOR, then vi
popup = "auto"         # auto | tmux | embedded | fullscreen
author = "zach"        # your name in the history; default: $USER
popup_width = 84       # percent
popup_height = 84
```

## Agents

An agent uses the CLI. It can read, propose, comment, reply and wait. It cannot
accept, reject, resolve or edit.

```bash
redline show spec.md                    # text + line numbers + open threads
redline propose edit spec.md --text 'old words' --new 'new words' --why 'reason'
redline propose insert spec.md --after-line 20 --text 'New paragraph.'
redline propose delete spec.md --lines 31 --why 'duplicate'
redline comment spec.md --text 'some phrase' --body 'Is this a hard requirement?'
redline reply spec.md 7 --body '…'
redline wait spec.md                    # blocks until the human acts, prints what they did
redline log spec.md                     # full history
```

The TUI shows a new proposal in less than a second. `redline wait` gives the
agent your decisions and your rejection reasons, so the loop is: propose, wait,
read, propose again.

Install the agent skill (`skill/redline/SKILL.md`):

```bash
redline skill install                   # writes ~/.claude/skills/redline/SKILL.md
```

The Nix package also has the skill at `share/redline/skill/redline`, for a
home-manager link.

## Storage

```
~/.redline/                    # or $REDLINE_HOME
  config.toml
  docs/<name>-<hash>/          # one directory per document path
    meta.json                  # the document path
    state.json                 # threads: comments and proposals, with status
    history.jsonl              # append-only log: proposals, comments, decisions, edits
    snapshot.md                # the text redline last saw
```

The spec stays a plain Markdown file where you put it. Threads point at quoted
text, not at line numbers, so they move with your edits. If something changes
the file outside redline, the history records it as an external change.
