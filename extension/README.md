# ReadSubtitles Bridge

Sends captions from the browser to the ReadSubtitles app, so they can be spoken aloud without reading the screen.

Install it with `./bundle/make-extension.sh`, which writes `config.js` with the port and shared token the app expects, then follow the instructions it prints.

`config.js` is generated and git-ignored. It holds the token that lets this extension talk to the app, and nothing else.
