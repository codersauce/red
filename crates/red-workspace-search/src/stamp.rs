//! Validate an indexed negative against the identity and change time of the file on disk.
use std::{fs::File, path::Path};

#[derive(PartialEq, Eq)]
pub(super) struct Stamp {
    pub(super) len: u64,
    volume: u64,
    file_id: u64,
    changed: i64,
    #[cfg(unix)]
    nanos: i64,
}

#[cfg(unix)]
impl Stamp {
    fn metadata(meta: std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            len: meta.len(),
            volume: meta.dev(),
            file_id: meta.ino(),
            changed: meta.ctime(),
            nanos: meta.ctime_nsec(),
        }
    }
    pub(super) fn file(file: &File) -> Option<Self> {
        file.metadata().ok().map(Self::metadata)
    }
    pub(super) fn path(path: &Path) -> Option<Self> {
        path.metadata().ok().map(Self::metadata)
    }
}

#[cfg(windows)]
impl Stamp {
    pub(super) fn path(path: &Path) -> Option<Self> {
        Self::file(&File::open(path).ok()?)
    }

    pub(super) fn file(file: &File) -> Option<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandle,
            GetFileInformationByHandleEx,
        };
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        let mut basic = FILE_BASIC_INFO::default();
        // SAFETY: both outputs are live, correctly sized structures for this open file.
        let ok = unsafe {
            GetFileInformationByHandle(file.as_raw_handle(), &mut info) != 0
                && GetFileInformationByHandleEx(
                    file.as_raw_handle(),
                    FileBasicInfo,
                    (&raw mut basic).cast(),
                    size_of::<FILE_BASIC_INFO>() as u32,
                ) != 0
        };
        (ok && basic.ChangeTime != 0).then_some(Self {
            len: ((info.nFileSizeHigh as u64) << 32) | info.nFileSizeLow as u64,
            volume: info.dwVolumeSerialNumber as u64,
            file_id: ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
            changed: basic.ChangeTime,
        })
    }
}
