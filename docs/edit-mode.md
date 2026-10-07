# Editing selected text by voice

Select text in an editor, browser input, chat box, or document. Record with
any existing dictation hotkey and say **“edit make this formal”**. The
colon forms `edit:`, `fix:`, `change:`, `rewrite:`, and `transform:` also
work. A local protocol client may set `SessionOptions.route = "edit"` and
speak only the instruction. Other ordinary verbs remain dictation: “change
the setting” does not trigger an edit.

When recording stops, the daemon pins the input window. After recognizing
an EDIT instruction, it reads PRIMARY and confirms the selected text with
a fresh Ctrl+C from that window, preserving the clipboard. A stale PRIMARY,
a stale clipboard, or no selected text cannot supply an edit target in apps
with conventional copy behavior. Conflicting PRIMARY and copied text are
rejected; a PRIMARY owned by another X11 client is ignored. The
LLM receives the instruction and selection separately, with technical spans
masked. It returns a replacement for the selection only.

Immediately before pasting, the daemon copies again and checks that the
window and selected bytes still match. It sends one Ctrl+V, serves its payload
only to the destination's X11 client while that window has focus, and
restores the clipboard. It never raises a window, selects all, deletes text,
retries a paste, or falls back to ordinary typing. Moving focus or changing
the selected bytes discards the edit. Cancelling during selection capture or
LLM work produces no replacement. Once replacement begins, cancel returns
the existing “too late” result and the clipboard transaction finishes.

EDIT uses the `[local]` host/model ladder (preferred `qwen3:14b`), plus S21's
`[format.llm]` timeout and keep-alive settings. It works independently of
whether prose formatting is enabled. Missing models, timeouts, incomplete
replies, commentary, and broken protected spans leave selected text alone and
produce an edit error notification. Errors do not copy anything to the
clipboard. The selection limit is 64 KiB; instructions are limited to 8 KiB.

```toml
[edit]
preview_only = true
```

Preview mode shows the proposed replacement without pasting. The default is
`false`. Session privacy and global history privacy suppress preview
notifications and persisted interactions. Global output policy must also allow clipboard paste. The original selection is never
stored by EDIT; ordinary interaction history can retain the spoken instruction
and successful replacement when privacy is off.

This implementation supports local, live X11 capture. Uploads, callers
without injection/context permission, profiles requiring injection off or
direct typing, and terminal
profiles cannot edit. Terminal Ctrl+C interrupts programs, so EDIT does not
send it there. Apps must use conventional Ctrl+C / Ctrl+V selection behavior
and own the copied text on the destination's X11 client. Apps configured to
copy a whole line when nothing is selected must disable that behavior before
using EDIT; X11's clipboard fallback cannot distinguish it from a selection.
A clipboard manager
or a separate-client copy owner cannot act as an edit target. Clipboard
preservation has the existing S13b limits: text, HTML with text, or image
pixels; arbitrary target bundles and file lists are not supported.

X11 exposes focus, selection owners and text, not a portable editable-widget
selection range. Checks cannot distinguish two identical selections within
one widget, or guarantee widget semantics when an application changes the
selection internally during the paste handshake. Leave the target focused
and selected while processing. All automated X11 tests run in a private Xvfb.

Manual acceptance item for the operator/integrator: in representative GTK,
Qt, and browser text inputs, select the middle of a paragraph, dictate an
edit, and confirm unchanged surrounding text, unchanged clipboard, and no
window raising. Repeat while switching focus during recognition/LLM work and
while changing the selection; the proposed edit must be discarded. Exercise
preview mode and cancellation before replacement. These checks have not been
run on the operator's live desktop by the worker.
