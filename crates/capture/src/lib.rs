//! Watching a rectangle of the screen and emitting only the frames that changed.
//!
//! Two things learned from the Step 1 spike shape this module:
//!
//! 1. ScreenCaptureKit suppresses duplicate frames, so a still screen costs
//!    almost nothing and the configured fps is a ceiling, not a rate.
//! 2. Work done inside the stream's dispatch callback must be trivial. Calling
//!    Vision from it never returns. So the callback does a sparse signature
//!    check and a copy, and everything else happens downstream.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::Instant;

use objc2_core_graphics::{CGDisplayCopyDisplayMode, CGDisplayMode, CGDisplayPixelsWide};
use screencapturekit::cg::CGRect;
use screencapturekit::cv::CVPixelBufferLockFlags;
use screencapturekit::prelude::*;

pub mod audio;

/// A changed frame, copied out of the capture callback.
pub struct Frame {
    pub bgra: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    /// When the change was noticed, for end-to-end lag measurement.
    pub seen_at: Instant,
}

/// What to watch.
#[derive(Debug, Clone)]
pub enum Target {
    /// A display, by index in ScreenCaptureKit's ordering.
    Display(usize),
    /// The first on-screen window whose title contains this (case-insensitive).
    /// More robust than a display index when several screens are in use.
    WindowTitle(String),
}

/// The slice of the target to read, as fractions of its frame.
///
/// Defaults to the bottom fifth, middle 80%, which is where players put
/// subtitles. A sighted helper overrides this once with the region picker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Default for Band {
    fn default() -> Self {
        Self {
            x: 0.10,
            y: 0.78,
            width: 0.80,
            height: 0.22,
        }
    }
}

#[derive(Debug, Default)]
pub struct Stats {
    pub frames: AtomicUsize,
    pub changes: AtomicUsize,
    /// Frames discarded because the consumer was busy. A non-zero value here
    /// means subtitles are arriving faster than they can be read out.
    pub dropped: AtomicUsize,
    pub signature_ns: AtomicU64,
}

/// A running capture. Dropping it stops the stream.
pub struct Capture {
    stream: SCStream,
    pub stats: Arc<Stats>,
    /// Pixel size of the captured region, for warming the recognizer.
    pub size: (u32, u32),
}

impl Capture {
    /// Start watching `target`'s `band`.
    ///
    /// The returned receiver yields only frames whose content changed.
    /// `queue` bounds the backlog; when it is full the oldest pending frame is
    /// dropped rather than queued, because reading out a subtitle that left the
    /// screen seconds ago is worse than missing it.
    pub fn start(
        target: &Target,
        band: Band,
        fps: u32,
        queue: usize,
    ) -> Result<(Self, Receiver<Frame>), Box<dyn std::error::Error>> {
        let content = SCShareableContent::get()?;
        let displays = content.displays();
        let area = resolve_area(&content, target)?;

        // The display containing the area's centre.
        let (cx, cy) = (
            area.origin.x + area.size.width / 2.0,
            area.origin.y + area.size.height / 2.0,
        );
        let display = displays
            .iter()
            .find(|d| {
                let f = d.frame();
                cx >= f.origin.x
                    && cx < f.origin.x + f.size.width
                    && cy >= f.origin.y
                    && cy < f.origin.y + f.size.height
            })
            .or_else(|| displays.first())
            .ok_or("no displays found")?
            .clone();

        let df = display.frame();
        // `source_rect` is relative to the display, so convert from the global
        // coordinate space the window frame is expressed in.
        let rect = CGRect::new(
            area.origin.x - df.origin.x + area.size.width * band.x,
            area.origin.y - df.origin.y + area.size.height * band.y,
            area.size.width * band.width,
            area.size.height * band.height,
        );

        // Points are not pixels on a Retina panel, and OCR quality depends on
        // feeding Vision real pixels.
        let scale = backing_scale(display.display_id(), f64::from(display.width()));
        let size = (
            (rect.size.width * scale) as u32,
            (rect.size.height * scale) as u32,
        );

        let filter = SCContentFilter::create()
            .with_display(&display)
            .with_excluding_windows(&[])
            .build()?;

        let config = SCStreamConfiguration::new()
            .with_source_rect(rect)
            .with_width(size.0)
            .with_height(size.1)
            .with_pixel_format(PixelFormat::BGRA)
            .with_fps(fps)
            .with_queue_depth(3);

        let stats = Arc::new(Stats::default());
        let (tx, rx): (SyncSender<Frame>, Receiver<Frame>) = sync_channel(queue);
        let last_sig = Arc::new(AtomicU64::new(0));

        let (s, l) = (stats.clone(), last_sig.clone());
        let mut stream = SCStream::new(&filter, &config)?;
        stream.add_output_handler(
            move |sample: CMSampleBuffer, kind: SCStreamOutputType| {
                if kind != SCStreamOutputType::Screen {
                    return;
                }
                let Some(pb) = sample.pixel_buffer() else {
                    return;
                };
                let Ok(guard) = pb.lock(CVPixelBufferLockFlags::READ_ONLY) else {
                    return;
                };
                let (w, h, stride) = (guard.width(), guard.height(), guard.bytes_per_row());
                let Some(slice) = (unsafe { guard.as_slice() }) else {
                    return;
                };
                if slice.len() < h * stride {
                    return;
                }

                s.frames.fetch_add(1, Ordering::Relaxed);

                let t = Instant::now();
                let sig = signature(slice, w, h, stride);
                s.signature_ns
                    .fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);

                let prev = l.swap(sig, Ordering::Relaxed);
                if prev == 0 || prev == sig {
                    return;
                }
                s.changes.fetch_add(1, Ordering::Relaxed);

                let frame = Frame {
                    bgra: slice[..h * stride].to_vec(),
                    width: w,
                    height: h,
                    stride,
                    seen_at: Instant::now(),
                };
                if tx.try_send(frame).is_err() {
                    s.dropped.fetch_add(1, Ordering::Relaxed);
                }
            },
            SCStreamOutputType::Screen,
        )?;
        stream.start_capture()?;

        Ok((
            Self {
                stream,
                stats,
                size,
            },
            rx,
        ))
    }

    pub fn stop(&mut self) {
        let _ = self.stream.stop_capture();
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Cheap change signal over a sparse grid of quantised luma values.
///
/// Subtitles are high-contrast, so a sparse sample notices them appearing or
/// changing. Quantising to eight buckets keeps video grain and compression
/// noise from registering as a change. Measured at 0.01-0.04 ms per frame.
fn signature(bgra: &[u8], width: usize, height: usize, stride: usize) -> u64 {
    let mut acc: u64 = 0xcbf2_9ce4_8422_2325;
    let step_y = (height / 48).max(1);
    let step_x = (width / 96).max(1);
    for y in (0..height).step_by(step_y) {
        let row = &bgra[y * stride..];
        for x in (0..width).step_by(step_x) {
            let i = x * 4;
            if i + 2 >= row.len() {
                break;
            }
            let luma = (u16::from(row[i]) + u16::from(row[i + 1]) * 2 + u16::from(row[i + 2])) / 4;
            acc = (acc ^ u64::from(luma >> 5)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    acc
}

/// Print the displays and windows available to watch.
///
/// Mostly a setup aid: with several screens attached, `Target::Display(0)` is
/// rarely the one with the video on it.
/// How many backing pixels a display packs into one point.
///
/// `CGDisplayPixelsWide` is the obvious call and the wrong one: on a scaled
/// mode (the "Looks like 1512x982" settings, which is what most Retina Macs
/// ship on) it reports the *point* width, so dividing by the point width yields
/// 1.0 and the capture is requested at half the resolution the screen actually
/// has. The current display mode's pixel width is the real figure.
///
/// Falls back to the old call, and then to 1.0, rather than failing: capturing
/// at the wrong scale still reads most subtitles, whereas refusing to start
/// reads none.
fn backing_scale(display_id: u32, point_width: f64) -> f64 {
    if point_width <= 0.0 {
        return 1.0;
    }
    let pixels = CGDisplayCopyDisplayMode(display_id)
        .map(|mode| CGDisplayMode::pixel_width(Some(&mode)) as f64)
        .filter(|w| *w > 0.0)
        .unwrap_or_else(|| CGDisplayPixelsWide(display_id) as f64);

    // Clamped because a nonsense ratio would ask the compositor for an absurd
    // buffer. Real displays are 1x or 2x; 3x leaves room without being wild.
    (pixels / point_width).round().clamp(1.0, 3.0)
}

/// A rectangle in the global, top-left-origin space ScreenCaptureKit uses.
///
/// Deliberately a plain struct rather than a `CGRect`, so callers do not have to
/// depend on the capture backend's types just to ask where something is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

fn resolve_area(
    content: &SCShareableContent,
    target: &Target,
) -> Result<CGRect, Box<dyn std::error::Error>> {
    Ok(match target {
        Target::Display(i) => content
            .displays()
            .get(*i)
            .ok_or_else(|| format!("no display at index {i}"))?
            .frame(),
        Target::WindowTitle(sub) => {
            let needle = sub.to_lowercase();
            content
                .windows()
                .into_iter()
                // Skip the small helper and panel windows an app also owns, so
                // a title match lands on the actual video window.
                .filter(|w| w.frame().size.width >= 400.0)
                .find(|w| {
                    w.title()
                        .is_some_and(|t| t.to_lowercase().contains(&needle))
                })
                .ok_or_else(|| format!("no on-screen window matching {sub:?}"))?
                .frame()
        }
    })
}

/// Where the target currently is on screen.
///
/// Used by the region picker to turn a dragged rectangle into fractions of the
/// window, so the saved region keeps pointing at the subtitles after the window
/// is moved or resized.
pub fn target_area(target: &Target) -> Result<Area, Box<dyn std::error::Error>> {
    let content = SCShareableContent::get()?;
    let r = resolve_area(&content, target)?;
    Ok(Area {
        x: r.origin.x,
        y: r.origin.y,
        width: r.size.width,
        height: r.size.height,
    })
}

pub fn print_targets() -> Result<(), Box<dyn std::error::Error>> {
    let content = SCShareableContent::get()?;

    println!("displays:");
    for (i, d) in content.displays().iter().enumerate() {
        let f = d.frame();
        let scale = backing_scale(d.display_id(), f64::from(d.width()));
        println!(
            "  --display {i}   id={} {}x{}pt at ({:.0},{:.0})  {:.0}x backing ({}x{}px)",
            d.display_id(),
            d.width(),
            d.height(),
            f.origin.x,
            f.origin.y,
            scale,
            (f64::from(d.width()) * scale) as u32,
            (f64::from(d.height()) * scale) as u32,
        );
    }

    println!("\nwindows:");
    for w in content.windows() {
        let f = w.frame();
        let Some(title) = w.title().filter(|t| !t.is_empty()) else {
            continue;
        };
        if f.size.width < 400.0 {
            continue;
        }
        let app = w
            .owning_application()
            .map_or_else(|| "?".to_string(), |a| a.application_name());
        println!(
            "  --window {title:?}   [{app}] {:.0}x{:.0}",
            f.size.width, f.size.height
        );
    }
    Ok(())
}
