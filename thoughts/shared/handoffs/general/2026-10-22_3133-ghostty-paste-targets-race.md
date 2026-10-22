# #3133 Ghostty clipboard paste always timed out

## Root cause (reproduced and measured, private Xvfb, Ghostty 1.3.1, synthetic text)

The X11 client attribution was **not** the problem. The Ghostty window that
requested the selection (`0x200003`, GTK's leader window) and the focused window
(`0x200005`) share one X11 client resource base, so `same_client` passed.

The real defect is an ordering race in `X11Injector::paste_transaction`:

1. We claim CLIPBOARD, then immediately send Ctrl+V.
2. GTK4 (Ghostty) learns what the new owner offers by sending a **TARGETS**
   request on the owner change. Nothing answered it: `transfer_to` only starts
   serving after `send_paste`.
3. Ghostty's key event is processed while its clipboard format list is still
   empty. `performable:ctrl+v=paste_from_clipboard` is therefore not performable
   and Ghostty forwards a raw `0x16` to the pty. That is the byte the Xvfb repro
   saw. Ghostty never asked for text, and our 2 s wait ended in
   "paste unconfirmed; no typing retry".

Evidence: the only `SelectionRequest` in the whole transaction was target 308
(`TARGETS`) from `0x200003`; no UTF8_STRING request ever arrived. The same
Ghostty pastes from `xclip -i` correctly when Ctrl+V comes 0.5 s after the copy.

## Fix (`crates/dictate-inject/src/clipboard.rs`)

`PasteSelection::send_paste` first calls `await_destination_formats`: for up
to 200 ms it serves the destination client's first request under the existing
guard (owner + focus check under a server grab, factored into `serve_guarded`),
refuses every other client, then settles 40 ms so the toolkit's main loop
processes the reply before the key. Destinations that never query (xterm) cost
the 200 ms grace once per paste. Unchanged: no typing retry after an uncertain
paste, no restore after a timeout, same-client-only serving, focus re-check at
delivery. `replace_selection` (EDIT) goes through `send_paste` and gets the same
fix.

Result on the same Ghostty/Xvfb: `Injected { method: Paste }`, whole
transaction 40 ms, exact text delivered, 6/6 runs.

## Tests (`--features x11-tests`, private Xvfb)

- `paste_key_waits_for_the_destinations_format_query`: a destination that asks
  TARGETS 30 ms after the owner change must not see the key first. Verified to
  FAIL with the wait removed.
- `silent_destination_gets_the_key_after_a_bounded_wait`: xterm-like client still
  gets Ctrl+V after the bounded wait (>= grace, < 1 s).
- `format_wait_refuses_other_clients`: a clipboard manager asking during the
  wait receives no payload.

## Notes

Live workaround (Ghostty context profile `inject = "type"`) can be retired after
the PM verifies paste on the real Ghostty.
