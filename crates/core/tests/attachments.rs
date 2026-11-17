use tg_core::{decode_waveform, is_executable_name, waveform_bars};
#[test]
fn telegram_five_bit_samples_are_little_endian_and_bounded() {
    assert_eq!(
        decode_waveform(&[0x20, 0x88, 0x41, 0x8a, 0xf9]).unwrap(),
        vec![0, 1, 2, 3, 4, 5, 6, 31]
    );
    assert_eq!(decode_waveform(&[]).unwrap(), Vec::<u8>::new());
    assert_eq!(decode_waveform(&[255; 256]).unwrap().len(), 409);
    assert!(decode_waveform(&[0; 257]).is_err());
    assert_eq!(waveform_bars(&[]), [4; 48]);
    assert_eq!(waveform_bars(&[31; 409]), [31; 48]);
}
#[test]
fn dangerous_extensions_ignore_case_and_respect_double_extensions() {
    for name in [
        "photo.jpg.ExE",
        "installer.MSI",
        "script.PS1",
        "demo.vbs",
        "run.JS",
        "shortcut.lnk",
        "PAYLOAD.CPL ",
        "run.jar.",
        "a.bat",
        "a.hta",
        "a.com",
        "a.reg",
        "a.cmd",
        "a.scr",
        "a.pif",
        "a.jse",
        "a.wsf",
    ] {
        assert!(is_executable_name(name), "{name}");
    }
    for name in [
        "notes.txt",
        "archive.exe.zip",
        "report.pdf",
        "image.webp",
        "",
    ] {
        assert!(!is_executable_name(name), "{name}");
    }
}
