#![cfg(windows)]

use lcr_helper::uninstall::{
    cleanup_child_authority, prove_cleanup_parent_exit, CleanupParentHandleValue,
};
use std::{
    env,
    ffi::c_void,
    fs::{self, OpenOptions},
    io::Read as _,
    os::windows::io::{AsRawHandle, IntoRawHandle},
    process::{self, Command, Stdio},
};

type Handle = *mut c_void;

const TRUE: i32 = 1;
const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x0000_1000;
const WAIT_FAILED: u32 = 0xFFFF_FFFF;

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> Handle;
    fn SetHandleInformation(object: Handle, mask: u32, flags: u32) -> i32;
    fn GetHandleInformation(object: Handle, flags: *mut u32) -> i32;
    fn GetProcessId(process: Handle) -> u32;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
}

fn test_root(case: &str) -> std::path::PathBuf {
    let root = env::temp_dir().join(format!("lcr-helper-c3-negative-{case}-{}", process::id()));

    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

fn non_self_process_id() -> u32 {
    let current = process::id();

    if current == u32::MAX {
        current - 1
    } else {
        current + 1
    }
}

#[test]
fn inheritable_non_process_handle_cannot_mint_parent_exit_proof() {
    let root = test_root("non-process");
    let file_path = root.join("non-process.handle");
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file_path)
        .unwrap();
    let raw = file.as_raw_handle() as Handle;

    let inherited = unsafe { SetHandleInformation(raw, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
    assert_ne!(inherited, 0);

    let mut flags = 0u32;
    let queried = unsafe { GetHandleInformation(raw, &mut flags as *mut u32) };
    assert_ne!(queried, 0);
    assert_ne!(flags & HANDLE_FLAG_INHERIT, 0);

    let raw = file.into_raw_handle() as Handle;
    let parent_handle = CleanupParentHandleValue::new(raw as usize).unwrap();
    let expected_parent_process_id = non_self_process_id();
    let temp = root.join("Temp");
    let authority = cleanup_child_authority(&temp, expected_parent_process_id, 0x41).unwrap();

    let result = prove_cleanup_parent_exit(
        &authority.executable_path,
        expected_parent_process_id,
        authority.nonce,
        parent_handle,
    );

    let file_removal = fs::remove_file(&file_path);
    let _ = fs::remove_dir_all(&root);

    assert!(result.is_err());
    assert!(file_removal.is_ok());
}

#[test]
#[ignore = "subprocess target for no-SYNCHRONIZE negative proof"]
fn query_only_target_process() {
    let mut stdin = std::io::stdin().lock();
    let mut sink = Vec::new();

    stdin.read_to_end(&mut sink).unwrap();
}

#[test]
fn query_only_process_handle_cannot_mint_parent_exit_proof() {
    let current_test_executable = env::current_exe().unwrap();
    let mut child = Command::new(&current_test_executable)
        .args([
            "--ignored",
            "--exact",
            "query_only_target_process",
            "--nocapture",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let target_process_id = child.id();

    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, TRUE, target_process_id) };

    if raw.is_null() {
        drop(child.stdin.take());
        let _ = child.wait();
        panic!("failed to open query-only target process HANDLE");
    }

    assert_eq!(unsafe { GetProcessId(raw) }, target_process_id);

    let mut flags = 0u32;
    let queried = unsafe { GetHandleInformation(raw, &mut flags as *mut u32) };
    assert_ne!(queried, 0);
    assert_ne!(flags & HANDLE_FLAG_INHERIT, 0);

    let parent_handle = CleanupParentHandleValue::new(raw as usize).unwrap();
    let root = test_root("no-synchronize");
    let temp = root.join("Temp");
    let authority = cleanup_child_authority(&temp, target_process_id, 0x42).unwrap();

    drop(child.stdin.take());
    let status = child.wait().unwrap();
    assert!(status.success());
    assert_eq!(unsafe { GetProcessId(raw) }, target_process_id);
    assert_eq!(unsafe { WaitForSingleObject(raw, 0) }, WAIT_FAILED);

    let result = prove_cleanup_parent_exit(
        &authority.executable_path,
        target_process_id,
        authority.nonce,
        parent_handle,
    );

    let _ = fs::remove_dir_all(&root);

    assert!(result.is_err());
}
