//! Read-only IMFByteStream adapter. Caller buffers are retained only until their async callback.
use crate::streaming::Reader;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    mpsc,
};
use windows::{
    Win32::{
        Foundation::{E_FAIL, E_INVALIDARG, E_NOTIMPL, E_POINTER},
        Media::MediaFoundation::*,
        System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize},
    },
    core::{AgileReference, IUnknown, Interface, Ref, Result, implement},
};
struct Job {
    buffer: usize,
    length: u32,
    epoch: u64,
    result: AgileReference<IMFAsyncResult>,
}
struct Completion {
    busy: AtomicBool,
    count: AtomicU32,
    identity: AtomicUsize,
}
#[implement(IMFByteStream)]
struct ByteStream {
    reader: Arc<Reader>,
    sender: mpsc::SyncSender<Option<Job>>,
    completion: Arc<Completion>,
}
pub(crate) struct Source {
    pub stream: IMFByteStream,
    reader: Arc<Reader>,
    sender: mpsc::SyncSender<Option<Job>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Source {
    pub fn is_closed(&self) -> bool {
        self.reader.closed()
    }
    pub fn new(reader: Arc<Reader>) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Option<Job>>(1);
        let completion = Arc::new(Completion {
            busy: AtomicBool::new(false),
            count: AtomicU32::new(0),
            identity: AtomicUsize::new(0),
        });
        let (r, c) = (reader.clone(), completion.clone());
        let (ready, started) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("tg-stream-reader".into())
            .spawn(move || {
                // SAFETY: initialize and balance COM on the dedicated callback/reader thread.
                let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
                let _ = ready.send(initialized);
                if !initialized {
                    return;
                }
                while let Ok(Some(job)) = receiver.recv() {
                    // SAFETY: BeginRead's MF caller owns this writable buffer until MFInvokeCallback completes
                    // the request. The queue holds at most one read and the length was checked <= 1 MiB.
                    let result_object = match job.result.resolve() {
                        Ok(result) => result,
                        Err(_) => {
                            r.close();
                            break;
                        }
                    };
                    let result = {
                        // SAFETY: the validated caller buffer remains live until this request
                        // callback. End the mutable slice's lifetime before invoking that callback.
                        let output = unsafe {
                            std::slice::from_raw_parts_mut(
                                job.buffer as *mut u8,
                                job.length as usize,
                            )
                        };
                        r.read_epoch(output, job.epoch)
                    };
                    c.count.store(
                        result.as_ref().copied().unwrap_or(0) as u32,
                        Ordering::Release,
                    );
                    let status = if result.is_ok() {
                        windows::core::HRESULT(0)
                    } else {
                        MF_E_OPERATION_CANCELLED
                    };
                    // SAFETY: owned agile async result; count/status are published before invoking the caller.
                    unsafe {
                        let _ = result_object.SetStatus(status);
                        let _ = MFInvokeCallback(&result_object);
                    }
                    if r.closed() {
                        break;
                    }
                }
                if initialized {
                    // SAFETY: balances successful CoInitializeEx on this same thread.
                    unsafe { CoUninitialize() };
                }
            })
            .map_err(|_| windows::core::Error::from_hresult(E_FAIL))?;
        if started.recv() != Ok(true) {
            let _ = thread.join();
            return Err(E_FAIL.into());
        }
        let stream: IMFByteStream = ByteStream {
            reader: reader.clone(),
            sender: sender.clone(),
            completion,
        }
        .into();
        Ok(Self {
            stream,
            reader,
            sender,
            thread: Some(thread),
        })
    }
}
impl Drop for Source {
    fn drop(&mut self) {
        self.reader.close();
        let _ = self.sender.try_send(None);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
impl ByteStream {
    fn validate(&self, p: *mut u8, count: u32) -> Result<()> {
        if self.reader.closed() {
            return Err(MF_E_SHUTDOWN.into());
        }
        if p.is_null() || count > tg_core::STREAM_PRELOAD as u32 {
            return Err(E_INVALIDARG.into());
        }
        Ok(())
    }
}
impl IMFByteStream_Impl for ByteStream_Impl {
    fn GetCapabilities(&self) -> Result<u32> {
        Ok(MFBYTESTREAM_IS_READABLE | MFBYTESTREAM_IS_SEEKABLE)
    }
    fn GetLength(&self) -> Result<u64> {
        Ok(self.reader.length)
    }
    fn SetLength(&self, _: u64) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn GetCurrentPosition(&self) -> Result<u64> {
        self.reader.position().map_err(|_| E_FAIL.into())
    }
    fn SetCurrentPosition(&self, p: u64) -> Result<()> {
        let p = i64::try_from(p).map_err(|_| windows::core::Error::from_hresult(E_INVALIDARG))?;
        self.reader
            .seek(p, false)
            .map(|_| ())
            .map_err(|_| E_INVALIDARG.into())
    }
    fn IsEndOfStream(&self) -> Result<windows::core::BOOL> {
        Ok((self.GetCurrentPosition()? == self.reader.length).into())
    }
    fn Read(&self, p: *mut u8, count: u32, read: *mut u32) -> Result<()> {
        self.validate(p, count)?;
        if read.is_null() {
            return Err(E_POINTER.into());
        }
        if self.completion.busy.swap(true, Ordering::AcqRel) {
            return Err(MF_E_INVALIDREQUEST.into());
        }
        // SAFETY: IMFByteStream::Read caller provides a writable count-byte buffer and count output
        // for the duration of this synchronous method; both pointers were validated above.
        let result = unsafe {
            let out = std::slice::from_raw_parts_mut(p, count as usize);
            let result = self.reader.read(out);
            *read = result.as_ref().copied().unwrap_or(0) as u32;
            result
        };
        self.completion.busy.store(false, Ordering::Release);
        result
            .map(|_| ())
            .map_err(|_| MF_E_OPERATION_CANCELLED.into())
    }
    fn BeginRead(
        &self,
        p: *mut u8,
        count: u32,
        callback: Ref<IMFAsyncCallback>,
        state: Ref<IUnknown>,
    ) -> Result<()> {
        self.validate(p, count)?;
        let callback = callback.ok()?;
        if self.completion.busy.swap(true, Ordering::AcqRel) {
            return Err(MF_E_INVALIDREQUEST.into());
        }
        let epoch = match self.reader.epoch() {
            Ok(epoch) => epoch,
            Err(_) => {
                self.completion.busy.store(false, Ordering::Release);
                return Err(E_FAIL.into());
            }
        };
        // SAFETY: COM references are borrowed only during result creation; MF retains callback/state.
        let result = match unsafe { MFCreateAsyncResult(None, callback, state.as_ref()) } {
            Ok(r) => r,
            Err(e) => {
                self.completion.busy.store(false, Ordering::Release);
                return Err(e);
            }
        };
        self.completion
            .identity
            .store(result.as_raw() as usize, Ordering::Release);
        let result = match AgileReference::new(&result) {
            Ok(r) => r,
            Err(e) => {
                self.completion.busy.store(false, Ordering::Release);
                return Err(e);
            }
        };
        if self
            .sender
            .try_send(Some(Job {
                buffer: p as usize,
                length: count,
                epoch,
                result,
            }))
            .is_err()
        {
            self.completion.busy.store(false, Ordering::Release);
            return Err(MF_E_SHUTDOWN.into());
        }
        Ok(())
    }
    fn EndRead(&self, result: Ref<IMFAsyncResult>) -> Result<u32> {
        let result = result.ok()?;
        if result.as_raw() as usize != self.completion.identity.load(Ordering::Acquire) {
            return Err(E_INVALIDARG.into());
        }
        // SAFETY: the result is the retained result for this stream's sole outstanding request.
        let status = unsafe { result.GetStatus() };
        let count = self.completion.count.load(Ordering::Acquire);
        self.completion.identity.store(0, Ordering::Release);
        self.completion.busy.store(false, Ordering::Release);
        status?;
        Ok(count)
    }
    fn Write(&self, _: *const u8, _: u32) -> Result<u32> {
        Err(E_NOTIMPL.into())
    }
    fn BeginWrite(
        &self,
        _: *const u8,
        _: u32,
        _: Ref<IMFAsyncCallback>,
        _: Ref<IUnknown>,
    ) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn EndWrite(&self, _: Ref<IMFAsyncResult>) -> Result<u32> {
        Err(E_NOTIMPL.into())
    }
    fn Seek(&self, origin: MFBYTESTREAM_SEEK_ORIGIN, offset: i64, _: u32) -> Result<u64> {
        if origin != msoBegin && origin != msoCurrent {
            return Err(E_INVALIDARG.into());
        }
        self.reader
            .seek(offset, origin == msoCurrent)
            .map_err(|_| E_INVALIDARG.into())
    }
    fn Flush(&self) -> Result<()> {
        Ok(())
    }
    fn Close(&self) -> Result<()> {
        self.reader.close();
        let _ = self.sender.try_send(None);
        Ok(())
    }
}

#[cfg(test)]
mod async_tests {
    use super::*;
    use std::time::{Duration, Instant};
    #[implement(IMFAsyncCallback)]
    struct Callback {
        sender: mpsc::SyncSender<AgileReference<IMFAsyncResult>>,
    }
    impl IMFAsyncCallback_Impl for Callback_Impl {
        fn GetParameters(&self, _: *mut u32, _: *mut u32) -> Result<()> {
            Err(E_NOTIMPL.into())
        }
        fn Invoke(&self, result: Ref<IMFAsyncResult>) -> Result<()> {
            let _ = self.sender.try_send(AgileReference::new(result.ok()?)?);
            Ok(())
        }
    }
    #[test]
    fn begin_read_returns_without_data_and_close_completes_the_callback() {
        // SAFETY: COM and MF are initialized and balanced on this test thread; all interfaces are
        // released before shutdown. The supplied buffer stays live through callback and EndRead.
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap();
            MFStartup(MF_VERSION, MFSTARTUP_FULL).unwrap();
            let reader = Reader::new(
                tg_core::StreamFile {
                    id: 1,
                    size: 1024,
                    path: None,
                    offset: 0,
                    prefix: 0,
                    completed: false,
                },
                1,
                Arc::new(|_| {}),
            );
            let source = Source::new(reader.clone()).unwrap();
            let (tx, rx) = mpsc::sync_channel(1);
            let callback: IMFAsyncCallback = Callback { sender: tx }.into();
            let mut buffer = [0u8; 64];
            let start = Instant::now();
            source
                .stream
                .BeginRead(&mut buffer, &callback, None)
                .unwrap();
            assert!(start.elapsed() < Duration::from_millis(100));
            assert!(rx.recv_timeout(Duration::from_millis(25)).is_err());
            source.stream.Close().unwrap();
            let result = rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .resolve()
                .unwrap();
            assert!(source.stream.EndRead(&result).is_err());
            drop(result);
            drop(callback);
            drop(source);
            MFShutdown().unwrap();
            CoUninitialize();
        }
    }
}
