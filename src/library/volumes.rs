use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use tempfile::NamedTempFile;

use super::Release;
use crate::archive::is_reparse_or_symlink;

type Package<'a> = (Cow<'a, Path>, &'a Release);

pub(super) fn packages<'a>(
    archives: &'a [(PathBuf, Release)],
    scratch: &Path,
) -> Result<Vec<Package<'a>>> {
    let mut result = Vec::with_capacity(archives.len());
    let mut volumes: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    for (index, (path, release)) in archives.iter().enumerate() {
        let filename = release.filename.to_ascii_lowercase();
        if let Some((base, suffix)) = filename.rsplit_once('.') {
            if (base.ends_with(".7z") || base.ends_with(".zip")) && digits(suffix) {
                let part: usize = suffix.parse().context("invalid numbered archive part")?;
                volumes
                    .entry(base.to_owned())
                    .or_default()
                    .push((part, index));
                continue;
            }
            if (suffix == "rar"
                && base
                    .rsplit_once(".part")
                    .is_some_and(|(_, number)| digits(number)))
                || suffix.strip_prefix(['r', 'z']).is_some_and(digits)
            {
                bail!(
                    "split RAR and .z01 ZIP archives are not supported; reassemble them before importing"
                );
            }
        }
        result.push((index, Cow::Borrowed(path.as_path()), release));
    }
    for (base, mut parts) in volumes {
        parts.sort_unstable();
        ensure!(
            parts.len() > 1
                && parts
                    .iter()
                    .enumerate()
                    .all(|(i, (part, _))| *part == i + 1),
            "select all numbered parts of {base}, starting at 001 with no gaps"
        );
        let mut joined = NamedTempFile::new_in(scratch)?;
        for &(_, index) in &parts {
            let input = &archives[index].0;
            let metadata = fs::symlink_metadata(input)?;
            ensure!(
                metadata.is_file() && !is_reparse_or_symlink(&metadata),
                "archive part must be a regular file"
            );
            io::copy(&mut File::open(input)?, joined.as_file_mut())?;
        }
        let first = parts[0].1;
        let order = parts.iter().map(|(_, index)| *index).min().unwrap_or(first);
        let (_, path) = joined.keep()?;
        result.push((order, Cow::Owned(path), &archives[first].1));
    }
    result.sort_unstable_by_key(|(index, _, _)| *index);
    Ok(result
        .into_iter()
        .map(|(_, path, release)| (path, release))
        .collect())
}

fn digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::{RemoteMod, Source};

    #[test]
    fn numbered_parts_are_joined_in_file_order_and_reject_gaps() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let part = |part, data: &[u8]| -> Result<_> {
            let path = temp.path().join(format!("part-{part}"));
            fs::write(&path, data)?;
            Ok((
                path,
                Release {
                    id: RemoteMod {
                        source: Source::GameBanana,
                        game: "19567".into(),
                        id: 17,
                    },
                    file_id: part,
                    filename: format!("bundle.7z.{part:03}"),
                    source_url: None,
                    version: None,
                    updated_at: None,
                },
            ))
        };
        let first = part(1, b"first")?;
        let second = part(2, b"second")?;
        let third = part(3, b"third")?;
        let archives = [third, first.clone(), second];
        let combined = packages(&archives, temp.path())?;
        assert_eq!(combined.len(), 1);
        assert_eq!(fs::read(&combined[0].0)?, b"firstsecondthird");
        assert_eq!(combined[0].1.file_id, 1);
        assert!(packages(&[first, archives[0].clone()], temp.path()).is_err());
        Ok(())
    }
}
