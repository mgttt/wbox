//! Product-side logical byte accounting for wbox-owned directory trees.

use std::io;
use std::path::Path;

/// Sum file and symlink lengths without following links outside an owned tree.
pub(crate) fn logical_size(path: &Path) -> io::Result<u64> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    if !metadata.is_dir() {
        return Ok(metadata.len());
    }

    let mut total = 0_u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        total = total
            .checked_add(logical_size(&entry.path())?)
            .ok_or_else(|| io::Error::other("logical directory size exceeds u64"))?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_nested_files_and_treats_missing_tree_as_empty() {
        let home = crate::testenv::TempHome::new("logical-size");
        let root = home.dir.join("tree");
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(root.join("a"), b"abc").unwrap();
        std::fs::write(root.join("nested/b"), b"12345").unwrap();
        assert_eq!(logical_size(&root).unwrap(), 8);
        assert_eq!(logical_size(&home.dir.join("missing")).unwrap(), 0);
    }
}
