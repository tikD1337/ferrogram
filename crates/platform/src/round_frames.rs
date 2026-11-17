//! Media Engine frame-server output. All D3D/COM work stays on the player MTA.
use std::{
    cell::Cell,
    sync::{Arc, Mutex},
    time::Duration,
};
use windows::{
    Win32::{
        Foundation::{HMODULE, RECT},
        Graphics::{Direct3D::D3D_DRIVER_TYPE_HARDWARE, Direct3D11::*, Dxgi::Common::*},
        Media::MediaFoundation::*,
    },
    core::Interface,
};

/// Preserve cadence across late timer wakes, but skip missed periods after a stall/pause.
pub(crate) fn next_deadline(
    previous: std::time::Instant,
    now: std::time::Instant,
    interval: Duration,
) -> std::time::Instant {
    let next = previous + interval;
    if next > now { next } else { now + interval }
}

pub(crate) struct Frames {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    manager: IMFDXGIDeviceManager,
    textures: Option<(u32, ID3D11Texture2D, ID3D11Texture2D)>,
    pub size: u32,
    pub visible: bool,
    pub ready: Cell<bool>,
    pub interval: Cell<Duration>,
    pub frame: tg_core::RoundFrame,
    timestamp: Option<i64>,
}
impl Frames {
    pub fn new(size: u32, token: u64) -> windows::core::Result<Self> {
        let (mut device, mut context, mut manager) = (None, None, None);
        let mut reset = 0;
        // SAFETY: synchronous creation on the initialized player MTA; local output slots.
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            MFCreateDXGIDeviceManager(&mut reset, &mut manager)?;
        }
        let device = device.ok_or_else(windows::core::Error::empty)?;
        let context = context.ok_or_else(windows::core::Error::empty)?;
        let manager = manager.ok_or_else(windows::core::Error::empty)?;
        // SAFETY: the Media Engine shares this device across its decoder threads.
        unsafe {
            let _ = context
                .cast::<ID3D11Multithread>()?
                .SetMultithreadProtected(true);
        }
        // SAFETY: the manager retains the live D3D11 device for hardware video decoding.
        unsafe {
            manager.ResetDevice(&device, reset)?;
        }
        Ok(Self {
            device,
            context,
            manager,
            textures: None,
            size: size.clamp(1, 2048),
            visible: true,
            ready: Cell::new(false),
            interval: Cell::new(Duration::from_secs_f64(1.0 / 30.0)),
            frame: tg_core::RoundFrame {
                token,
                pixels: Arc::new(Mutex::new(Default::default())),
            },
            timestamp: None,
        })
    }
    pub fn attributes(&self, attributes: &IMFAttributes) -> windows::core::Result<()> {
        // SAFETY: attributes and manager are live COM objects owned on this MTA.
        unsafe {
            attributes.SetUnknown(&MF_MEDIA_ENGINE_DXGI_MANAGER, &self.manager)?;
            attributes.SetUINT32(
                &MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT,
                DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32,
            )
        }
    }
    pub fn set_rate(&self, engine: &IMFMediaEngine) {
        self.ready.set(true);
        // SAFETY: metadata is available; stream attributes are queried on the engine's MTA.
        unsafe {
            if let Ok(ex) = engine.cast::<IMFMediaEngineEx>() {
                for i in 0..ex.GetNumberOfStreams().unwrap_or(0) {
                    if let Ok(value) = ex.GetStreamAttribute(i, &MF_MT_FRAME_RATE) {
                        // VT_UI8 is the packed MF numerator/denominator representation.
                        if value.Anonymous.Anonymous.vt.0 == 21 {
                            let rate = value.Anonymous.Anonymous.Anonymous.uhVal;
                            let num = (rate >> 32) as u32;
                            let den = rate as u32;
                            if num > 0 && den > 0 {
                                self.interval.set(Duration::from_secs_f64(
                                    (f64::from(den) / f64::from(num)).clamp(1.0 / 120.0, 1.0),
                                ));
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
    pub fn tick(&mut self, engine: &IMFMediaEngine) -> windows::core::Result<bool> {
        // SAFETY: engine, textures and immediate context stay on this MTA. Map's pointer is
        // read only until Unmap; rows respect RowPitch and the validated output dimensions.
        unsafe {
            let mut timestamp = 0;
            // The generated wrapper erases S_FALSE. Only S_OK means a new frame exists.
            let status = (Interface::vtable(engine).OnVideoStreamTick)(
                Interface::as_raw(engine),
                &mut timestamp,
            );
            if status.0 == 1 {
                return Ok(false);
            }
            status.ok()?;
            if self.timestamp == Some(timestamp) {
                return Ok(false);
            }
            if self
                .textures
                .as_ref()
                .is_none_or(|(size, _, _)| *size != self.size)
            {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: self.size,
                    Height: self.size,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                    ..Default::default()
                };
                let (mut output, mut staging) = (None, None);
                self.device
                    .CreateTexture2D(&desc, None, Some(&mut output))?;
                let desc = D3D11_TEXTURE2D_DESC {
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    ..desc
                };
                self.device
                    .CreateTexture2D(&desc, None, Some(&mut staging))?;
                self.textures = Some((
                    self.size,
                    output.ok_or_else(windows::core::Error::empty)?,
                    staging.ok_or_else(windows::core::Error::empty)?,
                ));
            }
            let Some((size, output, staging)) = &self.textures else {
                return Ok(false);
            };
            let mut source = MFVideoNormalizedRect {
                left: 0.0,
                top: 0.0,
                right: 1.0,
                bottom: 1.0,
            };
            let (mut w, mut h) = (0, 0);
            engine.GetNativeVideoSize(Some(&mut w), Some(&mut h))?;
            if w > h && h > 0 {
                let crop = (1.0 - h as f32 / w as f32) * 0.5;
                source.left = crop;
                source.right -= crop;
            }
            if h > w && w > 0 {
                let crop = (1.0 - w as f32 / h as f32) * 0.5;
                source.top = crop;
                source.bottom -= crop;
            }
            let rect = RECT {
                left: 0,
                top: 0,
                right: *size as i32,
                bottom: *size as i32,
            };
            engine.TransferVideoFrame(output, Some(&source), &rect, None)?;
            self.context.CopyResource(staging, output);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            if let Ok(mut pixels) = self.frame.pixels.lock() {
                pixels.width = *size;
                pixels.height = *size;
                pixels.sequence += 1;
                pixels.rgba.resize(*size as usize * *size as usize * 4, 0);
                for y in 0..*size as usize {
                    let row = std::slice::from_raw_parts(
                        (mapped.pData as *const u8).add(y * mapped.RowPitch as usize),
                        *size as usize * 4,
                    );
                    for (src, dst) in row.as_chunks::<4>().0.iter().zip(
                        pixels.rgba[y * *size as usize * 4..][..*size as usize * 4]
                            .as_chunks_mut::<4>()
                            .0
                            .iter_mut(),
                    ) {
                        dst.copy_from_slice(&[src[2], src[1], src[0], 255]);
                    }
                }
            }
            self.context.Unmap(staging, 0);
            self.timestamp = Some(timestamp);
            Ok(true)
        }
    }
}

/// Latest output configuration. Animation/scroll events need at most one queued wakeup,
/// leaving the engine event queue available while D3D/Media Engine opens.
#[derive(Default)]
pub(crate) struct Surface(Mutex<Option<(u32, bool)>>);
impl Surface {
    pub fn update(&self, size: u32, visible: bool) -> Result<bool, crate::Error> {
        self.0
            .lock()
            .map(|mut slot| slot.replace((size, visible)).is_none())
            .map_err(|_| crate::Error("round output lock", 0))
    }
    pub fn take(&self) -> Option<(u32, bool)> {
        self.0.lock().ok()?.take()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timer_rounding_does_not_accumulate_and_stalls_do_not_burst() {
        let start = std::time::Instant::now();
        let ms = Duration::from_millis;
        assert_eq!(
            next_deadline(start + ms(10), start + ms(22), ms(16)),
            start + ms(26)
        );
        assert_eq!(
            next_deadline(start + ms(26), start + ms(35), ms(16)),
            start + ms(42)
        );
        assert_eq!(
            next_deadline(start + ms(42), start + ms(100), ms(16)),
            start + ms(116)
        );
    }
    #[test]
    fn a_resize_burst_keeps_only_latest_configuration_and_one_wakeup() {
        let surface = Surface::default();
        let mut wakes = 0;
        for size in 1..4096 {
            wakes += usize::from(surface.update(size, size % 2 == 0).unwrap());
        }
        assert_eq!(wakes, 1);
        assert_eq!(surface.take(), Some((4095, false)));
        assert!(surface.update(320, true).unwrap());
        assert_eq!(surface.take(), Some((320, true)));
        assert_eq!(surface.take(), None);
    }
}
