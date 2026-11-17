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

impl Player {
    /// `report` runs on the player thread for every state change.
    pub fn start(report: impl Fn(VideoState) + Send + Sync + 'static) -> Result<Self, Error> {
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
                )
            })
            .map_err(|_| Error("video worker startup", 0))?;
        match result.recv() {
            Ok(Ok(())) => Ok(Self {
                sender,
                audio: Cell::new(false),
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
                })
            }
            VideoRequest::OpenAudio { path, token } => {
                self.close();
                queued(Message::OpenAudio { path, token })?;
                self.audio.set(true);
                Ok(())
            }
            VideoRequest::Move(rect) => {
                if let Some(child) = self.window.borrow().as_ref() {
                    child.place(rect);
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
