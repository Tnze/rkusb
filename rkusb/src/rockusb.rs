//! Use `rockusb` driver instead of `libusb` as backend

use std::ffi::c_void;

use windows::{
    Win32::{
        Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE},
        Storage::FileSystem::{
            CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile,
            SECURITY_ANONYMOUS, WriteFile,
        },
        System::{IO::DeviceIoControl, WinRT::WindowsConcatString},
    },
    core::{HSTRING, Result, h},
};

pub enum Mode {
    Read = 0,
    Write = 1,
    Raw = 2, // Is this name correct?
}

fn open(path: &HSTRING, mode: Mode) -> Result<HANDLE> {
    let (filename, desired_access, share_mode) = match mode {
        Mode::Read => (h!(r"\PIPE00"), GENERIC_READ, FILE_SHARE_READ),
        Mode::Write => (h!(r"\PIPE01"), GENERIC_WRITE, FILE_SHARE_WRITE),
        Mode::Raw => (
            &HSTRING::new(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
        ),
    };
    unsafe {
        CreateFileW(
            &WindowsConcatString(path, &filename)?,
            desired_access.0,
            share_mode,
            None,
            OPEN_EXISTING,
            SECURITY_ANONYMOUS,
            None,
        )
    }
}

fn read(handle: HANDLE, buffer: &mut [u8]) -> Result<bool> {
    let mut bytes_read = 0;
    unsafe {
        ReadFile(handle, Some(buffer), Some(&raw mut bytes_read), None)?;
    }
    Ok(bytes_read as usize == buffer.len())
}

fn write(handle: HANDLE, buffer: &[u8]) -> Result<bool> {
    let mut bytes_written = 0;
    unsafe {
        WriteFile(handle, Some(buffer), Some(&raw mut bytes_written), None)?;
    }
    Ok(bytes_written as usize == buffer.len())
}

fn close(handle: HANDLE) -> Result<()> {
    unsafe { CloseHandle(handle) }
}

fn reset_pipe(handle: HANDLE, code: u8) -> Result<()> {
    unsafe {
        DeviceIoControl(
            handle,
            0x8000A008,
            Some((&raw const code) as *const c_void),
            1,
            None,
            0,
            None,
            None,
        )
    }
}
