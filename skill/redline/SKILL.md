---
name: redline
description: Use when the user works on a spec or design document with redline (the TUI spec editor), or asks you to review, comment on, or suggest changes to a document "in redline". Lets you read the document, propose edits/insertions/deletions, comment, and wait for the human's decisions through the `redline` CLI.
---

# redline

redline is a spec editor. The human owns the document and edits it in a TUI.
You are a reviewer: you **propose** changes and **comment**. The human accepts
or rejects each proposal. Every proposal, comment and decision goes into a
history in `~/.redline/`.

## Rules

- Never write to the document file directly (no Edit, Write, sed, redirects).
  A direct write skips the human's review, and the history records it as an
  external change. Use `redline propose` for every change.
- One proposal per idea. Small proposals get accepted; a rewrite of a full
  section in one proposal gets rejected.
- Give each proposal a `--why`. The human reads it when they decide.
- A question or a concern with no concrete replacement text is a `comment`,
  not a proposal.
- Line numbers change after each accepted proposal or human edit. Run
  `redline show` again before you use `--lines`.

## Loop

1. Read: `redline show FILE` prints the text with line numbers and the open threads.
2. Suggest: `redline propose …` and `redline comment …` (commands below).
3. Wait: `redline wait FILE` blocks until the human acts, then prints what they
   did: accepted, rejected (with their reason), replied, resolved, or edited.
   It returns after 100 seconds with "no human activity" if nothing happened;
   run it again, or pass `--timeout`.
4. Read the decisions. A rejection reason is a preference: apply it to your
   next proposals. Reply on a thread if the human asked something.
5. Go to step 1.

## Commands

Point at text with `--text 'exact text'` (must match the document exactly) or
`--lines 12-15` (from `redline show`). If the text occurs more than once, add
`--near-line N`. Any text value can be `-` to read stdin, for multi-line text.

```bash
redline show FILE                       # text + line numbers + open threads
redline threads FILE [--all]            # threads only; --all includes decided ones
redline log FILE [-n 20]                # history of changes and decisions

redline propose edit FILE --text 'old words' --new 'new words' --why 'reason'
redline propose edit FILE --lines 12-14 --new - --why 'reason' <<'EOF'
replacement for lines 12 to 14
EOF
redline propose insert FILE --after-line 20 --text 'New paragraph.' --why 'reason'
redline propose insert FILE --after 'last list item' --text '- one more item' --tight
redline propose delete FILE --lines 31 --why 'duplicates line 12'

redline comment FILE --text 'some phrase' --body 'Is this a hard requirement?'
redline reply FILE 7 --body 'Agreed, I will propose the smaller version.'
redline wait FILE [--timeout 300]
```

- `insert` adds the text as its own paragraph. `--tight` adds it on the next
  line with no blank lines (list items, table rows). `--after-line 0` is the top.
- `--as NAME` (or `$REDLINE_AUTHOR`) sets your name in the history. Default: `agent`.
- `--json` on `show`, `threads`, `log` and `wait` gives machine-readable output.
- You cannot accept, reject, resolve or edit. Those are human actions in the TUI.
- If the human is not in the TUI yet, tell them to run `redline FILE`.
