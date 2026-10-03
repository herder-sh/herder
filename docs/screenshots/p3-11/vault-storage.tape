# Generate the ANSI frames with the ignored vault_storage_frames TUI test and set HERDER_FRAMES.
# The frames use the actual TUI renderer with a fake vault's host list; no daemon runs.
Output docs/screenshots/p3-11/vault-storage.gif
Set FontSize 16
Set Width 1100
Set Height 650
Set Padding 12
Set Theme { "background": "#111216", "foreground": "#e6e6e6" }
Hide
Type "clear; cat $HERDER_FRAMES/vault-0.ansi; sleep 1.2; clear; cat $HERDER_FRAMES/vault-1.ansi; sleep 1.2; clear; cat $HERDER_FRAMES/vault-2.ansi; sleep 2.5; clear; cat $HERDER_FRAMES/vault-3.ansi; sleep 3" Enter
Sleep 200ms
Show
Sleep 8s
