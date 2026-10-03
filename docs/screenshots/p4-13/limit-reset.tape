# Generate the ANSI frames with the ignored limit_reset_frames TUI test and set HERDER_FRAMES.
# The frames use the actual TUI renderer with fake session events; no provider or daemon runs.
Output docs/screenshots/p4-13/limit-reset.gif
Set FontSize 16
Set Width 1100
Set Height 650
Set Padding 12
Set Theme { "background": "#111216", "foreground": "#e6e6e6" }
Hide
Type "clear; cat $HERDER_FRAMES/limit-0.ansi; sleep 2; clear; cat $HERDER_FRAMES/limit-1.ansi; sleep 2; clear; cat $HERDER_FRAMES/limit-2.ansi; sleep 3" Enter
Sleep 200ms
Show
Sleep 6s
