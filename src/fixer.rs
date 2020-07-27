use crate::{
	lpcwstr_to_pathbuf, lpcwstr_to_slice, slice_to_pathbuf,
	vfs::{Entry, Reader, Vfs},
	CloseHandleArgs, CreateFileWArgs, FindCloseArgs, FindFirstFileWArgs, FindNextFileWArgs,
	GetFileSizeArgs, GetFileTypeArgs, GetFullPathNameWArgs, ReadFileArgs, SetFilePointerArgs
};
use parking_lot::Mutex;
use std::{
	fs::File,
	io::{Read, Seek, SeekFrom},
	mem,
	os::windows::io::IntoRawHandle,
	path::PathBuf,
	ptr, slice
};
use winapi::{
	shared::{
		minwindef::{BOOL, DWORD, FALSE, TRUE},
		winerror::{ERROR_FILE_NOT_FOUND, ERROR_NEGATIVE_SEEK, ERROR_NO_MORE_FILES, NO_ERROR}
	},
	um::{
		errhandlingapi::SetLastError,
		fileapi::INVALID_SET_FILE_POINTER,
		handleapi::INVALID_HANDLE_VALUE,
		minwinbase::{LPWIN32_FIND_DATAW, WIN32_FIND_DATAW},
		winbase::{FILE_BEGIN, FILE_CURRENT, FILE_END, FILE_TYPE_DISK},
		winnt::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, GENERIC_READ, HANDLE}
	}
};

pub(crate) struct Fixer {
	vfs: Vfs,
	create: (HANDLE, Mutex<Option<Reader<'static>>>),
	find: (
		HANDLE,
		Mutex<Option<(Vec<(&'static str, &'static Entry)>, usize)>>
	)
}

impl Fixer {
	pub(crate) fn new(path: PathBuf) -> Self {
		Self {
			vfs: Vfs::open(path),
			create: (create_temp_file("create"), Mutex::new(None)),
			find: (create_temp_file("create"), Mutex::new(None))
		}
	}

	pub(crate) fn create_file_w<F>(&self, args: CreateFileWArgs, create_file_w: F) -> HANDLE
	where
		F: Fn(CreateFileWArgs) -> HANDLE
	{
		let path = lpcwstr_to_pathbuf(args.lp_file_name);

		match self.vfs.read(&path) {
			Some(r) => {
				assert_eq!(args.dw_desired_access, GENERIC_READ);

				let mut reader = self.create.1.lock();
				assert!(reader.is_none());

				match r {
					Some(r) => {
						let r = unsafe { mem::transmute::<Reader, Reader<'static>>(r) };
						*reader = Some(r);
						self.create.0
					}
					None => {
						unsafe {
							SetLastError(ERROR_FILE_NOT_FOUND);
						}

						INVALID_HANDLE_VALUE
					}
				}
			}
			None => create_file_w(args)
		}
	}

	pub(crate) fn close_handle<F>(&self, args: CloseHandleArgs, close_handle: F) -> BOOL
	where
		F: Fn(CloseHandleArgs) -> BOOL
	{
		if args.h_object == self.create.0 {
			let mut reader = self.create.1.lock();
			assert!(reader.is_some());
			*reader = None;
			TRUE
		} else {
			close_handle(args)
		}
	}

	pub(crate) fn get_file_type<F>(&self, args: GetFileTypeArgs, get_file_type: F) -> DWORD
	where
		F: Fn(GetFileTypeArgs) -> DWORD
	{
		if args.h_file == self.create.0 {
			assert!(self.create.1.lock().is_some());
			FILE_TYPE_DISK
		} else {
			get_file_type(args)
		}
	}

	pub(crate) fn get_file_size<F>(&self, args: GetFileSizeArgs, get_file_size: F) -> DWORD
	where
		F: Fn(GetFileSizeArgs) -> DWORD
	{
		if args.h_file == self.create.0 {
			let mut reader = self.create.1.lock();
			let reader = reader.as_mut().unwrap();
			let len = reader.len();

			if !args.lp_file_size_high.is_null() {
				unsafe {
					*args.lp_file_size_high = 0;
				}
			}

			len as u32
		} else {
			get_file_size(args)
		}
	}

	pub(crate) fn read_file<F>(&self, args: ReadFileArgs, read_file: F) -> BOOL
	where
		F: Fn(ReadFileArgs) -> BOOL
	{
		if args.h_file == self.create.0 {
			assert!(!args.lp_number_of_bytes_read.is_null());
			assert!(args.lp_overlapped.is_null());

			let mut reader = self.create.1.lock();
			let reader = reader.as_mut().unwrap();

			let buf = unsafe {
				slice::from_raw_parts_mut(
					args.lp_buffer as *mut u8,
					args.n_number_of_bytes_to_read as usize
				)
			};

			match reader.read(buf) {
				Ok(read) => {
					unsafe {
						*args.lp_number_of_bytes_read = read as u32;
					}

					TRUE
				}
				Err(e) => {
					unsafe {
						*args.lp_number_of_bytes_read = 0;
						SetLastError(e.raw_os_error().unwrap() as u32);
					}

					FALSE
				}
			}
		} else {
			read_file(args)
		}
	}

	pub(crate) fn set_file_pointer<F>(&self, args: SetFilePointerArgs, set_file_pointer: F) -> DWORD
	where
		F: Fn(SetFilePointerArgs) -> DWORD
	{
		if args.h_file == self.create.0 {
			let mut reader = self.create.1.lock();
			let reader = reader.as_mut().unwrap();

			let o = unsafe {
				SetLastError(NO_ERROR);

				if args.lp_distance_to_move_high.is_null() {
					args.l_distance_to_move as i64
				} else {
					(args.l_distance_to_move as u64
						| ((*args.lp_distance_to_move_high as u64) << 32)) as i64
				}
			};

			let from = match args.dw_move_method {
				FILE_BEGIN => {
					if o < 0 {
						unsafe {
							SetLastError(ERROR_NEGATIVE_SEEK);
						}

						return INVALID_SET_FILE_POINTER;
					}

					SeekFrom::Start(o as u64)
				}
				FILE_CURRENT => SeekFrom::Current(o),
				FILE_END => SeekFrom::End(o),
				_ => unreachable!("set_file_pointer dw_move_method: {}", args.dw_move_method)
			};

			match reader.seek(from) {
				Ok(pos) => pos as u32,
				Err(_) => {
					unsafe {
						SetLastError(ERROR_NEGATIVE_SEEK);
					}

					INVALID_SET_FILE_POINTER
				}
			}
		} else {
			set_file_pointer(args)
		}
	}

	pub(crate) fn get_full_path_name_w<F>(
		&self,
		args: GetFullPathNameWArgs,
		get_full_path_name_w: F
	) -> DWORD
	where
		F: Fn(GetFullPathNameWArgs) -> DWORD
	{
		let path_slice = lpcwstr_to_slice(args.lp_file_name);
		let path = slice_to_pathbuf(path_slice);

		if self.vfs.inside(&path) {
			assert!(args.lp_file_part.is_null());

			let ret = if (args.n_buffer_length as usize) < path_slice.len() + 1 {
				path_slice.len() + 1
			} else {
				unsafe {
					ptr::copy_nonoverlapping(
						path_slice.as_ptr(),
						args.lp_buffer as *mut u16,
						path_slice.len()
					);
					*args.lp_buffer.add(path_slice.len()) = 0;
				}

				path_slice.len()
			};

			ret as u32
		} else {
			get_full_path_name_w(args)
		}
	}

	pub(crate) fn find_first_file_w<F>(
		&self,
		args: FindFirstFileWArgs,
		find_first_file_w: F
	) -> HANDLE
	where
		F: Fn(FindFirstFileWArgs) -> HANDLE
	{
		let path = lpcwstr_to_pathbuf(args.lp_file_name);

		match self.vfs.find(&path) {
			Some(vec) => {
				let mut find = self.find.1.lock();
				assert!(find.is_none());

				let vec = unsafe {
					mem::transmute::<Vec<(&str, &Entry)>, Vec<(&'static str, &'static Entry)>>(vec)
				};

				*find = Some((vec, 0));
				let (entries, index) = find.as_mut().unwrap();

				if self.find_next_file_impl(args.lp_find_file_data, entries, index) {
					self.find.0
				} else {
					*find = None;

					unsafe {
						SetLastError(ERROR_FILE_NOT_FOUND);
					}

					INVALID_HANDLE_VALUE
				}
			}
			None => find_first_file_w(args)
		}
	}

	pub(crate) fn find_next_file_w<F>(&self, args: FindNextFileWArgs, find_next_file_w: F) -> BOOL
	where
		F: Fn(FindNextFileWArgs) -> BOOL
	{
		if args.h_find_file == self.find.0 {
			let mut find = self.find.1.lock();
			let (entries, index) = find.as_mut().unwrap();

			if self.find_next_file_impl(args.lp_find_file_data, entries, index) {
				TRUE
			} else {
				unsafe {
					SetLastError(ERROR_NO_MORE_FILES);
				}

				FALSE
			}
		} else {
			find_next_file_w(args)
		}
	}

	pub(crate) fn find_close<F>(&self, args: FindCloseArgs, find_close: F) -> BOOL
	where
		F: Fn(FindCloseArgs) -> BOOL
	{
		if args.h_find_file == self.find.0 {
			let mut find = self.find.1.lock();
			assert!(find.is_some());
			*find = None;
			TRUE
		} else {
			find_close(args)
		}
	}

	pub(crate) fn find_next_file_impl(
		&self,
		data: LPWIN32_FIND_DATAW,
		entries: &[(&str, &Entry)],
		index: &mut usize
	) -> bool {
		assert!(!data.is_null());
		let data: &mut WIN32_FIND_DATAW = unsafe { &mut *data };
		*data = unsafe { mem::zeroed() };

		match entries.get(*index) {
			Some((name, &entry)) => {
				*index += 1;

				match entry {
					Entry::Directory => {
						data.dwFileAttributes = FILE_ATTRIBUTE_DIRECTORY;
					}
					Entry::File { len, .. } => {
						data.dwFileAttributes = FILE_ATTRIBUTE_NORMAL;
						data.nFileSizeLow = len as u32;
					}
				}

				for (i, b) in name.bytes().enumerate() {
					data.cFileName[i] = b as u16;
				}

				data.cFileName[name.len()] = 0;
				true
			}
			None => false
		}
	}
}

unsafe impl Send for Fixer {}
unsafe impl Sync for Fixer {}

fn create_temp_file(ty: &str) -> HANDLE {
	File::create(std::env::temp_dir().join(format!("underrail_fixer_{}", ty)))
		.expect("failed to create temp file")
		.into_raw_handle()
}
