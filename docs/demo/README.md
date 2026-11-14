# Demo recordings

The two GIFs in the README are made with [vhs](https://github.com/charmbracelet/vhs), which drives a real terminal and records only that. Nothing of the machine's screen, and nothing of whatever its owner happens to be watching, can get into them.

```sh
brew install vhs
vhs docs/demo/usage.tape
vhs docs/demo/install.tape
```

`captions.sh` feeds a short invented scene to a running bridge. It exists so the usage recording can show the real pipeline - real timings, real speaker separation - without putting someone's viewing habits or anyone's copyrighted dialogue into a public repository. The lines are written to exercise each signal the app reads: a speaker label, a dash-alternated cue, and an italic aside.

Two details in the tapes are deliberate rather than incidental:

**The usage recording pins port 53999.** The default, 53117, is where a real extension posts. A first attempt at this recording used the default and captured the dialogue of the film that happened to be playing at the time. A real user needs no such flag.

**`captions.sh` waits for the port to accept a connection** rather than sleeping for a guessed interval. Typing inside a `Hide` block still takes real time, so the first version's timer fired before the reader had started and produced an empty scene.
