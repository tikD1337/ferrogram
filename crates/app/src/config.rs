//! Runtime-only credentials and font loading. Never logs raw input or file contents.
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};
use tg_core::Secret;
pub(crate) struct Credentials {
    pub id: i32,
    pub hash: Secret,
}
/// `.env` in the working directory first, then next to the executable and up to four
/// directories above it, so a build in `target*/release` started by double-click still
/// finds the file in the project root.
fn env_candidates(working: Option<PathBuf>, executable: Option<PathBuf>) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = working.map(|dir| dir.join(".env")).into_iter().collect();
    if let Some(executable) = executable {
        candidates.extend(
            executable
                .ancestors()
                .skip(1)
                .take(5)
                .map(|dir| dir.join(".env")),
        );
    }
    candidates
}
fn open_env() -> std::io::Result<File> {
    let candidates = env_candidates(std::env::current_dir().ok(), std::env::current_exe().ok());
    for path in candidates {
        match File::open(&path) {
            Ok(file) => return Ok(file),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::ErrorKind::NotFound.into())
}
/// Both values or neither: a release built with only one of them falls back to .env.
fn built_credentials() -> (Option<String>, Option<String>) {
    match (
        option_env!("VEYGRAM_BUILD_API_ID"),
        option_env!("VEYGRAM_BUILD_API_HASH"),
    ) {
        (Some(id), Some(hash)) if !id.is_empty() && !hash.is_empty() => {
            (Some(id.to_owned()), Some(hash.to_owned()))
        }
        _ => (None, None),
    }
}
pub(crate) fn credentials() -> Result<Credentials, &'static str> {
    let id = std::env::var("TG_API_ID").ok();
    let hash = std::env::var("TG_API_HASH").ok();
    let mut file_id = None;
    let mut file_hash = None;
    if id.is_none() || hash.is_none() {
        match open_env() {
            Ok(file) => {
                // At most 64 KiB plus one overflow-detection byte. Only two values are retained.
                let mut text = String::new();
                file.take(65537)
                    .read_to_string(&mut text)
                    .map_err(|_| "Could not read application credentials")?;
                if text.len() > 65536 {
                    return Err("Credential file exceeds the size limit");
                }
                for line in text.lines() {
                    let line = line.trim().strip_prefix("export ").unwrap_or(line.trim());
                    let Some((key, value)) = line.split_once('=') else {
                        continue;
                    };
                    let value = value.trim();
                    let value = if value.len() >= 2
                        && ((value.starts_with('"') && value.ends_with('"'))
                            || (value.starts_with('\'') && value.ends_with('\'')))
                    {
                        &value[1..value.len() - 1]
                    } else {
                        value.split(" #").next().unwrap_or("").trim()
                    };
                    match key.trim() {
                        "TG_API_ID" => file_id = Some(value.to_owned()),
                        "TG_API_HASH" => file_hash = Some(value.to_owned()),
                        _ => {}
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("Could not open application credentials"),
        }
    }
    // Public release builds carry the project's own application credentials, set at build
    // time (scripts/package-release.ps1), as every distributed Telegram client does. The
    // environment and .env still take precedence, so developers keep their own.
    let (built_id, built_hash) = built_credentials();
    let id = id
        .or(file_id)
        .or(built_id)
        .and_then(|id| id.parse::<i32>().ok())
        .filter(|id| *id > 0)
        .ok_or("Set TG_API_ID and TG_API_HASH in the environment or .env")?;
    let hash = hash
        .or(file_hash)
        .or(built_hash)
        .filter(|hash| !hash.is_empty() && hash.len() <= 4096)
        .ok_or("Set TG_API_ID and TG_API_HASH in the environment or .env")?;
    Ok(Credentials {
        id,
        hash: Secret::new(hash),
    })
}
/// Preserve ordinary drive/UNC paths for TDLib; do not canonicalize security::data_dir().
/// If the user supplied a verbatim Windows override, remove only the verbatim prefix.
pub(crate) fn plain_directory(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{unc}"))
    } else if let Some(drive) = text.strip_prefix(r"\\?\") {
        PathBuf::from(drive)
    } else {
        path
    }
}
/// The interface scale file: a plain percentage, not secret, next to the TDLib data.
const SCALE_FILE: &str = "interface-scale";
/// The stored interface scale; a missing or unreadable file means auto.
pub(crate) fn interface_scale(directory: &Path) -> u16 {
    let Ok(file) = File::open(directory.join(SCALE_FILE)) else {
        return tg_core::SCALE_AUTO;
    };
    let mut text = String::new();
    if file.take(16).read_to_string(&mut text).is_err() {
        return tg_core::SCALE_AUTO;
    }
    tg_core::parse_scale(&text)
}
/// Stores the interface scale through a temporary file, so a crash never leaves half a value.
pub(crate) fn save_interface_scale(directory: &Path, value: u16) -> std::io::Result<()> {
    let target = directory.join(SCALE_FILE);
    let temporary = directory.join(format!("{SCALE_FILE}.tmp"));
    std::fs::write(&temporary, tg_core::check_scale(value).to_string())?;
    std::fs::rename(&temporary, &target)
}
pub(crate) fn emoji_font(directory: Option<&Path>) -> Result<Option<Vec<u8>>, &'static str> {
    if let Some(path) = std::env::var_os("TG_EMOJI_FONT") {
        return read_font(Path::new(&path)).map(Some);
    }
    if let Some(directory) = directory {
        let entries = match std::fs::read_dir(directory.join("emoji")) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("Could not inspect the emoji font directory"),
        };
        // At most 64 directory entries examined; deterministically choose the first font name.
        let mut selected: Option<PathBuf> = None;
        for entry in entries.take(64) {
            let path = entry.map_err(|_| "Could not inspect an emoji font")?.path();
            let font = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    ["ttf", "otf", "ttc"]
                        .iter()
                        .any(|kind| ext.eq_ignore_ascii_case(kind))
                });
            if font && selected.as_ref().is_none_or(|current| path < *current) {
                selected = Some(path);
            }
        }
        if let Some(path) = selected {
            return read_font(&path).map(Some);
        }
    }
    Ok(None)
}
fn read_font(path: &Path) -> Result<Vec<u8>, &'static str> {
    let file = File::open(path).map_err(|_| "Could not open the emoji font")?;
    let mut bytes = Vec::new();
    file.take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read the emoji font")?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("Emoji font exceeds the 32 MiB limit");
    }
    Ok(bytes)
}
#[cfg(test)]
mod scale_tests {
    use super::*;
    #[test]
    fn interface_scale_round_trips_and_defaults_to_auto() {
        let directory = std::env::temp_dir().join(format!("veygram-scale-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        assert_eq!(interface_scale(&directory), tg_core::SCALE_AUTO);
        save_interface_scale(&directory, 120).unwrap();
        assert_eq!(interface_scale(&directory), 120);
        std::fs::write(directory.join(SCALE_FILE), "garbage that is far too long").unwrap();
        assert_eq!(interface_scale(&directory), tg_core::SCALE_AUTO);
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn env_is_searched_in_the_working_directory_then_above_the_executable() {
        let found = env_candidates(
            Some(PathBuf::from(r"C:\work")),
            Some(PathBuf::from(r"C:\project\target-lead\release\tg-app.exe")),
        );
        assert_eq!(
            found,
            [
                r"C:\work\.env",
                r"C:\project\target-lead\release\.env",
                r"C:\project\target-lead\.env",
                r"C:\project\.env",
                r"C:\.env",
            ]
            .map(PathBuf::from)
        );
    }
    #[test]
    fn tdlib_paths_remain_plain() {
        assert_eq!(
            plain_directory(PathBuf::from(r"E:\sample")),
            PathBuf::from(r"E:\sample")
        );
        assert_eq!(
            plain_directory(PathBuf::from(r"\\?\E:\sample")),
            PathBuf::from(r"E:\sample")
        );
        assert_eq!(
            plain_directory(PathBuf::from(r"\\?\UNC\server\share")),
            PathBuf::from(r"\\server\share")
        );
    }
}
