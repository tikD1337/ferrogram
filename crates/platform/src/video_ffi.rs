//! Media Foundation Media Engine in rendering mode: the engine decodes (on the GPU when the
//! driver offers it), plays the audio and presents the frames into a child window of the main
//! window. No video frame is copied back to the CPU or into the UI toolkit.
use std::{path::Path, sync::mpsc};

use windows::Win32::Foundation::RECT;
use windows::Win32::Media::MediaFoundation::{
    CLSID_MFMediaEngineClassFactory, IMFAttributes, IMFMediaEngine, IMFMediaEngineClassFactory,
    IMFMediaEngineEx, IMFMediaEngineNotify, IMFMediaEngineNotify_Impl, MF_MEDIA_ENGINE_CALLBACK,
    MF_MEDIA_ENGINE_PLAYBACK_HWND, MF_VERSION, MFARGB, MFCreateAttributes, MFSTARTUP_FULL,
    MFShutdown, MFStartup,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::core::{BSTR, Interface, implement};
use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BLACK_BRUSH, GetMonitorInfoW, GetStockObject, HBRUSH, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromWindow,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetWindowLongPtrW, HWND_TOP,
    LWA_ALPHA, RegisterClassW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SetLayeredWindowAttributes,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_LBUTTONUP, WM_NCDESTROY, WM_RBUTTONUP,
    WNDCLASSW, WS_CHILD, WS_CLIPSIBLINGS, WS_DISABLED, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
};

use crate::Error;

/// Messages for the player thread: requests from the UI and engine events.
pub(crate) enum Message {
    Open {
        path: std::path::PathBuf,
        window: isize,
        width: i32,
        height: i32,
    },
    Resize {
        width: i32,
        height: i32,
    },
    TogglePause,
    Seek(f64),
    /// The acknowledgement lets the UI thread destroy the window only after the engine stopped.
    Close(mpsc::SyncSender<()>),
    Event(u32),
    Quit,
}

#[implement(IMFMediaEngineNotify)]
struct Notify {
    sender: mpsc::SyncSender<Message>,
}

impl IMFMediaEngineNotify_Impl for Notify_Impl {
    fn EventNotify(&self, event: u32, _param1: usize, _param2: u32) -> windows::core::Result<()> {
        // Bounded queue: a dropped time update is replaced by the next one.
        let _ = self.sender.try_send(Message::Event(event));
        Ok(())
    }
}

struct Engine {
    engine: IMFMediaEngine,
    extended: Option<IMFMediaEngineEx>,
}

impl Engine {
    fn open(
        notify: &IMFMediaEngineNotify,
        path: &Path,
        window: isize,
    ) -> windows::core::Result<Self> {
        // SAFETY: COM calls on this thread, which initialized COM (MTA) and Media Foundation.
        // `window` is a live child window owned by the UI thread until Close is acknowledged.
        unsafe {
            let factory: IMFMediaEngineClassFactory =
                CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)?;
            let mut attributes: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attributes, 2)?;
            let attributes = attributes.ok_or_else(windows::core::Error::empty)?;
            attributes.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, notify)?;
            attributes.SetUINT64(&MF_MEDIA_ENGINE_PLAYBACK_HWND, window as u64)?;
            let engine = factory.CreateInstance(0, &attributes)?;
            engine.SetSource(&BSTR::from(path.as_os_str().to_string_lossy().as_ref()))?;
            engine.Play()?;
            let extended = engine.cast::<IMFMediaEngineEx>().ok();
            Ok(Self { engine, extended })
        }
    }

    fn resize(&self, width: i32, height: i32) {
        if let Some(extended) = &self.extended {
            let target = RECT {
                left: 0,
                top: 0,
                right: width.max(1),
                bottom: height.max(1),
            };
            let black = MFARGB {
                rgbBlue: 0,
                rgbGreen: 0,
                rgbRed: 0,
                rgbAlpha: 255,
            };
            // SAFETY: both pointers refer to locals that outlive this synchronous call.
            let _ = unsafe { extended.UpdateVideoStream(None, Some(&target), Some(&black)) };
        }
    }

    fn state(&self, event: u32) -> tg_core::VideoState {
        // SAFETY: read-only property calls on the engine owned by this thread.
        let (position, duration, paused, ended) = unsafe {
            (
                self.engine.GetCurrentTime(),
                self.engine.GetDuration(),
                self.engine.IsPaused().as_bool(),
                self.engine.IsEnded().as_bool(),
            )
        };
        let ms = |seconds: f64| {
            if seconds.is_finite() {
                (seconds * 1000.0).clamp(0.0, f64::from(u32::MAX)) as u32
            } else {
                0
            }
        };
        tg_core::VideoState {
            position_ms: ms(position),
            duration_ms: ms(duration),
            playing: !paused && !ended,
            ended,
            failed: event == ERROR_EVENT,
            dismissed: false,
        }
    }

    fn toggle(&self) {
        // SAFETY: playback control on the engine owned by this thread.
        unsafe {
            let _ = if self.engine.IsPaused().as_bool() || self.engine.IsEnded().as_bool() {
                self.engine.Play()
            } else {
                self.engine.Pause()
            };
        }
    }

    fn seek(&self, fraction: f64) {
        // SAFETY: playback control on the engine owned by this thread.
        unsafe {
            let duration = self.engine.GetDuration();
            if duration.is_finite() && duration > 0.0 {
                let _ = self
                    .engine
                    .SetCurrentTime(fraction.clamp(0.0, 1.0) * duration);
            }
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // SAFETY: stops rendering into the child window before the UI thread destroys it.
        let _ = unsafe { self.engine.Shutdown() };
    }
}

// MF_MEDIA_ENGINE_EVENT values used to build states.
const ERROR_EVENT: u32 = 5;
const STATE_EVENTS: [u32; 8] = [5, 9, 10, 13, 14, 18, 19, 21];

/// Player thread body. Blocks on its queue; wakes only for requests and engine events.
pub(crate) fn run(
    receiver: mpsc::Receiver<Message>,
    sender: mpsc::SyncSender<Message>,
    report: Box<dyn Fn(tg_core::VideoState) + Send>,
    ready: mpsc::SyncSender<Result<(), Error>>,
) {
    // SAFETY: per-thread COM and Media Foundation initialization, undone at the end of run.
    let started = unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .and_then(|()| MFStartup(MF_VERSION, MFSTARTUP_FULL))
    };
    if started.is_err() {
        let _ = ready.send(Err(Error("Media Foundation startup", 0)));
        return;
    }
    let _ = ready.send(Ok(()));
    let notify: IMFMediaEngineNotify = Notify { sender }.into();
    let mut engine: Option<Engine> = None;
    let mut size = (1, 1);
    while let Ok(message) = receiver.recv() {
        match message {
            Message::Open {
                path,
                window,
                width,
                height,
            } => {
                engine = None;
                size = (width, height);
                match Engine::open(&notify, &path, window) {
                    Ok(opened) => {
                        opened.resize(width, height);
                        engine = Some(opened);
                    }
                    Err(_) => report(tg_core::VideoState {
                        failed: true,
                        ..Default::default()
                    }),
                }
            }
            Message::Resize { width, height } => {
                size = (width, height);
                if let Some(engine) = &engine {
                    engine.resize(width, height);
                }
            }
            Message::TogglePause => {
                if let Some(engine) = &engine {
                    engine.toggle();
                }
            }
            Message::Seek(fraction) => {
                if let Some(engine) = &engine {
                    engine.seek(fraction);
                }
            }
            Message::Close(done) => {
                engine = None;
                let _ = done.send(());
            }
            Message::Event(event) => {
                if let Some(engine) = &engine
                    && STATE_EVENTS.contains(&event)
                {
                    if event == 10 {
                        // Loaded metadata: the video size is known, fit it into the window.
                        engine.resize(size.0, size.1);
                    }
                    report(engine.state(event));
                }
            }
            Message::Quit => break,
        }
    }
    drop(engine);
    drop(notify);
    // SAFETY: balances MFStartup and CoInitializeEx above on this thread.
    unsafe {
        let _ = MFShutdown();
        CoUninitialize();
    }
}

/// Child window that receives the video. Created, moved and destroyed on the UI thread.
pub(crate) struct ChildWindow(HWND);

impl ChildWindow {
    pub(crate) fn create(parent: isize, rect: tg_core::VideoRect) -> Result<Self, Error> {
        let class: Vec<u16> = "FerrogramVideo\0".encode_utf16().collect();
        // SAFETY: the module handle of this process; null asks for the executable.
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let description = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(DefWindowProcW),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            // SAFETY: stock objects are owned by the system and never freed.
            hbrBackground: unsafe { GetStockObject(BLACK_BRUSH) } as HBRUSH,
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
        };
        // SAFETY: `description` and `class` live across the call; a repeated registration
        // fails harmlessly with "class already exists" and the class stays usable.
        unsafe { RegisterClassW(&description) };
        // SAFETY: `parent` is the live main window of this thread; the class is registered.
        // The window is disabled so mouse input goes to the main window's controls.
        let window = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | WS_DISABLED,
                rect.x,
                rect.y,
                rect.width.max(1),
                rect.height.max(1),
                parent as HWND,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            )
        };
        if window.is_null() {
            // SAFETY: reads this thread's last error right after the failed call.
            return Err(Error("video window", unsafe { GetLastError() }));
        }
        Ok(Self(window))
    }

    pub(crate) fn handle(&self) -> isize {
        self.0 as isize
    }

    pub(crate) fn place(&self, rect: tg_core::VideoRect) {
        // SAFETY: the window is owned by this struct and still alive.
        unsafe {
            SetWindowPos(
                self.0,
                HWND_TOP,
                rect.x,
                rect.y,
                rect.width.max(1),
                rect.height.max(1),
                SWP_NOACTIVATE,
            )
        };
    }
}

impl Drop for ChildWindow {
    fn drop(&mut self) {
        // SAFETY: destroyed once, on the UI thread that created it, after the engine stopped.
        unsafe { DestroyWindow(self.0) };
    }
}

/// Shared state-report callback; the backdrop calls it on the UI thread when it is clicked.
pub(crate) type Report = std::sync::Arc<dyn Fn(tg_core::VideoState) + Send + Sync>;

/// Fullscreen dimmed backdrop behind the viewer, as in Telegram Desktop. A click on it asks the
/// UI to close the viewer. It never takes the focus away from the viewer.
pub(crate) struct Backdrop(HWND);

unsafe extern "system" fn backdrop_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_LBUTTONUP || message == WM_RBUTTONUP {
        // SAFETY: the pointer was stored at creation and stays valid until WM_NCDESTROY.
        let report = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *const Report;
        if !report.is_null() {
            // SAFETY: see above; the closure only posts an event to the UI queue.
            unsafe {
                (*report)(tg_core::VideoState {
                    dismissed: true,
                    ..Default::default()
                })
            };
        }
        return 0;
    }
    if message == WM_NCDESTROY {
        // SAFETY: takes back the box leaked in Backdrop::show exactly once.
        let report = unsafe { SetWindowLongPtrW(window, GWLP_USERDATA, 0) } as *mut Report;
        if !report.is_null() {
            // SAFETY: allocated by Box::into_raw in Backdrop::show and not freed elsewhere.
            drop(unsafe { Box::from_raw(report) });
        }
    }
    // SAFETY: default processing for all other messages of this window.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

impl Backdrop {
    /// Dims the monitor of `viewer`, then centres the viewer on it with the video's aspect ratio.
    pub(crate) fn show(
        viewer: isize,
        width: u32,
        height: u32,
        bar: i32,
        report: Report,
    ) -> Result<Self, Error> {
        let viewer = viewer as HWND;
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: `viewer` is the live viewer window of this thread; `info` is a valid buffer.
        let ok = unsafe {
            GetMonitorInfoW(
                MonitorFromWindow(viewer, MONITOR_DEFAULTTONEAREST),
                &mut info,
            )
        };
        if ok == 0 {
            return Err(Error("monitor", 0));
        }
        let screen = info.rcMonitor;
        let class: Vec<u16> = "FerrogramBackdrop\0".encode_utf16().collect();
        // SAFETY: the module handle of this process.
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let description = WNDCLASSW {
            lpfnWndProc: Some(backdrop_procedure),
            hInstance: instance,
            // SAFETY: stock objects are owned by the system and never freed.
            hbrBackground: unsafe { GetStockObject(BLACK_BRUSH) } as HBRUSH,
            lpszClassName: class.as_ptr(),
            ..Default::default()
        };
        // SAFETY: as for the video class; a repeated registration fails harmlessly.
        unsafe { RegisterClassW(&description) };
        // SAFETY: top-level popup covering the monitor; no parent, the class is registered.
        let window = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class.as_ptr(),
                std::ptr::null(),
                WS_POPUP,
                screen.left,
                screen.top,
                screen.right - screen.left,
                screen.bottom - screen.top,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            )
        };
        if window.is_null() {
            // SAFETY: reads this thread's last error right after the failed call.
            return Err(Error("backdrop window", unsafe { GetLastError() }));
        }
        let report = Box::into_raw(Box::new(report));
        // SAFETY: the window is ours; ownership of the box passes to the window until WM_NCDESTROY.
        unsafe {
            SetWindowLongPtrW(window, GWLP_USERDATA, report as isize);
            // About 70 % black, like Telegram Desktop's media viewer.
            SetLayeredWindowAttributes(window, 0, 178, LWA_ALPHA);
            ShowWindow(window, SW_SHOWNOACTIVATE);
        }
        let backdrop = Self(window);
        // Fit the video into 90 % of the monitor, keeping room for the control bar.
        let (screen_width, screen_height) =
            (screen.right - screen.left, screen.bottom - screen.top);
        let (video_width, video_height) = if width == 0 || height == 0 {
            (16.0, 9.0)
        } else {
            (width as f32, height as f32)
        };
        let scale = (screen_width as f32 * 0.9 / video_width)
            .min((screen_height as f32 * 0.9 - bar as f32).max(1.0) / video_height);
        let window_width = ((video_width * scale) as i32).max(480).min(screen_width);
        let window_height = ((video_height * scale) as i32 + bar).min(screen_height);
        // SAFETY: moves the live viewer above the backdrop, centred on the same monitor.
        unsafe {
            SetWindowPos(
                viewer,
                HWND_TOP,
                screen.left + (screen_width - window_width) / 2,
                screen.top + (screen_height - window_height) / 2,
                window_width,
                window_height,
                0,
            );
        }
        Ok(backdrop)
    }
}

impl Drop for Backdrop {
    fn drop(&mut self) {
        // SAFETY: destroyed once on the UI thread that created it; WM_NCDESTROY frees the box.
        unsafe { DestroyWindow(self.0) };
    }
}
