//! User-initiated shell operations; all file I/O and shell calls run on one bounded worker.
use crate::Error;
use std::sync::mpsc;
use tg_core::FileRequest;
pub struct Files {
    sender: Option<mpsc::SyncSender<FileRequest>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl std::fmt::Debug for Files {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Files(<bounded>)")
    }
}
impl Files {
    pub fn start(report: impl Fn(bool) + Send + 'static) -> Result<Self, Error> {
        let (sender, receiver) = mpsc::sync_channel(8);
        let thread = std::thread::Builder::new()
            .name("tg-files".into())
            .spawn(move || crate::files_ffi::run(receiver, report))
            .map_err(|_| Error("file worker startup", 0))?;
        Ok(Self {
            sender: Some(sender),
            thread: Some(thread),
        })
    }
    pub fn request(&self, request: FileRequest) -> Result<(), Error> {
        self.sender
            .as_ref()
            .ok_or(Error("file worker closed", 0))?
            .try_send(request)
            .map_err(|_| Error("file worker busy", 0))
    }
}
impl Drop for Files {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
pub(crate) const INTERNET_ZONE: &[u8] = b"[ZoneTransfer]\r\nZoneId=3\r\n";
pub(crate) fn mark_internet(path: &std::path::Path) -> std::io::Result<()> {
    use std::io::Write;
    if !path.is_absolute() || !path.metadata()?.is_file() {
        return Err(std::io::Error::other("Not a local file"));
    }
    let mut stream = path.as_os_str().to_os_string();
    stream.push(":Zone.Identifier");
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(std::path::PathBuf::from(stream))?
        .write_all(INTERNET_ZONE)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn downloaded_file_gets_exact_internet_zone_before_opening() {
        let path =
            std::env::temp_dir().join(format!("ferrogram-zone-test-{}.txt", std::process::id()));
        std::fs::write(&path, b"synthetic test file").unwrap();
        mark_internet(&path).unwrap();
        let mut stream = path.as_os_str().to_os_string();
        stream.push(":Zone.Identifier");
        assert_eq!(
            std::fs::read(std::path::PathBuf::from(stream)).unwrap(),
            b"[ZoneTransfer]\r\nZoneId=3\r\n"
        );
        std::fs::remove_file(path).unwrap();
    }
}
