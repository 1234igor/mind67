//! Trackpad pinch-to-zoom.
//!
//! GPUI 0.2.2's macOS layer translates `NSScrollWheel` and `NSEventTypeSwipe`
//! but never `NSEventTypeMagnify`, so a pinch is dropped before it can reach
//! any element. We watch for it ourselves with a local `NSEvent` monitor.
//!
//! The monitor fires inside AppKit's own dispatch, where re-entering GPUI's
//! context would risk a double borrow — so it only queues, and the view's tick
//! loop applies whatever has arrived.

use std::sync::Mutex;

/// One magnify event, already in GPUI's window coordinate space.
#[derive(Clone, Copy, Debug)]
pub struct Pinch {
    /// AppKit's `magnification`: the fractional size change for this event.
    /// `scale = 1 + magnification`.
    pub magnification: f32,
    /// Pointer position, top-left origin, logical points — same space as
    /// `MouseMoveEvent::position`, so it can be fed straight to `Camera`.
    pub x: f32,
    pub y: f32,
}

static QUEUE: Mutex<Vec<Pinch>> = Mutex::new(Vec::new());
/// Cap so a stalled UI thread cannot grow the queue without bound.
const MAX_QUEUED: usize = 64;

fn push(p: Pinch) {
    if let Ok(mut q) = QUEUE.lock() {
        if q.len() < MAX_QUEUED {
            q.push(p);
        }
    }
}

/// Take everything queued since the last call.
pub fn drain() -> Vec<Pinch> {
    match QUEUE.lock() {
        Ok(mut q) => std::mem::take(&mut *q),
        Err(_) => Vec::new(),
    }
}

/// Cheap check used to decide how soon to wake up again.
pub fn pending() -> bool {
    QUEUE.lock().map(|q| !q.is_empty()).unwrap_or(false)
}

/// Start watching for pinch gestures. Must run on the main thread; calling it
/// more than once is a no-op.
#[cfg(target_os = "macos")]
pub fn install() {
    use std::ptr::NonNull;
    use std::sync::OnceLock;

    use block2::RcBlock;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSEvent, NSEventMask};

    static INSTALLED: OnceLock<()> = OnceLock::new();
    if INSTALLED.set(()).is_err() {
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("mind-map-rust: pinch monitor needs the main thread; skipping");
        return;
    };

    let handler = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit hands us a live NSEvent for the duration of the call.
        let event = unsafe { event.as_ref() };
        let magnification = event.magnification() as f32;
        if magnification != 0.0 && magnification.is_finite() {
            let loc = event.locationInWindow();
            // GPUI measures from the top of the *content* view; AppKit reports
            // from the bottom of the window. Flip to match, exactly as
            // gpui's `convert_mouse_position` does for the mouse.
            let content_h = event
                .window(mtm)
                .and_then(|w| w.contentView())
                .map(|v| v.frame().size.height)
                .unwrap_or(0.0);
            push(Pinch {
                magnification,
                x: loc.x as f32,
                y: (content_h - loc.y) as f32,
            });
        }
        // Pass the event along untouched — we are observing, not consuming.
        (event as *const NSEvent).cast_mut()
    });

    // SAFETY: the handler returns the event pointer AppKit gave us, which is
    // what this API requires. The monitor must outlive the app, so it leaks.
    let monitor =
        unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::Magnify, &handler) };
    std::mem::forget(monitor);
}

#[cfg(not(target_os = "macos"))]
pub fn install() {}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, because the queue is process-global and the harness runs
    /// tests in parallel threads.
    #[test]
    fn queue_hands_events_over_once_and_stays_bounded() {
        let _ = drain();
        assert!(!pending());

        push(Pinch {
            magnification: 0.05,
            x: 10.0,
            y: 20.0,
        });
        assert!(pending());
        let got = drain();
        assert_eq!(got.len(), 1);
        assert!((got[0].magnification - 0.05).abs() < 1e-6);
        assert!(!pending(), "a drained queue is empty");
        assert!(drain().is_empty(), "events are not delivered twice");

        for _ in 0..(MAX_QUEUED + 100) {
            push(Pinch {
                magnification: 0.01,
                x: 0.0,
                y: 0.0,
            });
        }
        assert_eq!(
            drain().len(),
            MAX_QUEUED,
            "a stalled UI must not grow the queue forever"
        );
    }
}
