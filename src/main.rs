#[cfg(not(windows))]
compile_error!("not windows");

use crate::detours::DetourCreateProcessWithDllExW;
use std::{env, io::Error, mem, os::windows::ffi::OsStrExt, path::PathBuf, ptr};
use vfs::Vfs;
use winapi::{
	shared::minwindef::TRUE,
	um::{
		processthreadsapi::{GetExitCodeProcess, ResumeThread, PROCESS_INFORMATION, STARTUPINFOW},
		synchapi::WaitForSingleObject,
		winbase::{CREATE_DEFAULT_ERROR_MODE, CREATE_SUSPENDED, INFINITE}
	}
};

mod detours;
mod vfs;

fn main() {
	let exe = env::var("UNDERRAIL_EXE").map_or_else(
		|_| {
			let mut exe = env::current_exe().expect("failed to get current exe path");

			let s = if exe.file_name().unwrap().to_str().unwrap() == "underrail.exe" {
				"underrail.original.exe"
			} else {
				"underrail.exe"
			};

			exe.set_file_name(s);
			exe
		},
		PathBuf::from
	);

	Vfs::create(exe.parent().unwrap().into());

	let mut exe: Vec<_> = exe.as_os_str().encode_wide().collect();
	exe.push(0);

	unsafe {
		let mut si: STARTUPINFOW = mem::zeroed();
		si.cb = mem::size_of::<STARTUPINFOW>() as _;

		let mut pi: PROCESS_INFORMATION = mem::zeroed();

		let b = DetourCreateProcessWithDllExW(
			exe.as_ptr(),
			ptr::null_mut(),
			ptr::null_mut(),
			ptr::null_mut(),
			TRUE,
			CREATE_DEFAULT_ERROR_MODE | CREATE_SUSPENDED,
			ptr::null_mut(),
			ptr::null_mut(),
			&mut si as *mut _ as *mut _,
			&mut pi as *mut _ as *mut _,
			b"underrail_fix.dll\0".as_ptr() as _,
			None
		);

		assert_eq!(
			b,
			TRUE,
			"DetourCreateProcessWithDllExW: {}",
			Error::last_os_error()
		);

		assert_ne!(
			ResumeThread(pi.hThread),
			!0,
			"ResumeThread: {}",
			Error::last_os_error()
		);

		assert_ne!(
			WaitForSingleObject(pi.hProcess, INFINITE),
			!0,
			"WaitForSingleObject: {}",
			Error::last_os_error()
		);

		let mut exit_code = 0;

		assert_ne!(
			GetExitCodeProcess(pi.hProcess, &mut exit_code),
			0,
			"GetExitCodeProcess: {:#x} {}",
			exit_code,
			Error::last_os_error()
		);
	}
}
