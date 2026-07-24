//! Strict file-URI conversion for Husk workspaces.

use std::path::{Component, Path, PathBuf};

pub(crate) fn file_path(uri: &str) -> anyhow::Result<PathBuf> {
    let encoded = uri
        .strip_prefix("file://")
        .ok_or_else(|| anyhow::anyhow!("unsupported document URI `{uri}`"))?;
    let encoded = encoded.strip_prefix("localhost").unwrap_or(encoded);
    anyhow::ensure!(
        encoded.starts_with('/'),
        "unsupported file URI authority in `{uri}`"
    );
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        let hex = bytes
            .get(index + 1..index + 3)
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .ok_or_else(|| anyhow::anyhow!("invalid percent escape in `{uri}`"))?;
        decoded.push(
            u8::from_str_radix(hex, 16)
                .map_err(|_| anyhow::anyhow!("invalid percent escape in `{uri}`"))?,
        );
        index += 3;
    }
    let path = String::from_utf8(decoded)
        .map_err(|_| anyhow::anyhow!("file URI is not UTF-8: `{uri}`"))?;
    #[cfg(windows)]
    let path = path
        .strip_prefix('/')
        .filter(|path| path.as_bytes().get(1) == Some(&b':'))
        .map(|path| path.replace('/', "\\"))
        .unwrap_or(path);
    normalize(Path::new(&path))
}

pub(crate) fn file_uri(path: &Path) -> anyhow::Result<String> {
    let path = normalize(path)?;
    let path = path.to_string_lossy();
    #[cfg(windows)]
    let path = {
        let path = path.replace('\\', "/");
        let path = path.strip_prefix("//?/").unwrap_or(&path).to_string();
        anyhow::ensure!(
            !path.starts_with("UNC/") && !path.starts_with("//"),
            "UNC paths are not supported"
        );
        path
    };
    let mut uri = String::with_capacity(path.len().saturating_add(8));
    uri.push_str("file://");
    if !path.starts_with('/') {
        uri.push('/');
    }
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/' | b':') {
            uri.push(char::from(byte));
        } else {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            uri.push('%');
            uri.push(char::from(HEX[(byte >> 4) as usize]));
            uri.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    Ok(uri)
}

fn normalize(path: &Path) -> anyhow::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                anyhow::ensure!(
                    normalized.pop(),
                    "path escapes filesystem root: `{}`",
                    path.display()
                );
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_unicode_and_reserved_bytes() {
        let path = std::env::temp_dir().join("café #1%.hk");
        let uri = file_uri(&path).expect("encode URI");

        assert!(uri.contains("caf%C3%A9%20%231%25.hk"));
        assert_eq!(file_path(&uri).expect("decode URI"), path);
    }
}
