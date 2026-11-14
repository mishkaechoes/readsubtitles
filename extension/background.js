// Posts captions to the ReadSubtitles app.
//
// The fetch happens here rather than in the content script on purpose. A
// content script runs in the page's origin, so a request to 127.0.0.1 would be
// a cross-origin request subject to CORS. The service worker runs in the
// extension's own origin and holds the host permission, so it can simply post.

importScripts("config.js");

const ENDPOINT = `http://127.0.0.1:${CONFIG.port}`;

async function send(path, body) {
  return fetch(ENDPOINT + path, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "X-ReadSubtitles-Token": CONFIG.token,
    },
    body: JSON.stringify(body ?? {}),
  });
}

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (message?.kind !== "caption") return false;

  send("/cue", { text: message.text })
    .then((r) => sendResponse({ ok: r.ok, status: r.status }))
    // The app not running is the normal case, not an error worth shouting
    // about: the user may simply be watching without it.
    .catch(() => sendResponse({ ok: false, status: 0 }));

  // Keeps the message channel open for the async reply.
  return true;
});
