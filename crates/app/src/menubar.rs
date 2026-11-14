//! The menu bar app: a status item, global hotkeys, and the engine behind them.
//!
//! Written for a blind primary user, which drives two choices that look odd
//! otherwise. Every state change is spoken, not just shown, because the person
//! using this cannot see the menu bar. And the start/stop control is a single
//! toggle on one hotkey rather than two separate keys, because one key whose
//! effect is announced is easier to use than two that have to be remembered
//! apart.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSControlStateValueOff, NSControlStateValueOn,
    NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSObject, NSString, NSTimer};
use std::cell::RefCell;

use readsubtitles_capture::Target;
use readsubtitles_ocr::Level;
use readsubtitles_voice::{AppleVoice, Speaker};

use crate::engine::{self, EngineConfig, Running};
use crate::settings::{Settings, Source};

/// Toggle reading on and off. Chosen to avoid colliding with anything a
/// browser or player binds.
const TOGGLE_KEY: Code = Code::KeyS;
/// Re-draw the subtitle region.
const REGION_KEY: Code = Code::KeyR;
/// Quit outright.
const QUIT_KEY: Code = Code::KeyQ;
fn hotkey_modifiers() -> Modifiers {
    Modifiers::CONTROL | Modifiers::ALT | Modifiers::SUPER
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);
static SETTINGS: Mutex<Option<Settings>> = Mutex::new(None);
static IS_ON: AtomicBool = AtomicBool::new(false);
/// Set by the hotkey thread and picked up by the main-thread timer, because the
/// picker's overlay can only be driven from the main thread.
static PICK_REQUESTED: AtomicBool = AtomicBool::new(false);
/// The last ElevenLabs voice seen in settings. Switching to the built-in voice
/// clears the setting, and without this the menu would have no id to switch
/// back to and the choice would be one-way.
static LAST_ELEVENLABS: Mutex<Option<String>> = Mutex::new(None);

fn last_elevenlabs_voice() -> Option<String> {
    LAST_ELEVENLABS.lock().unwrap().clone()
}

fn settings() -> Settings {
    SETTINGS.lock().unwrap().clone().unwrap_or_default()
}

/// Say something to the user in the on-device voice.
///
/// Status messages always use the on-device voice even when ElevenLabs is
/// configured: they must work with no network and cost nothing, and the user
/// needs to hear "stopped" even when the paid backend is what just failed.
///
/// A dedicated long-lived thread owns the voice. Both halves of that matter.
/// `AVSpeechSynthesizer` speaks asynchronously and falls silent if it is
/// deallocated mid-utterance, so a synthesizer built and dropped inside this
/// function would cut off every announcement it made. And the voice is not
/// `Send`, so it cannot simply be parked in a static.
fn announce(text: &str) {
    static ANNOUNCER: OnceLock<Sender<String>> = OnceLock::new();
    let tx = ANNOUNCER.get_or_init(|| {
        let (tx, rx) = channel::<String>();
        std::thread::spawn(move || {
            let Ok(mut voice) = AppleVoice::best_for("en-US") else {
                return;
            };
            for line in rx {
                voice.speak(&line).ok();
                // Let each announcement finish before starting the next, so a
                // burst of state changes does not talk over itself.
                while voice.is_speaking() {
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        });
        tx
    });
    tx.send(text.to_string()).ok();
}

fn engine_config(s: &Settings) -> EngineConfig {
    EngineConfig {
        source: s.source,
        bridge_port: s.bridge_port,
        multi_voice: s.multi_voice,
        diarize: s.diarize,
        listen_ahead: std::time::Duration::from_millis(s.listen_ahead_ms),
        cast_overrides: s.cast_overrides.clone(),
        debug_regions: false,
        on_browser_silence: Some(Box::new(announce)),
        target: match &s.window {
            Some(w) if !w.is_empty() => Target::WindowTitle(w.clone()),
            _ => Target::Display(s.display),
        },
        band: s.band,
        language: s.language.clone(),
        level: Level::Accurate,
        fps: s.fps,
        verbose: false,
    }
}

/// Build the voice, announcing aloud if the chosen one is unavailable.
///
/// In a bundled app stderr goes nowhere anyone will read, so a silent fallback
/// looks exactly like "the app ignored my settings".
fn speaker_factory(s: &Settings) -> engine::SpeakerFactory {
    engine::speaker_factory(
        s.elevenlabs_voice.clone(),
        s.elevenlabs_cast
            .iter()
            .map(|e| e.voice_id.clone())
            .collect(),
        s.voice.clone(),
        s.language.clone(),
        s.rate,
        Some(Box::new(|_| {
            announce("The Eleven Labs voice is unavailable, so I am using the built-in voice.");
        })),
    )
}

pub fn start_reading() {
    let mut guard = RUNNING.lock().unwrap();
    if guard.is_some() {
        return;
    }
    let s = settings();
    announce("Starting. Preparing the recognizer.");
    match engine::start(engine_config(&s), speaker_factory(&s)) {
        Ok(running) => {
            *guard = Some(running);
            IS_ON.store(true, Ordering::Relaxed);
            drop(guard);
            announce("Reading subtitles.");
        }
        Err(e) => {
            drop(guard);
            eprintln!("could not start: {e}");
            // The likely causes are a missing Screen Recording permission or a
            // window that is no longer open, and the user cannot see either
            // dialog, so say what to do rather than failing silently.
            announce(
                "Could not start. Check that screen recording is allowed in System Settings, \
                 and that the video window is open.",
            );
        }
    }
}

pub fn stop_reading() {
    let taken = RUNNING.lock().unwrap().take();
    if let Some(mut r) = taken {
        let watching = r.watching.clone();
        let spoken = r.stats.spoken.load(Ordering::Relaxed);
        let cut_short = r.stats.dropped.load(Ordering::Relaxed);
        r.stop();
        IS_ON.store(false, Ordering::Relaxed);
        println!("stopped: {spoken} line(s) from {watching}, {cut_short} cut short");
        announce("Stopped.");
    }
}

pub fn toggle_reading() {
    if IS_ON.load(Ordering::Relaxed) {
        stop_reading();
    } else {
        start_reading();
    }
}

/// Re-draw the watched region and save it.
///
/// Must run on the main thread, which is why it is reachable from the menu and
/// from a main-thread hop, but not called directly on the hotkey thread.
fn pick_region(mtm: MainThreadMarker) {
    let was_running = IS_ON.load(Ordering::Relaxed);
    // The overlay sits above everything, so leaving capture running would mean
    // reading the dimming layer instead of the film.
    stop_reading();

    let s = settings();
    let target = match &s.window {
        Some(w) if !w.is_empty() => Target::WindowTitle(w.clone()),
        _ => Target::Display(s.display),
    };

    announce("Drag a box around the subtitles. Press escape to cancel.");
    let Some(selection) = crate::picker::pick_region_for_capture(mtm) else {
        announce("Region unchanged.");
        if was_running {
            start_reading();
        }
        return;
    };

    let area = match readsubtitles_capture::target_area(&target) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("could not locate the video window: {e}");
            announce("Could not find the video window. The region was not changed.");
            return;
        }
    };

    let band = crate::picker::band_within(selection, area);
    let mut updated = s;
    updated.band = band;
    if let Err(e) = updated.save() {
        eprintln!("could not save settings: {e}");
    }
    *SETTINGS.lock().unwrap() = Some(updated);
    println!(
        "region set to {:.3},{:.3},{:.3},{:.3} of the target",
        band.x, band.y, band.width, band.height
    );
    announce("Region set.");

    if was_running {
        start_reading();
    }
}

/// Change one setting, save it, and say what changed.
///
/// Every settings action funnels through here so that saving and announcing can
/// never be forgotten for one of them. The announcement is the whole point: the
/// person using this cannot see the menu they just clicked, and a helper
/// changing a setting for them needs to hear that it took.
fn update_settings(spoken: &str, change: impl FnOnce(&mut Settings)) {
    let restart = IS_ON.load(Ordering::Relaxed);
    if restart {
        stop_reading();
    }

    let mut s = settings();
    change(&mut s);
    if let Err(e) = s.save() {
        eprintln!("could not save settings: {e}");
    }
    if s.elevenlabs_voice.is_some() {
        *LAST_ELEVENLABS.lock().unwrap() = s.elevenlabs_voice.clone();
    }
    *SETTINGS.lock().unwrap() = Some(s);
    announce(spoken);

    if restart {
        start_reading();
    }
}

/// The speaking rates offered in the menu.
///
/// Experienced screen-reader users commonly run well above 1.0, and the backlog
/// policy depends on being able to out-pace the dialogue, so the range reaches
/// well past "normal".
const RATES: [(f32, &str); 6] = [
    (0.8, "Slower (0.8x)"),
    (1.0, "Normal (1.0x)"),
    (1.2, "Faster (1.2x)"),
    (1.5, "Fast (1.5x)"),
    (1.8, "Very fast (1.8x)"),
    (2.2, "Fastest (2.2x)"),
];

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "RSMenuTarget"]
    #[thread_kind = MainThreadOnly]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(toggle:))]
        fn toggle_action(&self, _sender: Option<&AnyObject>) {
            toggle_reading();
            refresh_title(self.mtm());
            rebuild_menu(self.mtm());
        }

        #[unsafe(method(useScreen:))]
        fn use_screen_action(&self, _sender: Option<&AnyObject>) {
            update_settings(&format!("Now {}.", Source::Screen.spoken_name()), |s| {
                s.source = Source::Screen;
            });
            rebuild_menu(self.mtm());
        }

        #[unsafe(method(useBrowser:))]
        fn use_browser_action(&self, _sender: Option<&AnyObject>) {
            update_settings(&format!("Now {}.", Source::Browser.spoken_name()), |s| {
                s.source = Source::Browser;
            });
            rebuild_menu(self.mtm());
        }

        #[unsafe(method(useAppleVoice:))]
        fn use_apple_voice_action(&self, _sender: Option<&AnyObject>) {
            update_settings("Using the built-in voice.", |s| s.elevenlabs_voice = None);
            rebuild_menu(self.mtm());
        }

        #[unsafe(method(useElevenLabs:))]
        fn use_elevenlabs_action(&self, _sender: Option<&AnyObject>) {
            // Only offered when a voice id is already configured, so there is
            // nothing to prompt for here.
            update_settings("Using the Eleven Labs voice.", |s| {
                s.elevenlabs_voice = s.elevenlabs_voice.clone().or(last_elevenlabs_voice());
            });
            rebuild_menu(self.mtm());
        }

        /// The chosen rate is carried in the menu item's tag, as tenths, since
        /// a tag is an integer.
        #[unsafe(method(setRate:))]
        fn set_rate_action(&self, sender: Option<&AnyObject>) {
            let Some(item) = sender.and_then(|s| s.downcast_ref::<NSMenuItem>()) else {
                return;
            };
            let rate = item.tag() as f32 / 10.0;
            update_settings(&format!("Speed {rate:.1}"), |s| s.rate = rate);
            rebuild_menu(self.mtm());
        }

        #[unsafe(method(toggleMultiVoice:))]
        fn toggle_multi_voice(&self, _sender: Option<&AnyObject>) {
            let on = !settings().multi_voice;
            update_settings(
                if on {
                    "Different voices for characters, on"
                } else {
                    "One voice for everything"
                },
                |s| s.multi_voice = on,
            );
        }

        #[unsafe(method(toggleDiarize:))]
        fn toggle_diarize(&self, _sender: Option<&AnyObject>) {
            let on = !settings().diarize;
            update_settings(
                if on {
                    "Listening for who is speaking"
                } else {
                    "No longer listening for who is speaking"
                },
                |s| s.diarize = on,
            );
        }

        #[unsafe(method(openSettingsFile:))]
        fn open_settings_file_action(&self, _sender: Option<&AnyObject>) {
            let path = crate::settings::settings_path();
            std::process::Command::new("/usr/bin/open")
                .arg("-R")
                .arg(&path)
                .spawn()
                .ok();
            announce("Opened the settings file.");
        }

        #[unsafe(method(pickRegion:))]
        fn pick_region_action(&self, _sender: Option<&AnyObject>) {
            pick_region(self.mtm());
            refresh_title(self.mtm());
        }

        /// Repaints the status icon so a hotkey toggle from another thread is
        /// reflected without that thread touching AppKit, and runs a region
        /// pick that a hotkey asked for, since the overlay is main-thread only.
        #[unsafe(method(tick:))]
        fn tick_action(&self, _sender: Option<&AnyObject>) {
            if PICK_REQUESTED.swap(false, Ordering::Relaxed) {
                pick_region(self.mtm());
            }
            refresh_title(self.mtm());
        }

        #[unsafe(method(quit:))]
        fn quit_action(&self, _sender: Option<&AnyObject>) {
            stop_reading();
            announce("Goodbye.");
            std::process::exit(0);
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}

// AppKit objects are not `Send`, so the status item lives in main-thread
// storage rather than a static. The hotkey thread therefore cannot repaint it
// directly; a timer on the main thread picks up the state change instead. The
// icon is a convenience for sighted helpers anyway, since the blind user goes
// by the spoken announcements, which are immediate.
thread_local! {
    static STATUS_ITEM: RefCell<Option<Retained<NSStatusItem>>> = const { RefCell::new(None) };
    /// Kept so the menu can be rebuilt after a setting changes. Like the status
    /// item it is main-thread only, so it lives here rather than in a static.
    static MENU_TARGET: RefCell<Option<Retained<MenuTarget>>> = const { RefCell::new(None) };
}

fn refresh_title(mtm: MainThreadMarker) {
    let on = IS_ON.load(Ordering::Relaxed);
    STATUS_ITEM.with_borrow(|item| {
        if let Some(item) = item.as_ref()
            && let Some(button) = item.button(mtm)
        {
            // Filled while reading, hollow while idle.
            button.setTitle(&NSString::from_str(if on { "◉" } else { "◎" }));
        }
    });
}

/// Build the whole menu from the current settings.
///
/// Rebuilt after every change rather than mutated in place. The menu is small,
/// this runs only on a click, and it removes an entire class of bug where a tick
/// mark disagrees with the setting it is meant to show.
/// How many voices are available for characters, besides the main one.
///
/// For ElevenLabs this is whatever cast the settings file lists. For the
/// on-device voices it is every installed voice for the chosen language that is
/// fit to read dialogue.
fn character_voice_count(s: &Settings) -> usize {
    if s.elevenlabs_voice.is_some() {
        s.elevenlabs_cast.len()
    } else {
        readsubtitles_voice::apple::dialogue_voices(&s.language)
            .len()
            .saturating_sub(1)
    }
}

fn build_menu(mtm: MainThreadMarker, target: &MenuTarget) -> Retained<NSMenu> {
    let s = settings();
    let menu = NSMenu::new(mtm);

    menu.addItem(&menu_item(
        mtm,
        if IS_ON.load(Ordering::Relaxed) {
            "Stop Reading"
        } else {
            "Start Reading"
        },
        sel!(toggle:),
        "s",
        target,
    ));
    menu.addItem(&NSMenuItem::separatorItem(mtm));

    // Source.
    let source_menu = NSMenu::new(mtm);
    let screen = menu_item(mtm, "Read the screen", sel!(useScreen:), "", target);
    let browser = menu_item(
        mtm,
        "Read captions from the browser",
        sel!(useBrowser:),
        "",
        target,
    );
    tick(&screen, s.source == Source::Screen);
    tick(&browser, s.source == Source::Browser);
    source_menu.addItem(&screen);
    source_menu.addItem(&browser);
    menu.addItem(&submenu(mtm, "Source", source_menu));

    // Voice.
    let voice_menu = NSMenu::new(mtm);
    let apple = menu_item(mtm, "Built-in voice", sel!(useAppleVoice:), "", target);
    tick(&apple, s.elevenlabs_voice.is_none());
    voice_menu.addItem(&apple);
    if s.elevenlabs_voice.is_some() || last_elevenlabs_voice().is_some() {
        let eleven = menu_item(mtm, "Eleven Labs voice", sel!(useElevenLabs:), "", target);
        tick(&eleven, s.elevenlabs_voice.is_some());
        voice_menu.addItem(&eleven);
    }
    // Reading characters in their own voices only means something when there
    // are voices to tell apart, so say how many are configured.
    let voices = character_voice_count(&s);
    voice_menu.addItem(&NSMenuItem::separatorItem(mtm));
    let multi = menu_item(
        mtm,
        &if voices > 0 {
            format!("Different voices for characters ({voices} available)")
        } else {
            "Different voices for characters".to_string()
        },
        sel!(toggleMultiVoice:),
        "",
        target,
    );
    tick(&multi, s.multi_voice);
    voice_menu.addItem(&multi);

    let listen = menu_item(
        mtm,
        &format!(
            "Tell voices apart by listening (+{:.1}s delay)",
            s.listen_ahead_ms as f64 / 1000.0
        ),
        sel!(toggleDiarize:),
        "",
        target,
    );
    tick(&listen, s.diarize);
    listen.setEnabled(s.multi_voice);
    voice_menu.addItem(&listen);
    menu.addItem(&submenu(mtm, "Voice", voice_menu));

    // Speaking rate.
    let rate_menu = NSMenu::new(mtm);
    for (rate, label) in RATES {
        let item = menu_item(mtm, label, sel!(setRate:), "", target);
        // Tags are integers, so the rate travels as tenths.
        item.setTag((rate * 10.0).round() as isize);
        tick(&item, (s.rate - rate).abs() < 0.01);
        rate_menu.addItem(&item);
    }
    menu.addItem(&submenu(mtm, "Speed", rate_menu));

    menu.addItem(&NSMenuItem::separatorItem(mtm));
    // Only meaningful for the screen source; the browser source has no region.
    let region = menu_item(
        mtm,
        "Set Subtitle Region...",
        sel!(pickRegion:),
        "r",
        target,
    );
    region.setEnabled(s.source == Source::Screen);
    menu.addItem(&region);
    menu.addItem(&menu_item(
        mtm,
        "Show Settings File...",
        sel!(openSettingsFile:),
        "",
        target,
    ));

    menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu.addItem(&menu_item(mtm, "Quit", sel!(quit:), "q", target));
    menu
}

fn tick(item: &NSMenuItem, on: bool) {
    item.setState(if on {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });
}

fn submenu(mtm: MainThreadMarker, title: &str, child: Retained<NSMenu>) -> Retained<NSMenuItem> {
    let item = NSMenuItem::new(mtm);
    item.setTitle(&NSString::from_str(title));
    item.setSubmenu(Some(&child));
    item
}

/// Swap in a freshly built menu after a setting changes.
fn rebuild_menu(mtm: MainThreadMarker) {
    MENU_TARGET.with_borrow(|target| {
        let Some(target) = target.as_ref() else {
            return;
        };
        let menu = build_menu(mtm, target);
        STATUS_ITEM.with_borrow(|item| {
            if let Some(item) = item.as_ref() {
                item.setMenu(Some(&menu));
            }
        });
    });
}

fn menu_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Sel,
    key: &str,
    target: &MenuTarget,
) -> Retained<NSMenuItem> {
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(key),
        )
    };
    unsafe { item.setTarget(Some(target)) };
    item
}

pub fn run(settings: Settings) -> Result<(), Box<dyn std::error::Error>> {
    *LAST_ELEVENLABS.lock().unwrap() = settings.elevenlabs_voice.clone();
    *SETTINGS.lock().unwrap() = Some(settings);

    let mtm = MainThreadMarker::new().ok_or("menu bar app must run on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    // Accessory: lives in the menu bar with no Dock icon and no main window.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let target = MenuTarget::new(mtm);
    let menu = build_menu(mtm, &target);

    let status_item =
        NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    status_item.setMenu(Some(&menu));
    STATUS_ITEM.with_borrow_mut(|slot| *slot = Some(status_item));
    refresh_title(mtm);

    // Printed so a stale build is obvious from the log rather than only from
    // squinting at the menu. Two silent refactor misses hid behind that.
    let titles: Vec<String> = menu
        .itemArray()
        .iter()
        .map(|i| i.title().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    println!("menu: {}", titles.join(" | "));

    // Keeps the icon honest when the state is changed by a hotkey rather than
    // by the menu.
    unsafe {
        NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
            0.4,
            &target,
            sel!(tick:),
            None,
            true,
        )
    };

    MENU_TARGET.with_borrow_mut(|slot| *slot = Some(target));

    // Global hotkeys. Registering through Carbon means no Accessibility
    // permission is needed, which matters because that is another prompt the
    // blind user cannot see.
    let manager = GlobalHotKeyManager::new()?;
    let toggle = HotKey::new(Some(hotkey_modifiers()), TOGGLE_KEY);
    let region = HotKey::new(Some(hotkey_modifiers()), REGION_KEY);
    let quit = HotKey::new(Some(hotkey_modifiers()), QUIT_KEY);
    manager.register(toggle)?;
    manager.register(region)?;
    manager.register(quit)?;
    let (toggle_id, region_id, quit_id) = (toggle.id(), region.id(), quit.id());

    std::thread::spawn(move || {
        let rx = GlobalHotKeyEvent::receiver();
        for event in rx {
            if event.state != global_hotkey::HotKeyState::Pressed {
                continue;
            }
            if event.id == toggle_id {
                toggle_reading();
            } else if event.id == region_id {
                // Hand off to the main thread; the overlay cannot be built here.
                PICK_REQUESTED.store(true, Ordering::Relaxed);
            } else if event.id == quit_id {
                stop_reading();
                announce("Goodbye.");
                std::process::exit(0);
            }
        }
    });

    println!("readsubtitles is in the menu bar.");
    println!("  Control+Option+Command+S  start / stop");
    println!("  Control+Option+Command+R  set the subtitle region");
    println!("  Control+Option+Command+Q  quit");
    announce("Read subtitles is ready. Press control option command S to start.");

    // Held for the life of the app: dropping the manager unregisters the
    // hotkeys, which would silently leave the user with no way to start.
    std::mem::forget(manager);

    app.run();
    Ok(())
}
