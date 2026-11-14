//! On-device text recognition through Apple's Vision framework.
//!
//! Measured on a real subtitle strip (873x289, white on black): `.fast` reads
//! it in 6.6 ms and `.accurate` in 35 ms, both character-for-character correct.
//! `.accurate` is cheap enough at this size that there is no reason to trade
//! accuracy for speed, so it is the default.
//!
//! Two constraints are baked into the shape of this API:
//!
//! - `Retained<VNRecognizeTextRequest>` is neither `Send` nor `Sync`, so a
//!   [`Recognizer`] cannot cross threads. Construct it on the thread that uses
//!   it.
//! - Vision's first call on a machine compiles its model, which took 26-28
//!   seconds here. [`Recognizer::warm_up`] pays that cost up front so it never
//!   lands on the first subtitle of a film.

use std::time::Instant;

use objc2::rc::Retained;
use objc2::{AnyThread, Message};
use objc2_core_foundation::{CFData, CFRetained};
use objc2_core_graphics::{
    CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo,
};
use objc2_foundation::{NSArray, NSDictionary, NSString};
use objc2_vision::{
    VNImageRequestHandler, VNRecognizeTextRequest, VNRecognizedTextObservation, VNRequest,
    VNRequestTextRecognitionLevel,
};

/// Revisions 1 and 2 are deprecated. Pinning 3 keeps behaviour from shifting
/// under a macOS update.
const REQUEST_REVISION: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// ~7 ms on a subtitle strip.
    Fast,
    /// ~35 ms on a subtitle strip. The default.
    Accurate,
}

impl From<Level> for VNRequestTextRecognitionLevel {
    fn from(l: Level) -> Self {
        match l {
            Level::Fast => Self::Fast,
            Level::Accurate => Self::Accurate,
        }
    }
}

/// A reusable text recognizer. Not `Send`; build one per thread.
pub struct Recognizer {
    request: Retained<VNRecognizeTextRequest>,
}

/// One text region Vision found, with where it sat.
#[derive(Debug, Clone)]
pub struct Region {
    pub text: String,
    /// Normalised bounding box in Vision's coordinate space: origin is the
    /// bottom-left of the image, x and y run 0.0 to 1.0.
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Region {
    /// Horizontal centre, 0.0 at the left edge and 1.0 at the right.
    #[must_use]
    pub fn centre_x(&self) -> f64 {
        self.x + self.width / 2.0
    }

    /// Vertical centre measured from the top, so it sorts in reading order.
    #[must_use]
    pub fn centre_from_top(&self) -> f64 {
        1.0 - (self.y + self.height / 2.0)
    }
}

/// What one recognition pass read.
#[derive(Debug, Clone)]
pub struct Reading {
    /// Text regions, sorted into reading order: top to bottom, then left to
    /// right. Vision does not guarantee an order, and a two-line subtitle read
    /// bottom-up is a garbled sentence.
    pub regions: Vec<Region>,
    pub elapsed_ms: f64,
}

impl Reading {
    /// All regions joined in reading order, which is what a two-line subtitle
    /// needs to come out as one sentence.
    #[must_use]
    pub fn joined(&self) -> String {
        self.regions
            .iter()
            .map(|r| r.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }
}

impl Recognizer {
    /// `languages` are BCP-47 tags such as `en-US`.
    ///
    /// Naming them explicitly rather than relying on automatic detection keeps
    /// recognition stable across frames; auto-detection can flip language
    /// between two reads of the same subtitle.
    #[must_use]
    pub fn new(level: Level, languages: &[&str], correction: bool) -> Self {
        let request = VNRecognizeTextRequest::new();
        request.setRecognitionLevel(level.into());
        request.setUsesLanguageCorrection(correction);
        unsafe { request.setRevision(REQUEST_REVISION) };
        let langs: Vec<_> = languages.iter().map(|s| NSString::from_str(s)).collect();
        request.setRecognitionLanguages(&NSArray::from_retained_slice(&langs));
        Self { request }
    }

    /// Run one recognition pass over a BGRA frame.
    ///
    /// Returns `None` only if CoreGraphics refuses the buffer, which means the
    /// dimensions and stride disagree.
    pub fn read(&self, bgra: &[u8], width: usize, height: usize, stride: usize) -> Option<Reading> {
        let image = cg_image_from_bgra(bgra, width, height, stride)?;
        let handler = unsafe {
            VNImageRequestHandler::initWithCGImage_options(
                VNImageRequestHandler::alloc(),
                &image,
                &NSDictionary::new(),
            )
        };

        let requests: Retained<NSArray<VNRequest>> =
            NSArray::from_retained_slice(&[Retained::into_super(Retained::into_super(
                self.request.retain(),
            ))]);

        let t0 = Instant::now();
        handler.performRequests_error(&requests).ok()?;
        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let mut regions = Vec::new();
        if let Some(results) = self.request.results() {
            for obs in results.iter() {
                if let Ok(text) = obs.downcast::<VNRecognizedTextObservation>()
                    && let Some(best) = text.topCandidates(1).firstObject()
                {
                    let bb = unsafe { text.boundingBox() };
                    regions.push(Region {
                        text: best.string().to_string(),
                        x: bb.origin.x,
                        y: bb.origin.y,
                        width: bb.size.width,
                        height: bb.size.height,
                    });
                }
            }
        }
        // Reading order: top to bottom, then left to right.
        regions.sort_by(|a, b| {
            a.centre_from_top()
                .total_cmp(&b.centre_from_top())
                .then(a.x.total_cmp(&b.x))
        });
        Some(Reading {
            regions,
            elapsed_ms,
        })
    }

    /// Force Vision to compile its model now, against a blank frame the size of
    /// the region we will actually read.
    ///
    /// Without this the first real subtitle can stall for roughly half a minute
    /// on a machine that has never run this model.
    pub fn warm_up(&self, width: usize, height: usize) -> f64 {
        let stride = width * 4;
        let blank = vec![0u8; stride * height];
        self.read(&blank, width, height, stride)
            .map_or(0.0, |r| r.elapsed_ms)
    }
}

/// Wrap a BGRA frame as a `CGImage` without reordering bytes.
///
/// ScreenCaptureKit gives us BGRA; `ByteOrder32Little` plus `NoneSkipFirst`
/// tells CoreGraphics to read it as-is, so there is no per-pixel swizzle on the
/// hot path.
fn cg_image_from_bgra(
    bgra: &[u8],
    width: usize,
    height: usize,
    stride: usize,
) -> Option<CFRetained<CGImage>> {
    let needed = height.checked_mul(stride)?;
    if bgra.len() < needed || width * 4 > stride {
        return None;
    }
    let data = CFData::from_bytes(&bgra[..needed]);
    let provider = CGDataProvider::with_cf_data(Some(&data))?;
    let space = CGColorSpace::new_device_rgb()?;
    let info =
        CGBitmapInfo(CGImageByteOrderInfo::Order32Little.0 | CGImageAlphaInfo::NoneSkipFirst.0);
    unsafe {
        CGImage::new(
            width,
            height,
            8,
            32,
            stride,
            Some(&space),
            info,
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_buffer_too_small_for_its_dimensions() {
        assert!(cg_image_from_bgra(&[0; 10], 100, 100, 400).is_none());
    }

    #[test]
    fn rejects_a_stride_narrower_than_the_row() {
        assert!(cg_image_from_bgra(&[0; 4000], 100, 10, 100).is_none());
    }

    #[test]
    fn accepts_a_well_formed_buffer() {
        assert!(cg_image_from_bgra(&[0; 4000], 10, 100, 40).is_some());
    }

    #[test]
    fn reads_white_text_sized_region_without_crashing() {
        // Exercises the full Vision path on a blank frame; the point is that
        // the bridge holds together, not that blank pixels contain words.
        let r = Recognizer::new(Level::Fast, &["en-US"], true);
        let reading = r.read(&vec![0u8; 400 * 100 * 4], 400, 100, 400 * 4);
        assert!(reading.is_some());
        assert!(reading.unwrap().is_empty());
    }
}
