//! Disposable, event-driven Media Engine audio-only compatibility probe.
mod ffi {
    use std::{path::Path, sync::mpsc, time::{Duration,Instant}};
    use windows::{core::{implement,BSTR},Win32::{Media::MediaFoundation::*,System::Com::*}};
    #[implement(IMFMediaEngineNotify)]
    struct Notify { sender:mpsc::SyncSender<u32> }
    impl IMFMediaEngineNotify_Impl for Notify_Impl {
        fn EventNotify(&self,event:u32,_:usize,_:u32)->windows::core::Result<()> {
            let _=self.sender.try_send(event);Ok(())
        }
    }
    pub fn probe(path:&Path)->Result<(),String> {
        // SAFETY: initialize COM/MF on this probe's sole thread, balance below.
        unsafe {CoInitializeEx(None,COINIT_MULTITHREADED).ok().map_err(|e|format!("CoInitializeEx {:08x}",e.code().0))?;}
        // SAFETY: Media Foundation is started only after COM initialization.
        unsafe {MFStartup(MF_VERSION,MFSTARTUP_FULL).map_err(|e|format!("MFStartup {:08x}",e.code().0))?;}
        let result=run(path);
        // SAFETY: all COM interfaces in run have been released on this thread.
        unsafe {let _=MFShutdown();CoUninitialize();}
        result
    }
    fn run(path:&Path)->Result<(),String> {
        let (send,events)=mpsc::sync_channel(64);
        let notify:IMFMediaEngineNotify=Notify {sender:send}.into();
        // SAFETY: initialized MTA; attributes and callback outlive the engine, no HWND is used.
        let engine=unsafe {
            let factory:IMFMediaEngineClassFactory=CoCreateInstance(&CLSID_MFMediaEngineClassFactory,None,CLSCTX_INPROC_SERVER)
                .map_err(|e|format!("CoCreateInstance {:08x}",e.code().0))?;
            let mut attributes=None;MFCreateAttributes(&mut attributes,1).map_err(|e|format!("MFCreateAttributes {:08x}",e.code().0))?;
            let attributes=attributes.ok_or("Missing attributes")?;
            attributes.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK,&notify).map_err(|e|format!("SetUnknown {:08x}",e.code().0))?;
            let engine=factory.CreateInstance(MF_MEDIA_ENGINE_AUDIOONLY.0 as u32,&attributes)
                .map_err(|e|format!("CreateInstance AUDIOONLY {:08x}",e.code().0))?;
            engine.SetSource(&BSTR::from(path.to_string_lossy().as_ref())).map_err(|e|format!("SetSource {:08x}",e.code().0))?;
            engine.Play().map_err(|e|format!("Play {:08x}",e.code().0))?;engine
        };
        let deadline=Instant::now()+Duration::from_secs(12);
        let mut advanced=false;
        let result=loop {
            let Some(timeout)=deadline.checked_duration_since(Instant::now()) else {break Err("Timed out waiting for Media Engine events".into())};
            let event=match events.recv_timeout(timeout) {Ok(e)=>e,Err(_)=>break Err("Timed out waiting for Media Engine events".into())};
            // SAFETY: read-only engine state; engine is alive and belongs to this initialized thread.
            let position=unsafe {engine.GetCurrentTime()};
            println!("event={event} position_ms={:.0}",position*1000.0);
            advanced|=position>0.05;
            if event==5 {
                // SAFETY: Media Engine error details have fixed numeric fields and reveal no input contents.
                let detail=unsafe {engine.GetError().map(|error|format!("code={} extended={:08x}",error.GetErrorCode(),error.GetExtendedErrorCode().err().map_or(0,|e|e.code().0)))};
                break Err(format!("MediaEngine ERROR {}",detail.unwrap_or_else(|_|"unavailable details".into())));
            }
            if event==19 {break if advanced {Ok(())} else {Err("Ended without advancing audio position".into())};}
        };
        // SAFETY: stop audio/callbacks before the callback interface is released.
        unsafe {let _=engine.Shutdown();}
        result
    }
}
fn main()->std::process::ExitCode {
    let path=std::env::args_os().nth(1).map(std::path::PathBuf::from).unwrap_or_else(||"spikes/opus-probe/tone.ogg".into());
    match ffi::probe(&path) {Ok(())=>{println!("OPUS_PROBE=PLAYED");std::process::ExitCode::SUCCESS},Err(e)=>{eprintln!("OPUS_PROBE=FAILED {e}");std::process::ExitCode::FAILURE}}
}
