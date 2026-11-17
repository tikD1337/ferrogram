//! Video player: one video at a time, created only while the player is open.
use std::{
    cell::{Cell, RefCell},
    sync::mpsc,
    time::Duration,
};

use tg_core::{VideoRequest, VideoState};

use crate::Error;
use crate::video_ffi::{Backdrop, ChildWindow, Message, Report, run};

/// Owned by the UI thread: requests arrive from UI callbacks, which also own the child window.
pub struct Player {
    sender: mpsc::SyncSender<Message>,
    audio: Cell<bool>,
    surface: std::sync::Arc<crate::round_frames::Surface>,
    stream: RefCell<Option<std::sync::Arc<crate::streaming::Reader>>>,
    window: RefCell<Option<ChildWindow>>,
    backdrop: RefCell<Option<Backdrop>>,
    report: Report,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Player(<redacted>)")
    }
}

/// Whether the player's video window under the main window `parent` is visible with a
/// non-empty clip region; `None` when no video window exists. For reference checks only.
pub fn video_window_shown(parent: isize) -> Option<bool> {
    crate::video_ffi::video_window_shown(parent)
}

impl Player {
    /// `report` runs on the player thread for every state change.
    pub fn start(report: impl Fn(VideoState) + Send + Sync + 'static) -> Result<Self, Error> {
        Self::start_with_frames(report, |_| {})
    }

    /// Reports latest round frames separately from clock/state events.
    pub fn start_with_frames(
        report: impl Fn(VideoState) + Send + Sync + 'static,
        frames: impl Fn(tg_core::RoundFrame) + Send + 'static,
    ) -> Result<Self, Error> {
        let report: Report = std::sync::Arc::new(report);
        let worker = report.clone();
        // At most 64 queued requests and engine events; time updates beyond that are dropped.
        let (sender, receiver) = mpsc::sync_channel(64);
        let (ready, result) = mpsc::sync_channel(1);
        let events = sender.clone();
        let thread = std::thread::Builder::new()
            .name("tg-video".into())
            .spawn(move || {
                run(
                    receiver,
                    events,
                    Box::new(move |state| worker(state)),
                    ready,
                    Box::new(frames),
                )
            })
            .map_err(|_| Error("video worker startup", 0))?;
        match result.recv() {
            Ok(Ok(())) => Ok(Self {
                sender,
                audio: Cell::new(false),
                surface: Default::default(),
                stream: RefCell::new(None),
                window: RefCell::new(None),
                backdrop: RefCell::new(None),
                report,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                let _ = thread.join();
                Err(Error("video worker startup", 0))
            }
        }
    }

    /// Call on the UI thread that owns the main window.
    pub fn request(&self, request: VideoRequest) -> Result<(), Error> {
        let queued = |message| {
            self.sender
                .try_send(message)
                .map_err(|_| Error("video queue", 0))
        };
        match request {
            VideoRequest::Availability { file, token } => {
                if let Some(stream) = self.stream.borrow().as_ref()
                    && stream.file == file.id
                    && stream.token == token
                {
                    stream.update(file);
                }
                Ok(())
            }
            VideoRequest::OpenStream {
                file,
                window,
                rect,
                token,
            } => {
                self.close();
                let child = ChildWindow::create(window, rect)?;
                let handle = child.handle();
                *self.window.borrow_mut() = Some(child);
                let report = self.report.clone();
                let reader = crate::streaming::Reader::new(
                    file,
                    token,
                    std::sync::Arc::new(move |s| {
                        report(VideoState {
                            stream: Some(s),
                            ..Default::default()
                        });
                    }),
                );
                *self.stream.borrow_mut() = Some(reader.clone());
                queued(Message::OpenStream {
                    reader,
                    window: handle,
                    width: rect.width,
                    height: rect.height,
                })
            }
            VideoRequest::Open { path, window, rect } => {
                self.close();
                let child = ChildWindow::create(window, rect)?;
                let handle = child.handle();
                *self.window.borrow_mut() = Some(child);
                queued(Message::Open {
                    path,
                    window: handle,
                    width: rect.width,
                    height: rect.height,
                    token: None,
                })
            }
            VideoRequest::OpenRound { path, size, token } => {
                self.close();
                queued(Message::OpenRound { path, size, token })?;
                self.audio.set(true);
                Ok(())
            }
            VideoRequest::RoundSurface { size, visible } => {
                if self.surface.update(size, visible)?
                    && let Err(error) = queued(Message::RoundSurface(self.surface.clone()))
                {
                    self.surface.take();
                    return Err(error);
                }
                Ok(())
            }
            VideoRequest::OpenAudio { path, token } => {
                self.close();
                queued(Message::OpenAudio { path, token })?;
                self.audio.set(true);
                Ok(())
            }
            VideoRequest::Move(rect) => {
                if let Some(child) = self.window.borrow().as_ref() {
                    child.place(rect)?;
                }
                queued(Message::Resize {
                    width: rect.width,
                    height: rect.height,
                })
            }
            VideoRequest::Present {
                viewer,
                width,
                height,
                bar,
            } => {
                *self.backdrop.borrow_mut() = None;
                let backdrop = Backdrop::show(viewer, width, height, bar, self.report.clone())?;
                *self.backdrop.borrow_mut() = Some(backdrop);
                Ok(())
            }
            VideoRequest::TogglePause => queued(Message::TogglePause),
            VideoRequest::Seek(fraction) => queued(Message::Seek(fraction)),
            VideoRequest::Close => {
                self.close();
                *self.backdrop.borrow_mut() = None;
                Ok(())
            }
        }
    }

    fn close(&self) {
        if let Some(stream) = self.stream.borrow_mut().take() {
            stream.close();
        }
        let child = self.window.borrow_mut().take();
        if !self.audio.replace(false) && child.is_none() {
            return;
        }
        let (done, stopped) = mpsc::sync_channel(1);
        if self.sender.send(Message::Close(done)).is_ok() {
            // The engine must stop rendering before its window disappears; Shutdown is quick,
            // the bound only protects the UI thread from a stuck driver.
            let _ = stopped.recv_timeout(Duration::from_secs(2));
        }
        drop(child);
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.close();
        let _ = self.sender.send(Message::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
