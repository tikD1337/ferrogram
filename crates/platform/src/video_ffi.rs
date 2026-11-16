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
use windows_sys::Win32::Foundation::{GetLastError, HWND};
use windows_sys::Win32::Graphics::Gdi::{BLACK_BRUSH, GetStockObject, HBRUSH};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, HWND_TOP, RegisterClassW, SWP_NOACTIVATE,
    SetWindowPos, WNDCLASSW, WS_CHILD, WS_CLIPSIBLINGS, WS_DISABLED, WS_VISIBLE,
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
