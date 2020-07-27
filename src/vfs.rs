#![allow(dead_code)]

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
	collections::BTreeMap,
	convert::TryInto,
	fs::File,
	io::{self, BufReader, BufWriter, ErrorKind, Read, Seek, SeekFrom, Write},
	os::windows::fs::FileExt,
	path::{Component, Path, PathBuf},
	time::{Duration, Instant}
};

const BUF_LEN: usize = 1 << 20;

pub struct Vfs {
	path: PathBuf,
	map: BTreeMap<PathBuf, Entry>,
	file: File
}

struct Walker {
	path: PathBuf,
	map: BTreeMap<PathBuf, Entry>,
	size: u64
}

pub struct Reader<'a> {
	file: &'a File,
	offset: u64,
	len: usize,
	index: usize
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Entry {
	Directory,
	File { offset: u64, len: usize }
}

impl Vfs {
	pub fn open(mut path: PathBuf) -> Self {
		let vfs_file = path.join("data.vfs");
		path.push("Data");

		let mut file = File::open(&vfs_file).expect("failed to open VFS");
		let mut map_offset = [0; 8];

		file.read_exact(&mut map_offset)
			.expect("failed to read map offset");

		file.seek(SeekFrom::Start(u64::from_le_bytes(map_offset)))
			.expect("failed to seek to map offset");

		let map = bincode::deserialize_from(BufReader::with_capacity(BUF_LEN, &file))
			.expect("failed to deserialize VFS");

		Self { path, map, file }
	}

	pub fn create(mut path: PathBuf) {
		let vfs_file = path.join("data.vfs");
		path.push("Data");

		let path_m = path
			.metadata()
			.expect("failed to get metadata for data path");

		assert!(path_m.is_dir(), "data path not a dir: {}", path.display());

		let vfs_m = match vfs_file.metadata() {
			Ok(m) => Some(m),
			Err(e) if e.kind() == ErrorKind::NotFound => None,
			Err(e) => return Err(e).expect("failed to get metadata for VFS file")
		};

		if vfs_m.map_or(true, |vfs_m| {
			path_m.modified().unwrap() > vfs_m.modified().unwrap()
		}) {
			println!("creating VFS...");

			let mut walker = Walker {
				path: path.clone(),
				map: BTreeMap::new(),
				size: 0
			};

			walker.walk(&path);
			let entries_len = walker.map.len();

			let mut file = BufWriter::with_capacity(
				BUF_LEN,
				File::create(&vfs_file).expect("failed to create VFS")
			);

			file.seek(SeekFrom::Start(8)).unwrap();

			let mut buf = vec![0; BUF_LEN];
			let mut offset: u64 = 8;
			let mut instant = Instant::now();

			for (i, (p, entry)) in walker.map.iter_mut().enumerate() {
				if let Entry::File {
					offset: e_offset,
					len
				} = entry
				{
					let path = path.join(p);

					let l = copy(
						&mut File::open(&path)
							.expect(&format!("failed to open file: {}", path.display())),
						&mut file,
						&mut buf
					)
					.expect("failed to write to VFS");

					assert_eq!(*len as u64, l);
					*e_offset = offset;
					offset += l;
				}

				let ins = Instant::now();

				if ins.duration_since(instant) >= Duration::from_millis(100) {
					print!(
						"\rcopying files into VFS: {:6}/{:6} {}/{} {:5.1}%",
						i,
						entries_len,
						format_size(offset - 8),
						format_size(walker.size),
						(offset - 8) as f64 / walker.size as f64 * 100.0
					);

					io::stdout().flush().unwrap();
					instant = ins;
				}
			}

			println!("\nfinished copying files into VFS");

			file.seek(SeekFrom::Start(0)).unwrap();
			file.write_all(&offset.to_le_bytes())
				.expect("failed to write VFS map offset");

			file.seek(SeekFrom::End(0)).unwrap();
			bincode::serialize_into(&mut file, &walker.map).expect("failed to serialize VFS map");

			println!("finished creating VFS");
		}
	}

	pub fn inside(&self, path: &Path) -> bool {
		suffix(&self.path, path).is_some()
	}

	pub fn read(&self, path: &Path) -> Option<Option<Reader>> {
		match self.map.get(&suffix(&self.path, path)?) {
			Some(&Entry::File { offset, len }) => {
				Some(Some(Reader {
					file: &self.file,
					offset,
					len,
					index: 0
				}))
			}
			_ => Some(None)
		}
	}

	pub fn find(&self, path: &Path) -> Option<Vec<(&str, &Entry)>> {
		let path = suffix(&self.path, path)?;
		let dir = path.parent().unwrap();

		assert_eq!(self.map.get(dir), Some(&Entry::Directory));

		let file_name = path.file_name().unwrap().to_str().unwrap();
		assert!(!file_name.contains('\\'));
		assert!(file_name.contains('*'));

		let mut pattern = String::new();
		pattern.insert(0, '^');
		pattern.push_str(
			&file_name
				.replace('.', r"\.")
				.replace('?', ".")
				.replace('*', r".*")
		);
		pattern.push('$');

		let pattern = Regex::new(&pattern).unwrap();

		Some(
			self.map
				.range(PathBuf::from(dir)..)
				.take_while(|(k, _)| k.starts_with(dir))
				.filter_map(|(k, v)| k.strip_prefix(dir).ok().map(|s| (s.to_str().unwrap(), v)))
				.filter(|(k, _)| !k.contains('\\') && pattern.is_match(k))
				.map(|(k, v)| (if k.is_empty() { "." } else { k }, v))
				.collect()
		)
	}
}

impl Walker {
	fn walk(&mut self, path: &Path) {
		let m = path
			.symlink_metadata()
			.expect(&format!("failed to get metadata: {}", path.display()));

		let suffix = suffix(&self.path, path).expect(&format!(
			"strip prefix: {} {}",
			path.display(),
			self.path.display()
		));

		if m.is_dir() {
			self.map.insert(suffix, Entry::Directory);

			for entry in path
				.read_dir()
				.expect(&format!("failed to read dir: {}", path.display()))
			{
				let entry = entry.expect(&format!("failed to read dir entry: {}", path.display()));
				self.walk(&entry.path());
			}
		} else if m.is_file() {
			self.size += m.len();

			self.map.insert(
				suffix,
				Entry::File {
					offset: 0,
					len: m.len().try_into().unwrap()
				}
			);
		} else {
			panic!();
		}
	}
}

impl Reader<'_> {
	pub fn len(&self) -> usize {
		self.len
	}
}

impl Read for Reader<'_> {
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
		let len = buf.len().min(self.len - self.index);

		if len == 0 {
			return Ok(0);
		}

		let read = self
			.file
			.seek_read(&mut buf[..len], self.offset + self.index as u64)?;

		self.index += read;
		Ok(read)
	}
}

impl Seek for Reader<'_> {
	fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
		let index = match from {
			SeekFrom::Start(o) => o as i64,
			SeekFrom::Current(o) => self.index as i64 + o,
			SeekFrom::End(o) => self.len() as i64 + o
		};

		if index < 0 {
			Err(ErrorKind::InvalidInput.into())
		} else if index > self.len() as i64 {
			panic!()
		} else {
			self.index = index as usize;
			Ok(self.index as u64)
		}
	}
}

fn suffix(prefix: &Path, path: &Path) -> Option<PathBuf> {
	Some(
		path.strip_prefix(prefix)
			.ok()?
			.components()
			.fold(PathBuf::new(), |mut path, c| {
				match c {
					Component::Normal(s) => path.push(s.to_str().unwrap().to_lowercase()),
					Component::ParentDir => assert!(path.pop(), "{}", path.display()),
					_ => panic!("{}", path.display())
				}

				path
			})
	)
}

fn copy<R, W>(reader: &mut R, writer: &mut W, buf: &mut [u8]) -> io::Result<u64>
where
	R: Read,
	W: Write
{
	let mut written = 0;

	loop {
		let len = match reader.read(buf) {
			Ok(0) => return Ok(written),
			Ok(len) => len,
			Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
			Err(e) => return Err(e)
		};

		writer.write_all(&buf[..len])?;
		written += len as u64;
	}
}

fn format_size(size: u64) -> String {
	match size {
		0..=999 => format!("{:6}B  ", size),
		1000..=1022976 => format!("{:6.1}KiB", size as f64 / 1024.0),
		1022977..=1047527424 => format!("{:6.2}MiB", size as f64 / 1024.0 / 1024.0),
		_ => format!("{:6.2}GiB", size as f64 / 1024.0 / 1024.0 / 1024.0)
	}
}
