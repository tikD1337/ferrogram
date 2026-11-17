//! Runs in its own process: the hardening applies to the whole test executable.
#![allow(unsafe_code)]
use windows_sys::Win32::{
    Foundation::CloseHandle,
    System::Threading::{GetCurrentProcessId, OpenProcess},
};

const PROCESS_VM_READ: u32 = 0x0010;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const WRITE_DAC: u32 = 0x0004_0000;

#[test]
fn after_hardening_the_process_cannot_be_opened_for_memory_reads() {
    let hardening = tg_platform::harden_process();
    assert_eq!(
        hardening,
        tg_platform::Hardening {
            dll_search: true,
            image_load: true,
            process_access: true,
        }
    );
    // Opening by PID goes through the access check like any other same-user program would.
    // SAFETY: plain handle requests by our own PID; any handle returned is closed.
    unsafe {
        let pid = GetCurrentProcessId();
        for denied in [PROCESS_VM_READ, WRITE_DAC] {
            let handle = OpenProcess(denied, 0, pid);
            if !handle.is_null() {
                CloseHandle(handle);
            }
            assert!(handle.is_null(), "access {denied:#x} must be denied");
        }
        let allowed = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        assert!(
            !allowed.is_null(),
            "Task Manager style queries keep working"
        );
        CloseHandle(allowed);
    }
}
