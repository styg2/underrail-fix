#[cfg(not(windows))]
compile_error!("not windows");

use crate::{
	detours::{
		DetourAttach, DetourIsHelperProcess, DetourRestoreAfterWith, DetourTransactionBegin,
		DetourTransactionCommit, DetourUpdateThread
	},
	fixer::Fixer
};
use once_cell::sync::OnceCell;
use std::{
	ffi::{c_void, OsString},
	io::Error,
	os::windows::ffi::OsStringExt,
	path::PathBuf,
	ptr, slice
};
use winapi::{
	shared::minwindef::{BOOL, DWORD, HINSTANCE, LPDWORD, LPVOID, TRUE},
	um::{
		fileapi::{
			CreateFileW, FindClose, FindFirstFileW, FindNextFileW, GetFileSize, GetFileType,
			GetFullPathNameW, ReadFile, SetFilePointer
		},
		handleapi::CloseHandle,
		libloaderapi::GetModuleFileNameW,
		minwinbase::{LPOVERLAPPED, LPSECURITY_ATTRIBUTES, LPWIN32_FIND_DATAW},
		processthreadsapi::GetCurrentThread,
		wincon::AttachConsole,
		winnt::{DLL_PROCESS_ATTACH, HANDLE, LONG, LPCWSTR, LPWSTR, PLONG}
	}
};

mod detours;
mod fixer;
mod vfs;

static FIXER: OnceCell<(Detours, Fixer)> = OnceCell::new();

struct Detour<T> {
	original: Box<T>,
	detoured: T
}

macro_rules! detours {
	($($fn:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {
		paste::item! {
			$(
				type [<$fn Fn>] = unsafe extern "system" fn($($ty),*) -> $ret;

				#[derive(Clone, Copy)]
				pub(crate) struct [<$fn Args>] {
					$($arg: $ty),*
				}
			)*

			struct Detours {
				$([<$fn:snake>]: Detour<[<$fn Fn>]>),*
			}

			impl Detours {
				unsafe fn create() -> Self {
					$(
						let mut [<$fn:snake>] = Detour {
							original: Box::new($fn as [<$fn Fn>]),
							detoured: [<detoured_ $fn:snake>] as [<$fn Fn>]
						};

						let error = DetourAttach(
							[<$fn:snake>].original.as_mut() as *mut [<$fn Fn>] as *mut *mut c_void,
							[<$fn:snake>].detoured as *mut [<$fn Fn>] as *mut c_void
						);

						assert!(error == 0, "DetourAttach {}: {:#x}", stringify!($fn), error);
					)*

					Self { $([<$fn:snake>]),* }
				}
			}

			$(
				unsafe extern "system" fn [<detoured_ $fn:snake>]($(
					#[allow(non_snake_case)] $arg: $ty
				),*) -> $ret {
					let (detours, fixer) = FIXER
						.get()
						.expect("FIXER singleton not initialized");

					let args = [<$fn Args>] { $($arg),* };
					let original: [<$fn Fn>] = *detours.[<$fn:snake>].original;
					fixer.[<$fn:snake>](args, |args| original($(args.$arg),*))
				}
			)*
		}
	};
}

detours! {
	CreateFileW(
		lp_file_name: LPCWSTR,
		dw_desired_access: DWORD,
		dw_share_mode: DWORD,
		lp_security_attributes: LPSECURITY_ATTRIBUTES,
		dw_creation_disposition: DWORD,
		dw_flags_and_attributes: DWORD,
		h_template_file: HANDLE
	) -> HANDLE;

	CloseHandle(h_object: HANDLE) -> BOOL;
	GetFileType(h_file: HANDLE) -> DWORD;
	GetFileSize(h_file: HANDLE, lp_file_size_high: LPDWORD) -> DWORD;

	ReadFile(
		h_file: HANDLE,
		lp_buffer: LPVOID,
		n_number_of_bytes_to_read: DWORD,
		lp_number_of_bytes_read: LPDWORD,
		lp_overlapped: LPOVERLAPPED
	) -> BOOL;

	SetFilePointer(
		h_file: HANDLE,
		l_distance_to_move: LONG,
		lp_distance_to_move_high: PLONG,
		dw_move_method: DWORD
	) -> DWORD;

	GetFullPathNameW(
		lp_file_name: LPCWSTR,
		n_buffer_length: DWORD,
		lp_buffer: LPWSTR,
		lp_file_part: *mut LPWSTR
	) -> DWORD;

	FindFirstFileW(
		lp_file_name: LPCWSTR,
		lp_find_file_data: LPWIN32_FIND_DATAW
	) -> HANDLE;

	FindNextFileW(
		h_find_file: HANDLE,
		lp_find_file_data: LPWIN32_FIND_DATAW
	) -> BOOL;

	FindClose(h_find_file: HANDLE) -> BOOL;
}

#[no_mangle]
unsafe extern "system" fn DllMain(_: HINSTANCE, reason: DWORD, _: LPVOID) -> BOOL {
	if DetourIsHelperProcess() == TRUE {
		return TRUE;
	}

	match reason {
		DLL_PROCESS_ATTACH => {
			assert_ne!(
				AttachConsole(!0),
				0,
				"AttachConsole: {}",
				Error::last_os_error()
			);

			let error = DetourRestoreAfterWith();
			assert_eq!(error, TRUE, "DetourRestoreAfterWith: {:#x}", error);

			let mut path = vec![0; 1 << 10];
			let path = loop {
				let len = GetModuleFileNameW(ptr::null_mut(), path.as_mut_ptr(), path.len() as _);

				if len == 0 {
					panic!("GetModuleFileNameW: {}", Error::last_os_error());
				} else if len >= path.len() as _ {
					path.resize(path.len() * 2, 0);
				} else {
					let mut path = lpcwstr_to_pathbuf(path.as_ptr());
					path.pop();
					break path;
				}
			};

			let fixer = Fixer::new(path);

			let error = DetourTransactionBegin();
			assert_eq!(error, 0, "DetourTransactionBegin: {:#x}", error);

			let error = DetourUpdateThread(GetCurrentThread());
			assert_eq!(error, 0, "DetourUpdateThread: {:#x}", error);

			let detours = Detours::create();

			let error = DetourTransactionCommit();
			assert_eq!(error, 0, "DetourTransactionCommit: {:#x}", error);

			assert!(
				FIXER.set((detours, fixer)).is_ok(),
				"FIXER singleton already initialized"
			);
		}
		_ => {}
	}

	TRUE
}

fn lpcwstr_to_slice<'a>(s: LPCWSTR) -> &'a [u16] {
	assert!(!s.is_null());

	unsafe {
		let mut len = 0;

		for i in 0.. {
			if *s.add(i) == 0 {
				len = i;
				break;
			}
		}

		slice::from_raw_parts(s, len)
	}
}

fn slice_to_pathbuf(s: &[u16]) -> PathBuf {
	PathBuf::from(OsString::from_wide(s))
}

fn lpcwstr_to_pathbuf(s: LPCWSTR) -> PathBuf {
	slice_to_pathbuf(lpcwstr_to_slice(s))
}
