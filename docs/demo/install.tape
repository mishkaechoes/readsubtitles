# Shows installing ReadSubtitles in one command.
#
# This is a real run of the real script, so the build step is quick: everything
# was already compiled. A first install also downloads the Xcode tools and Rust
# if they are missing, and the first build takes a few minutes.
#
# Regenerate with:  vhs docs/demo/install.tape

Output docs/demo-install.gif

Set Shell bash
Set FontFamily "Menlo"
Set FontSize 15
Set Width 1120
Set Height 720
Set Padding 26
Set Framerate 20
Set TypingSpeed 55ms
Set Theme "Catppuccin Mocha"

Sleep 1s
Type "./install.sh"
Sleep 700ms
Enter
Sleep 6s

# The one prompt: whether to set up the browser extension too.
Type "n"
Sleep 300ms
Enter
Sleep 9s
