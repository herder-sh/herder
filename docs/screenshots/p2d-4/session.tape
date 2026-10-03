# The agent session view driven against a local daemon and a real Claude turn.
# Run from the repo root with a paired client in $XDG_CONFIG_HOME: vhs docs/screenshots/p2d-4/session.tape
Output docs/screenshots/p2d-4/session.gif
Set FontSize 14
Set Width 1400
Set Height 820
Set Padding 12
Set Theme { "background": "#111216", "foreground": "#e6e6e6" }
Set TypingSpeed 40ms
Hide
Type "COLORTERM=truecolor target/debug/herder" Enter
Sleep 2s
Type "j" Enter Type "i"
Show
Sleep 1s
Type "/mo"
Sleep 1.5s
Backspace 3
Type "Write a three-line haiku about herding cats into notes.md, then reply in one sentence."
Sleep 500ms
Enter
Sleep 14s
Type "y"
Sleep 14s
Escape
Sleep 500ms
Type "["
Sleep 500ms
Type "["
Sleep 500ms
Type "["
Sleep 800ms
Type "e"
Sleep 2.5s
