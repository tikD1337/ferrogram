//! Disposable probe. `encode <out.mp4>` writes a synthetic 1280x720 30 fps H.264 clip with
//! Media Foundation; `decode <in.mp4> <width> <height> [--paced]` decodes it to RGB32 at the
//! given display size and prints CPU time and peak private memory of this process.
use std::time::{Duration, Instant};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
use windows::core::{HSTRING, Result};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
const FPS: u32 = 30;
const SECONDS: u32 = 10;

fn pack(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

fn video_type(subtype: &windows::core::GUID, width: u32, height: u32) -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(FPS, 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        Ok(t)
    }
}

fn encode(path: &str) -> Result<()> {
    unsafe {
        let writer = MFCreateSinkWriterFromURL(&HSTRING::from(path), None, None)?;
        let output = video_type(&MFVideoFormat_H264, WIDTH, HEIGHT)?;
        output.SetUINT32(&MF_MT_AVG_BITRATE, 4_000_000)?;
        let stream = writer.AddStream(&output)?;
        let input = video_type(&MFVideoFormat_NV12, WIDTH, HEIGHT)?;
        writer.SetInputMediaType(stream, &input, None)?;
        writer.BeginWriting()?;
        let size = (WIDTH * HEIGHT * 3 / 2) as usize;
        let duration = 10_000_000 / i64::from(FPS);
        for frame in 0..FPS * SECONDS {
            let buffer = MFCreateMemoryBuffer(size as u32)?;
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None)?;
            let pixels = std::slice::from_raw_parts_mut(data, size);
            // Moving diagonal gradient in luma, flat chroma: real motion for the encoder.
            // Plus pseudo-random grain, so the bitrate is close to a real 720p clip.
            let mut seed = frame.wrapping_mul(2_654_435_761) | 1;
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    let grain = (seed % 24) as u32;
                    pixels[(y * WIDTH + x) as usize] = ((x + y + frame * 8 + grain) % 256) as u8;
                }
            }
            pixels[(WIDTH * HEIGHT) as usize..].fill(128);
            buffer.Unlock()?;
            buffer.SetCurrentLength(size as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(i64::from(frame) * duration)?;
            sample.SetSampleDuration(duration)?;
            writer.WriteSample(stream, &sample)?;
        }
        writer.Finalize()?;
    }
    Ok(())
}

fn cpu_ms() -> f64 {
    let mut times = [FILETIME::default(); 4];
    let [a, b, c, d] = &mut times;
    unsafe {
        let _ = GetProcessTimes(GetCurrentProcess(), a, b, c, d);
    }
    let ticks = |t: &FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    (ticks(&times[2]) + ticks(&times[3])) as f64 / 10_000.0
}

fn memory_mb() -> (f64, f64) {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
    unsafe {
        let _ = GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters as *mut _ as *mut _,
            size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        );
    }
    let mb = |n: usize| n as f64 / 1024.0 / 1024.0;
    (mb(counters.PrivateUsage), mb(counters.PeakPagefileUsage))
}

/// Hardware decoding: a D3D11 device with video support shared through a DXGI manager.
/// Decode and scaling run on the GPU; only the display-size frame is copied back.
fn gpu_manager() -> Result<IMFDXGIDeviceManager> {
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    use windows::Win32::Graphics::Direct3D10::ID3D10Multithread;
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::core::Interface;
    unsafe {
        let mut device = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            Default::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
        let device = device.ok_or_else(windows::core::Error::empty)?;
        let _ = device.cast::<ID3D10Multithread>()?.SetMultithreadProtected(true);
        let mut token = 0;
        let mut manager = None;
        MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
        let manager = manager.ok_or_else(windows::core::Error::empty)?;
        manager.ResetDevice(&device, token)?;
        Ok(manager)
    }
}

fn decode(path: &str, width: u32, height: u32, paced: bool, gpu: bool) -> Result<()> {
    let (start_private, _) = memory_mb();
    let start_cpu = cpu_ms();
    let started = Instant::now();
    let mut frames = 0_u32;
    let mut checksum = 0_u64;
    unsafe {
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 2)?;
        let attributes = attributes.ok_or_else(windows::core::Error::empty)?;
        // The advanced processor converts to RGB32 and scales to the display size.
        attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
        if gpu {
            attributes.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, &gpu_manager()?)?;
            attributes.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
        }
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path), &attributes)?;
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let output = MFCreateMediaType()?;
        output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        output.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        output.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))?;
        reader.SetCurrentMediaType(stream, None, &output)?;
        // One reusable frame buffer, as the client would upload to its image.
        let mut frame_copy = vec![0_u8; (width * height * 4) as usize];
        loop {
            let mut flags = 0_u32;
            let mut sample = None;
            reader.ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample))?;
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
            let Some(sample) = sample else { continue };
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut data = std::ptr::null_mut();
            let mut length = 0_u32;
            buffer.Lock(&mut data, None, Some(&mut length))?;
            let n = (length as usize).min(frame_copy.len());
            frame_copy[..n].copy_from_slice(std::slice::from_raw_parts(data, n));
            buffer.Unlock()?;
            checksum = checksum.wrapping_add(u64::from(frame_copy[n / 2]));
            frames += 1;
            if paced {
                let due = Duration::from_secs_f64(f64::from(frames) / f64::from(FPS));
                if let Some(wait) = due.checked_sub(started.elapsed()) {
                    std::thread::sleep(wait);
                }
            }
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    let cpu = cpu_ms() - start_cpu;
    let (private, peak) = memory_mb();
    println!(
        "{width}x{height} gpu={gpu} paced={paced}: {frames} frames in {elapsed:.2} s, CPU {cpu:.0} ms = {:.1} % of one core, private start {start_private:.1} MB end {private:.1} MB peak {peak:.1} MB (checksum {checksum})",
        cpu / 10.0 / elapsed
    );
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
    }
    match args.get(1).map(String::as_str) {
        Some("encode") => encode(&args[2])?,
        Some("decode") => {
            // Repeated runs in one process separate a one-time cost from a leak.
            let repeat = if args.iter().any(|a| a == "--twice") { 2 } else { 1 };
            for _ in 0..repeat {
                decode(
                    &args[2],
                    args[3].parse().unwrap_or(640),
                    args[4].parse().unwrap_or(360),
                    args.iter().any(|a| a == "--paced"),
                    args.iter().any(|a| a == "--gpu"),
                )?;
            }
        }
        _ => println!("usage: encode <out.mp4> | decode <in.mp4> <w> <h> [--paced] [--gpu]"),
    }
    Ok(())
}
