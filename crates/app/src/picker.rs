//! The drag-a-box region picker.
//!
//! A full-screen dimmed overlay the user drags a rectangle on, the way a screen
//! recorder does. This is a sighted-helper task, done once: the blind user never
//! draws a box, they press the hotkey and listen.
//!
//! The chosen rectangle is stored as fractions of the watched window rather
//! than as screen coordinates, so it keeps pointing at the subtitles when the
//! window is moved, resized, or taken fullscreen.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSBezierPath, NSColor, NSCompositingOperation, NSEvent,
    NSRectFillUsingOperation, NSScreen, NSView, NSWindow, NSWindowCollectionBehavior,
    NSWindowLevel, NSWindowStyleMask,
};
use objc2_core_graphics::CGEvent;
use objc2_foundation::{NSPoint, NSRect, NSSize};

/// Shared between the view and the caller.
#[derive(Default)]
pub struct PickerState {
    start: Cell<NSPoint>,
    current: Cell<NSPoint>,
    dragging: Cell<bool>,
    /// Selection in the overlay's own coordinate space, set on mouse up.
    result: RefCell<Option<NSRect>>,
    done: Cell<bool>,
}

fn rect_between(a: NSPoint, b: NSPoint) -> NSRect {
    NSRect::new(
        NSPoint::new(a.x.min(b.x), a.y.min(b.y)),
        NSSize::new((a.x - b.x).abs(), (a.y - b.y).abs()),
    )
}

define_class!(
    #[unsafe(super(NSView))]
    #[name = "RSPickerView"]
    #[thread_kind = MainThreadOnly]
    #[ivars = Rc<PickerState>]
    struct PickerView;

    impl PickerView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let bounds = self.bounds();
            // Dim everything, then punch out the selection so the user can see
            // exactly which pixels will be read.
            //
            // Kept light on purpose. This overlay is the one moment the app
            // darkens the real screen, and it is drawn over a film other people
            // in the room are still watching, so it has to stay readable
            // underneath rather than black the picture out.
            NSColor::colorWithCalibratedRed_green_blue_alpha(0.0, 0.0, 0.0, 0.25).setFill();
            NSBezierPath::fillRect(bounds);

            let state = self.ivars();
            if !state.dragging.get() {
                return;
            }
            let sel = rect_between(state.start.get(), state.current.get());
            // Copy mode writes transparency through the dimming layer, rather
            // than compositing another colour on top of it.
            NSColor::clearColor().setFill();
            NSRectFillUsingOperation(sel, NSCompositingOperation::Copy);

            NSColor::whiteColor().setStroke();
            let outline = NSBezierPath::bezierPathWithRect(sel);
            outline.setLineWidth(2.0);
            outline.stroke();
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            let state = self.ivars();
            state.start.set(p);
            state.current.set(p);
            state.dragging.set(true);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            self.ivars().current.set(p);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            let state = self.ivars();
            state.current.set(p);
            state.dragging.set(false);
            let sel = rect_between(state.start.get(), p);
            // A stray click is not a selection. Anything this small is almost
            // certainly a misclick, and accepting it would silently leave the
            // app watching a few pixels.
            if sel.size.width >= 20.0 && sel.size.height >= 10.0 {
                *state.result.borrow_mut() = Some(sel);
            }
            state.done.set(true);
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            const ESCAPE: u16 = 53;
            if event.keyCode() == ESCAPE {
                let state = self.ivars();
                *state.result.borrow_mut() = None;
                state.done.set(true);
            }
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }
    }
);

impl PickerView {
    fn new(mtm: MainThreadMarker, frame: NSRect, state: Rc<PickerState>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(state);
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

/// The smallest rectangle containing every screen.
fn screens_union(mtm: MainThreadMarker) -> NSRect {
    let screens = NSScreen::screens(mtm);
    let mut union: Option<NSRect> = None;
    for s in screens.iter() {
        let f = s.frame();
        union = Some(match union {
            None => f,
            Some(u) => {
                let min_x = u.origin.x.min(f.origin.x);
                let min_y = u.origin.y.min(f.origin.y);
                let max_x = (u.origin.x + u.size.width).max(f.origin.x + f.size.width);
                let max_y = (u.origin.y + u.size.height).max(f.origin.y + f.size.height);
                NSRect::new(
                    NSPoint::new(min_x, min_y),
                    NSSize::new(max_x - min_x, max_y - min_y),
                )
            }
        });
    }
    union.unwrap_or_else(|| NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1440.0, 900.0)))
}

/// Show the overlay and return the chosen rectangle in global screen
/// coordinates, or `None` if the user pressed Escape or barely clicked.
///
/// A single window spanning every screen is simpler and better behaved than one
/// per display: the drag can start on one screen and finish on another.
pub fn pick_region(mtm: MainThreadMarker) -> Option<NSRect> {
    let frame = screens_union(mtm);
    let state = Rc::new(PickerState::default());

    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setOpaque(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    // Above normal windows and above full-screen video, so the overlay is
    // usable while the film is playing.
    window.setLevel(NSWindowLevel::from(objc2_app_kit::NSScreenSaverWindowLevel));
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    window.setIgnoresMouseEvents(false);

    let view = PickerView::new(
        mtm,
        NSRect::new(NSPoint::new(0.0, 0.0), frame.size),
        state.clone(),
    );
    window.setContentView(Some(&view));
    window.makeKeyAndOrderFront(None);
    window.makeFirstResponder(Some(&view));

    // A small nested run loop: pump events until the view says it is finished.
    // Simpler than a modal session here, because the overlay is borderless and
    // has no controls of its own.
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    while !state.done.get() {
        // The run-loop mode is an extern static, hence the unsafe.
        let mode = unsafe { objc2_foundation::NSDefaultRunLoopMode };
        let Some(event) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
            objc2_app_kit::NSEventMask::Any,
            None,
            mode,
            true,
        ) else {
            continue;
        };
        app.sendEvent(&event);
    }

    window.orderOut(None);

    // View coordinates are relative to the union frame's origin; callers work
    // in global screen coordinates.
    let selection = (*state.result.borrow())?;
    Some(NSRect::new(
        NSPoint::new(
            frame.origin.x + selection.origin.x,
            frame.origin.y + selection.origin.y,
        ),
        selection.size,
    ))
}

/// Show the picker and return the selection in the top-left-origin coordinate
/// space ScreenCaptureKit uses, ready to compare against `target_area`.
///
/// AppKit measures from the bottom-left of the main screen and ScreenCaptureKit
/// from the top-left, so the y axis has to be flipped across the main screen's
/// height. Getting this wrong is silent: the capture succeeds and watches the
/// wrong strip of the window.
pub fn pick_region_for_capture(mtm: MainThreadMarker) -> Option<readsubtitles_capture::Area> {
    let selection = pick_region(mtm)?;
    Some(flip_to_capture(selection, main_screen_height(mtm)?))
}

/// Height of the screen AppKit measures everything from.
#[must_use]
pub fn main_screen_height(mtm: MainThreadMarker) -> Option<f64> {
    NSScreen::screens(mtm)
        .iter()
        .next()
        .map(|s| s.frame().size.height)
}

/// The flip itself, separated so it can be tested and checked on its own.
#[must_use]
pub fn flip_to_capture(selection: NSRect, main_height: f64) -> readsubtitles_capture::Area {
    readsubtitles_capture::Area {
        x: selection.origin.x,
        y: main_height - (selection.origin.y + selection.size.height),
        width: selection.size.width,
        height: selection.size.height,
    }
}

/// Check that the AppKit-to-ScreenCaptureKit coordinate flip is correct.
///
/// This is the one part of the picker that cannot be covered by a unit test and
/// fails silently when wrong: the capture still succeeds, it just watches the
/// wrong strip of the window.
///
/// The check works by asking two unrelated APIs where the mouse pointer is.
/// AppKit's `mouseLocation` measures from the bottom-left of the main screen
/// and Core Graphics' event location measures from the top-left, so the two
/// must differ by exactly the main screen's height. Nothing here is derived
/// from the flip itself, so agreement is real evidence rather than a tautology.
pub fn verify_coordinate_flip(mtm: MainThreadMarker) -> Result<String, String> {
    let Some(main) = NSScreen::screens(mtm).iter().next().map(|s| s.frame()) else {
        return Err("no screens found".into());
    };
    let main_height = main.size.height;

    let appkit = NSEvent::mouseLocation();
    let event = CGEvent::new(None).ok_or("could not read the pointer position")?;
    let core_graphics = CGEvent::location(Some(&event));

    let expected = main_height - appkit.y;
    let error = (expected - core_graphics.y).abs();

    // Built line by line: a multi-line format string carries its own source
    // indentation into the output.
    let report = [
        format!("main screen {:.0}x{:.0}", main.size.width, main_height),
        format!(
            "pointer, AppKit (bottom-left origin):    ({:.1}, {:.1})",
            appkit.x, appkit.y
        ),
        format!(
            "pointer, CoreGraphics (top-left origin): ({:.1}, {:.1})",
            core_graphics.x, core_graphics.y
        ),
        format!(
            "flip predicts {main_height:.0} - {:.1} = {expected:.1}, actual {:.1}, off by {error:.1}px",
            appkit.y, core_graphics.y
        ),
    ]
    .join("\n");

    // A pixel of slack: the two APIs sample at slightly different moments, so
    // a pointer in motion can legitimately disagree by a hair.
    if error <= 2.0 {
        Ok(report)
    } else {
        Err(report)
    }
}

/// Convert a selection in global screen coordinates into fractions of `area`.
///
/// Clamped to the unit square: a box drawn slightly outside the window should
/// become "the edge", not an out-of-range value that makes the capture fail
/// somewhere far from the cause.
#[must_use]
pub fn band_within(
    selection: readsubtitles_capture::Area,
    area: readsubtitles_capture::Area,
) -> readsubtitles_capture::Band {
    let (w, h) = (area.width.max(1.0), area.height.max(1.0));
    let x = ((selection.x - area.x) / w).clamp(0.0, 1.0);
    let y = ((selection.y - area.y) / h).clamp(0.0, 1.0);
    readsubtitles_capture::Band {
        x,
        y,
        width: (selection.width / w).clamp(0.01, 1.0 - x),
        height: (selection.height / h).clamp(0.01, 1.0 - y),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: f64, y: f64, width: f64, height: f64) -> readsubtitles_capture::Area {
        readsubtitles_capture::Area {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn a_drag_in_any_direction_gives_the_same_rectangle() {
        let a = NSPoint::new(10.0, 20.0);
        let b = NSPoint::new(110.0, 70.0);
        assert_eq!(rect_between(a, b), rect_between(b, a));
        assert_eq!(
            rect_between(b, a),
            NSRect::new(NSPoint::new(10.0, 20.0), NSSize::new(100.0, 50.0))
        );
    }

    #[test]
    fn the_flip_turns_a_bottom_edge_box_into_a_bottom_edge_box() {
        // A 1000-tall screen, a box occupying the lowest 100 points. AppKit
        // calls that y=0; capture coordinates must call it y=900.
        let flipped = flip_to_capture(
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(500.0, 100.0)),
            1000.0,
        );
        assert_eq!(flipped.y, 900.0);
        assert_eq!(flipped.height, 100.0);
    }

    #[test]
    fn the_flip_turns_a_top_edge_box_into_a_top_edge_box() {
        let flipped = flip_to_capture(
            NSRect::new(NSPoint::new(0.0, 900.0), NSSize::new(500.0, 100.0)),
            1000.0,
        );
        assert_eq!(flipped.y, 0.0);
    }

    #[test]
    fn the_flip_leaves_the_horizontal_axis_alone() {
        let flipped = flip_to_capture(
            NSRect::new(NSPoint::new(123.0, 0.0), NSSize::new(456.0, 10.0)),
            1000.0,
        );
        assert_eq!(flipped.x, 123.0);
        assert_eq!(flipped.width, 456.0);
    }

    #[test]
    fn a_selection_becomes_fractions_of_the_window() {
        let band = band_within(r(100.0, 100.0, 800.0, 100.0), r(0.0, 0.0, 1000.0, 1000.0));
        assert!((band.x - 0.1).abs() < 1e-9);
        assert!((band.y - 0.1).abs() < 1e-9);
        assert!((band.width - 0.8).abs() < 1e-9);
        assert!((band.height - 0.1).abs() < 1e-9);
    }

    #[test]
    fn a_window_on_a_second_display_is_handled() {
        // Window sits at x=1728; a selection inside it must not come out
        // negative or beyond 1.
        let band = band_within(
            r(1828.0, 50.0, 800.0, 100.0),
            r(1728.0, 0.0, 1000.0, 1000.0),
        );
        assert!((band.x - 0.1).abs() < 1e-9);
        assert!(band.width > 0.0 && band.x + band.width <= 1.0);
    }

    #[test]
    fn a_selection_outside_the_window_is_clamped_rather_than_inverted() {
        let band = band_within(r(-500.0, -500.0, 300.0, 300.0), r(0.0, 0.0, 1000.0, 1000.0));
        assert_eq!(band.x, 0.0);
        assert_eq!(band.y, 0.0);
        assert!(band.width > 0.0 && band.height > 0.0);
    }

    #[test]
    fn an_oversized_selection_stays_within_the_window() {
        let band = band_within(r(900.0, 900.0, 5000.0, 5000.0), r(0.0, 0.0, 1000.0, 1000.0));
        assert!(band.x + band.width <= 1.0 + 1e-9);
        assert!(band.y + band.height <= 1.0 + 1e-9);
    }

    #[test]
    fn a_zero_sized_window_does_not_divide_by_zero() {
        let band = band_within(r(0.0, 0.0, 10.0, 10.0), r(0.0, 0.0, 0.0, 0.0));
        assert!(band.width.is_finite() && band.height.is_finite());
    }
}
