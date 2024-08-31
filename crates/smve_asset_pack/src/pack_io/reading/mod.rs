//! Blocking API for reading asset pack files
//!
//! If you are using this in a async context, use the API under [`async_read`] instead.

#[cfg(feature = "async_read")]
pub mod async_read;
mod errors;
mod file_reader;
pub mod flags;
mod iter_dir;
pub mod pack_group;
mod read_steps;
mod utils;

use cfg_if::cfg_if;
pub use errors::*;
pub use file_reader::*;
pub use iter_dir::*;
use read_steps::validate_header;
use utils::{io, read_bytes};

use crate::pack_io::reading::read_steps::{
    get_dir_start_indices, read_dl, read_toc, validate_files, validate_version,
};
use indexmap::IndexMap;
use std::collections::HashMap;
use std::fmt::Debug;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek};
use std::path::Path;

/// Create an instance of this struct to read an asset pack.
///
/// # Examples
/// * Read the pack front from the asset pack:
/// ```no_run
/// use smve_asset_pack::pack_io::reading::AssetPackReader;
///
/// # fn foo() -> smve_asset_pack::pack_io::reading::ReadResult<()> {
/// let mut pack_reader = AssetPackReader::new_from_path("./path/to/pack.smap")?;
/// let pack_front = pack_reader.get_pack_front();
/// let toc = &pack_front.toc;
/// let directories = &pack_front.directory_list;
/// # Ok(()) }
/// ```
///
/// * Read a file from the asset pack:
/// ```no_run
/// use smve_asset_pack::pack_io::reading::AssetPackReader;
///
/// # fn foo() -> smve_asset_pack::pack_io::reading::ReadResult<()> {
/// let mut pack_reader = AssetPackReader::new_from_path("./path/to/pack.smap")?;
/// let file_reader = pack_reader.get_file_reader("path/to/file.txt")?.unwrap();
/// # Ok(()) }
/// ```
///
/// * Read asset pack from memory:
/// ```no_run
/// use smve_asset_pack::pack_io::reading::AssetPackReader;
/// use std::io::Cursor;
///
/// # fn foo() -> smve_asset_pack::pack_io::reading::ReadResult<()> {
/// let mut pack_reader = AssetPackReader::new(Cursor::new(b"SMAP\x00\x01..."))?;
/// let file_reader = pack_reader.get_file_reader("pack/to/file.txt")?.unwrap();
/// # Ok(()) }
/// ```
/// See also [`AssetFileReader`].
#[non_exhaustive]
pub struct AssetPackReader<R: ConditionalSendSeekableBufRead> {
    reader: R,
    pack_front: PackFront,
    version: u16,
}

impl<R: ConditionalSendSeekableBufRead> Debug for AssetPackReader<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssetPackReader")
            .field("version", &self.version)
            .finish()
    }
}

impl AssetPackReader<BufReader<File>> {
    /// Create a new [`AssetPackReader`] from a path, verifies it, and reads its pack front.
    ///
    /// # Parameters
    /// - `pack_path`: Path to the asset pack file
    ///
    /// # Errors
    /// Will fail if the pack file is invalid or if the version of the format is incompatible.
    ///
    /// See [`ReadError`].
    pub fn new_from_path(pack_path: impl AsRef<Path>) -> ReadResult<Self> {
        let pack_path = pack_path.as_ref();

        let file = io!(
            File::open(pack_path),
            ReadStep::OpenPack(pack_path.to_path_buf())
        )?;

        Self::new_from_read(file)
    }
}

impl<R: ConditionalSendReadAndSeek> AssetPackReader<BufReader<R>> {
    /// Creates a new [`AssetPackReader`] from a [`Read`], verifies it, and reads its pack front.
    ///
    /// **NOTE**: If your read type already implements [`BufRead`], use [`new`](Self::new) instead
    /// to avoid double buffering.
    ///
    /// # Parameters
    /// - `reader`: A reader containing an asset pack. This will be wrapped in a [`BufReader`].
    ///
    /// # Errors
    /// Will fail if the pack file is invalid or if the version of the format is incompatible.
    ///
    /// See [`ReadError`].
    pub fn new_from_read(reader: R) -> ReadResult<Self> {
        let buf_reader = BufReader::new(reader);

        Self::new(buf_reader)
    }
}

impl<R: ConditionalSendSeekableBufRead> AssetPackReader<R> {
    /// Creates a new [`AssetPackReader`] from a [`BufRead`], verifies it, and reads its pack front.
    ///
    /// **NOTE**: If your type don't already implement [`BufRead`], use [`new_from_read`](Self::new_from_read) instead.
    ///
    /// # Parameters
    /// - `reader`: A buffered reader containing an asset pack.
    ///
    /// # Errors
    /// Will fail if the pack file is invalid or if the version of the format is incompatible.
    ///
    /// See [`ReadError`].
    pub fn new(mut reader: R) -> ReadResult<Self> {
        validate_header(&mut reader)?;

        let version = validate_version(&mut reader)?;

        let expected_toc_hash = io!(read_bytes!(reader, 32), ReadStep::ReadPackFront)?;
        let expected_dl_hash = io!(read_bytes!(reader, 32), ReadStep::ReadPackFront)?;

        let (mut toc, mut unique_files) = read_toc(&mut reader, &expected_toc_hash)?;
        let dl = read_dl(&mut reader, &expected_dl_hash)?;

        validate_files(&mut reader, &mut toc, &mut unique_files)?;

        let dl = get_dir_start_indices(&dl, &toc);

        let pack_front = PackFront {
            toc,
            directory_list: dl,
            unique_files,
        };

        Ok(Self {
            reader,
            pack_front,
            version,
        })
    }

    /// Gets the version of the format of the asset pack file.
    pub fn get_version(&self) -> u16 {
        self.version
    }

    /// Returns the pack front of the asset pack.
    ///
    /// # See also
    /// [`PackFront`]
    pub fn get_pack_front(&mut self) -> &PackFront {
        &self.pack_front
    }

    /// Returns a [`AssetFileReader`] for a specified file.
    ///
    /// # Parameters
    /// - `path`: The path of the file to be read relative to the original assets directory (without `./`)
    ///
    /// # Errors
    /// See [`ReadError`].
    ///
    /// # See Also
    /// If you wish to read a pack-unique file, see [`get_unique_file_reader`](Self::get_unique_file_reader)
    pub fn get_file_reader(&mut self, path: &str) -> ReadResult<Option<AssetFileReader<R>>> {
        let toc = &self.get_pack_front().toc;
        let meta = toc.get(path);
        if meta.is_none() {
            return Ok(None);
        }
        let meta = *meta.unwrap();

        let file_reader = DirectFileReader::new(&mut self.reader, meta)?;

        AssetFileReader::new(file_reader, meta).map(Some)
    }

    /// Returns a [`DirectFileReader`] for a specified pack-unique file.
    ///
    /// # Parameters
    /// - `path`: The path of the pack-unique file to be read relative to the `__unique__` directory.
    ///
    /// # Errors
    /// Returns an error if the file is not found in the pack or if getting the pack front fails.
    /// See [`ReadError`].
    ///
    /// # See Also
    /// If you wish to read an asset not marked as unique, see [`get_file_reader`](Self::get_file_reader).
    pub fn get_unique_file_reader(&mut self, path: &str) -> ReadResult<Option<AssetFileReader<R>>> {
        let unique_files = &self.get_pack_front().unique_files;
        let meta = unique_files.get(path);
        if meta.is_none() {
            return Ok(None);
        }
        let meta = *meta.unwrap();

        let file_reader = DirectFileReader::new(&mut self.reader, meta)?;

        AssetFileReader::new(file_reader, meta).map(Some)
    }

    /// Checks if a file exists in the asset pack.
    ///
    /// # Parameters
    /// - `path`: The path of the file to check relative to the original assets directory (without `./`)
    pub fn has_file(&mut self, path: &str) -> bool {
        let toc = &self.get_pack_front().toc;
        let meta = toc.get(path);
        meta.is_some()
    }

    /// Returns the flags for a specified file.
    ///
    /// # Parameters
    /// - `path`: The path of the file to be read relative to the original assets directory
    ///   (without `./`)
    ///
    /// # See also
    /// [Flags](https://github.com/smve-rs/smve_asset_pack/blob/master/docs/specification/v1.md#file-flags)
    pub fn get_flags(&mut self, path: &str) -> Option<u8> {
        let toc = &self.get_pack_front().toc;
        let meta = toc.get(path)?;

        Some(meta.flags)
    }

    /// Checks whether a specified path is a directory in the pack file.
    ///
    /// # Parameters
    /// - `path`: The path of the directory relative to the assets directory (without ./)
    ///
    /// # Returns
    /// `true` if the path is a directory, `false` if the path is not a directory`
    pub fn has_directory(&mut self, path: &str) -> bool {
        let pack_front = self.get_pack_front();

        pack_front.directory_list.contains_key(path)
    }
}

impl<R: ConditionalSendSeekableBufRead + 'static> AssetPackReader<R> {
    /// Converts the inner reader of an asset pack to a boxed generic reader.
    pub fn box_reader(self) -> AssetPackReader<Box<dyn ConditionalSendSeekableBufRead>> {
        let boxed_reader = Box::new(self.reader) as Box<dyn ConditionalSendSeekableBufRead>;

        AssetPackReader {
            reader: boxed_reader,
            pack_front: self.pack_front,
            version: self.version,
        }
    }
}

/// Stores the sections making up the Pack Front.
///
/// The Pack Front consists of the Table of Contents, Directory List, and the Metadata.
pub struct PackFront {
    /// The hashmap with the file path as a key and the [`FileMeta`] associated with the path as
    /// the value.
    ///
    /// This does NOT contain pack-unique files.
    pub toc: IndexMap<String, FileMeta>,
    /// A hashmap with all the directories in the pack, along with where the first file in them
    /// starts in the TOC.
    pub directory_list: HashMap<String, usize>,
    /// The hashmap with the path of the pack-unique file (without a leading __unique__/) as a key
    /// and the [`FileMeta`] associated with the path as the value.
    pub unique_files: HashMap<String, FileMeta>,
}

/// Information about the file stored in the Table of Contents of the asset pack.
///
/// See also: [V1 Specification](https://github.com/smve-rs/asset_pack/blob/master/docs/specification/v1.md#table-of-contents)
#[derive(Debug, Copy, Clone)]
pub struct FileMeta {
    /// A [`Blake3`](blake3::Hasher) hash of the file data.
    pub hash: [u8; 32],
    /// See [File Flags](https://github.com/smve-rs/asset_pack/blob/master/docs/specification/v1.md#file-flags)
    pub flags: u8,
    /// Offset in bytes from the **start of the pack file**.
    ///
    /// # Important
    /// This is different from what is specified in the specification.
    /// In the specification, it stores the offset from **the end of the TOC**,
    /// but this offset is from **the start of the pack file** for easier seeking.
    pub offset: u64,
    /// Size of the file in bytes.
    pub size: u64,
}

cfg_if! {
    if #[cfg(feature = "non_send_readers")] {
        /// A marker trait automatically implemented for anything that implements both [`BufRead`] and
        /// [`Seek`] which may be [`Send`] and [`Sync`] depending on the configuration.
        pub trait ConditionalSendSeekableBufRead: Seek + BufRead {}

        impl<T: BufRead + Seek> ConditionalSendSeekableBufRead for T {}

        /// A marker trait automatically implemented for anything that implements both [`Read`] and
        /// [`Seek`] which may be [`Send`] and [`Sync`] depending on the configuration.
        pub trait ConditionalSendReadAndSeek: Seek + Read {}

        impl<T: Read + Seek> ConditionalSendReadAndSeek for T {}
    } else {
        /// A marker trait automatically implemented for anything that implements both [`BufRead`] and
        /// [`Seek`] which may be [`Send`] and [`Sync`] depending on the configuration.
        pub trait ConditionalSendSeekableBufRead: Seek + BufRead + Send + Sync {}

        impl<T: BufRead + Seek + Send + Sync> ConditionalSendSeekableBufRead for T {}

        /// A marker trait automatically implemented for anything that implements both [`Read`] and
        /// [`Seek`] which may be [`Send`] and [`Sync`] depending on the configuration.
        pub trait ConditionalSendReadAndSeek: Seek + Read + Send + Sync {}

        impl<T: Read + Seek + Send + Sync> ConditionalSendReadAndSeek for T {}
    }
}
