//! Files opened from Finder: double-click, "Open With", or dropped on the
//! Dock icon. macOS sends these as an "open documents" Apple Event. winit
//! owns the application delegate that would normally receive them, so we
//! install our own handler for that event instead.
//!
//! Also: closing the window hides the app rather than quitting it, as Mac
//! apps do, so it stays in the Dock (with its dot) until ⌘Q.

use std::path::PathBuf;
use std::sync::Mutex;

use eframe::egui;
use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSApplicationWillFinishLaunchingNotification};
use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager, NSNotification, NSNotificationCenter};

/// Apple Event codes: kCoreEventClass 'aevt', kAEOpenDocuments 'odoc',
/// keyDirectObject '----'.
const CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
const OPEN_DOCUMENTS: u32 = u32::from_be_bytes(*b"odoc");
const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

/// Paths waiting for the app; files that launch the app arrive before it exists.
static OPENED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
/// Wakes the app up when files arrive while it is idle.
static WAKER: Mutex<Option<egui::Context>> = Mutex::new(None);

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TextEditorOpenHandler"]
    struct OpenHandler;

    impl OpenHandler {
        /// AppKit installs its own Apple Event handlers before this point, so
        /// ours registered here replaces its "open documents" handler, and is
        /// in place before the file that launched the app is delivered.
        #[unsafe(method(willFinishLaunching:))]
        fn will_finish_launching(&self, _note: &NSNotification) {
            let manager = NSAppleEventManager::sharedAppleEventManager();
            unsafe {
                let _: () = msg_send![
                    &*manager,
                    setEventHandler: self,
                    andSelector: sel!(openDocuments:withReplyEvent:),
                    forEventClass: CORE_EVENT_CLASS,
                    andEventID: OPEN_DOCUMENTS,
                ];
            }
        }

        #[unsafe(method(openDocuments:withReplyEvent:))]
        fn open_documents(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
            let list: Option<Retained<NSAppleEventDescriptor>> =
                unsafe { msg_send![event, paramDescriptorForKeyword: DIRECT_OBJECT] };
            let Some(list) = list else {
                return;
            };
            // Apple Event lists are 1-based.
            let paths = (1..=list.numberOfItems())
                .filter_map(|i| list.descriptorAtIndex(i))
                .filter_map(|item| item.fileURLValue())
                .filter_map(|url| url.to_file_path());
            OPENED.lock().unwrap().extend(paths);
            // Opening a file brings the window back if it was closed.
            NSApplication::sharedApplication(self.mtm()).unhide(None);
            if let Some(ctx) = WAKER.lock().unwrap().as_ref() {
                ctx.request_repaint();
            }
        }
    }
);

/// Call at the start of `main`, before the event loop starts.
pub fn install() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let handler: Retained<OpenHandler> = unsafe { msg_send![OpenHandler::alloc(mtm), init] };
    unsafe {
        NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
            &handler,
            sel!(willFinishLaunching:),
            Some(NSApplicationWillFinishLaunchingNotification),
            None,
        );
    }
    // The notification center and event manager don't keep it alive; it must
    // live as long as the app.
    std::mem::forget(handler);
}

/// Lets file-open events wake the app.
pub fn set_waker(ctx: &egui::Context) {
    *WAKER.lock().unwrap() = Some(ctx.clone());
}

/// Hides the app, as closing its window does on a Mac: it keeps running,
/// and clicking its Dock icon shows the window again. False off the main
/// thread (in tests), where the caller quits instead.
pub fn hide_app() -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    NSApplication::sharedApplication(mtm).hide(None);
    true
}

/// Files opened from Finder since the last call.
pub fn take() -> Vec<PathBuf> {
    std::mem::take(&mut *OPENED.lock().unwrap())
}
