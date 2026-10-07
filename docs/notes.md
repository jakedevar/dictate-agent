# Voice notes (the scratchpad)

Dictate a thought into a local notes list instead of the focused window.

## Saying a note

Record with any dictation hotkey and start with one of:

| You say | Saved |
|---|---|
| "**note:** buy oat milk" / "**note,** buy oat milk" | the text after the trigger |
| "**note to self** call the dentist" | the text after the trigger |
| "**quick note** / **new note** / **take a note** / **make a note** / **add a note** …" | the text after the trigger |

The trigger words are not saved. Nothing is typed into the window you were in;
a desktop notification says "Note saved".

A bare "note" is **not** a trigger, so ordinary dictation such as "note that the
deadline moved" or "I made a note of it" is still typed. If Whisper does not
hear a trigger the way you said it, force the route instead:

```bash
dictate notes new          # start a session whose transcript goes to the scratchpad
dictate stop               # (or toggle, or your hotkey) ends it and saves the note
dictate start --route note # the same thing
```

The hub's **Notes** page has a *Dictate a note* button that does this.

## Reading them back

```bash
dictate notes                      # newest first
dictate notes search oat milk      # case-insensitive substring
dictate notes list --limit 20
dictate notes show 7               # the whole note (multi-line notes included)
dictate notes copy 7               # to the clipboard (wl-copy, xclip or xsel)
dictate notes rm 7
dictate notes --json               # protocol JSON, for scripts
```

The hub's Notes page lists, searches, copies and deletes the same notes.

## Privacy and retention

Notes live in the history database (`history.db`, table `notes`), so they follow
its rules:

- **Privacy mode** (`[history] privacy_mode`, or `--privacy` / `options.privacy`
  for one session) stores nothing. The note is *not* typed instead; a
  notification says it was not saved. History disabled behaves the same way.
- **Retention** (`[history] retention_days`) expires old notes along with old
  dictations. With no window, notes are kept until you delete them.
- `dictate history --purge` removes dictations only. Notes are explicit saves, so
  they are deleted one at a time with `dictate notes rm`.
- Note text is also part of the dictation's history row (like any other
  transcript) and is subject to the same privacy/retention there.

A connection granted only the `type` route (a remote client) cannot write to the
scratchpad by saying a trigger; the `note` route has to be granted.

See `docs/protocol.md` ("Scratchpad (S35)") for the wire types.
