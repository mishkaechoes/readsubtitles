// Watches the player's caption element and reports each new caption.
//
// Reading the DOM gives the exact text the author wrote, with no OCR and no
// Screen Recording permission. The trade-off is that every site renders
// captions differently, so this needs a selector per site and will break when
// one of them rebuilds its player. That is the known cost of this source; the
// screen-capture source is immune to it.

(() => {
  "use strict";

  // Ordered most specific first. The generic cue selector catches plain HTML5
  // players and anything using the standard caption pseudo-elements.
  const SELECTORS = [
    ".player-timedtext", // Netflix
    ".ytp-caption-window-container", // YouTube
    ".atvwebplayersdk-captions-overlay", // Prime Video
    ".hudson-captions", // Disney+
    '[class*="caption-window"]',
    '[class*="subtitle"]',
  ];

  // Long enough for a caption to finish rendering, short enough to stay ahead
  // of the viewer. The app applies its own settle window on top of this.
  const DEBOUNCE_MS = 70;

  let lastSent = null;
  let timer = null;
  let announcedContainer = null;
  let postedOnce = false;

  // Everything this script logs is prefixed, so the whole story can be read
  // with a single filter in the console. Without it, diagnosing "nothing is
  // being spoken" means guessing between a dozen possible causes.
  const log = (...args) => console.log("[ReadSubtitles]", ...args);

  function captionContainer() {
    for (const selector of SELECTORS) {
      const el = document.querySelector(selector);
      if (el) return el;
    }
    return null;
  }

  /// Text from the standard text-track API, for players that expose it.
  function fromTextTracks() {
    for (const video of document.querySelectorAll("video")) {
      for (const track of video.textTracks ?? []) {
        if (track.mode === "disabled") continue;
        const cues = track.activeCues;
        if (!cues || cues.length === 0) continue;
        const text = Array.from(cues)
          .map((c) => c.text ?? "")
          .join("\n");
        if (text.trim()) return text;
      }
    }
    return "";
  }

  // Marks an italic run for the app. A Unicode private-use character, because
  // it cannot occur in real subtitle text, so there is nothing for it to
  // collide with. The app strips it before speaking.
  const ITALIC_MARK = "\uE000";

  // Walk the caption nodes keeping italics, which innerText throws away.
  //
  // Italics are how subtitles mark narration, a voice on a phone, someone off
  // screen and song lyrics. That distinction is worth a different voice, and
  // the browser is the only source that can see it: OCR cannot read slant
  // reliably at subtitle sizes.
  function textWithItalics(node, italicDepth) {
    if (node.nodeType === Node.TEXT_NODE) {
      return node.nodeValue;
    }
    if (node.nodeType !== Node.ELEMENT_NODE) {
      return "";
    }
    // Players hide the previous cue rather than removing it.
    const style = window.getComputedStyle(node);
    if (style.display === "none" || style.visibility === "hidden") {
      return "";
    }

    const nowItalic =
      node.tagName === "I" ||
      node.tagName === "EM" ||
      style.fontStyle === "italic" ||
      style.fontStyle === "oblique";
    const entering = nowItalic && italicDepth === 0;

    let out = "";
    for (const child of node.childNodes) {
      out += textWithItalics(child, italicDepth + (nowItalic ? 1 : 0));
    }
    // Players put each line in its own block, so a block boundary is a line
    // break. Without this the dash convention for two speakers is lost.
    if (style.display && style.display.startsWith("block") && out && !out.endsWith("\n")) {
      out += "\n";
    }
    return entering && out.trim() ? ITALIC_MARK + out + ITALIC_MARK : out;
  }

  function currentCaption() {
    const container = captionContainer();
    const fromDom = container ? textWithItalics(container, 0) : "";
    return (fromDom && fromDom.trim() ? fromDom : fromTextTracks()).trim();
  }

  function report() {
    // Say once which selector matched, so a site that has changed its markup
    // is immediately obvious rather than silently producing nothing.
    const matched = matchedSelector();
    if (matched !== announcedContainer) {
      announcedContainer = matched;
      log(matched ? `caption container: ${matched}` : "no caption container found yet");
    }

    const text = currentCaption();
    if (text === lastSent) return;
    lastSent = text;

    if (text && !postedOnce) {
      postedOnce = true;
      log("first caption found, sending to the app:", JSON.stringify(text));
    }

    try {
      chrome.runtime.sendMessage({ kind: "caption", text }, (reply) => {
        // Reading lastError suppresses the "unchecked runtime.lastError"
        // console noise when the app or worker is not listening.
        const err = chrome.runtime.lastError;
        if (err) {
          log("could not reach the extension worker:", err.message);
        } else if (reply && !reply.ok) {
          log(`the app refused or was not listening (status ${reply.status})`);
        }
      });
    } catch {
      // The extension context goes away on reload. Nothing to do.
    }
  }

  function matchedSelector() {
    for (const selector of SELECTORS) {
      if (document.querySelector(selector)) return selector;
    }
    return document.querySelector("video") ? "video textTracks" : null;
  }

  function schedule() {
    clearTimeout(timer);
    timer = setTimeout(report, DEBOUNCE_MS);
  }

  // Watching the whole document is deliberate. Players create and destroy their
  // caption container between cues and when switching titles, so observing the
  // container itself would stop working the first time it is replaced.
  new MutationObserver(schedule).observe(document.documentElement, {
    childList: true,
    subtree: true,
    characterData: true,
  });

  // A backstop for players that repaint captions onto a canvas or otherwise
  // mutate nothing the observer can see.
  setInterval(report, 250);

  log(`watching ${location.hostname} for captions`);
})();
