# Shows what ReadSubtitles does with the captions it is given.
#
# The scene is invented for the demo (docs/demo/captions.sh) rather than
# recorded from a film: this way the recording carries nobody's viewing habits
# and nobody's copyrighted dialogue, and the lines can be chosen to exercise
# each signal the app reads - a speaker label, a dash-alternated cue, and an
# italic aside. Everything after the prompt is the real program, real timings.
#
# Regenerate with:  vhs docs/demo/usage.tape

Output docs/demo-usage.gif

Set Shell bash
Set FontFamily "Menlo"
Set FontSize 17
Set Width 1180
Set Height 320
Set Padding 28
Set Framerate 20
Set TypingSpeed 55ms
Set Theme "Catppuccin Mocha"

# Plumbing, hidden because it is scaffolding for the recording rather than part
# of using the app. The alias pins a spare port: the default 53117 is where a
# real extension posts, and recording on it would capture whatever the person
# at the machine happens to be watching. A real user needs no such flag.
Hide
Type "export PATH=$PWD/target/release:$PATH"
Enter
Type "alias readsubtitles='readsubtitles --bridge-port 53999'"
Enter
Type "./docs/demo/captions.sh 53999 &"
Enter
Type "clear"
Enter
Show

Sleep 1s
Type "readsubtitles --source browser --dry-run --no-diarize"
Sleep 700ms
Enter
Sleep 15s
