use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
};

use super::*;

const MAX_FILES: usize = 4096;
const MAX_BYTES: u64 = 128 * 1024 * 1024;

pub(super) struct Package {
    pub(super) root: PathBuf,
    pub(super) revision: String,
    files: Vec<(PathBuf, File)>,
    pub(super) marketplace: Option<File>,
    _root: File,
}

impl Package {
    pub(super) fn open(root: &Path) -> Result<Self> {
        let root = ordinary_root(root)?;
        let locked_root = open_config_root(&root)?;
        let mut paths = Vec::new();
        let mut directories = vec![PathBuf::new()];
        let mut directory_count = 0;
        while let Some(directory) = directories.pop() {
            directory_count += 1;
            ensure!(
                directory_count <= MAX_FILES,
                "The plugin contains too many directories"
            );
            for entry in fs::read_dir(root.join(&directory))? {
                let entry = entry?;
                let relative = directory.join(entry.file_name());
                let metadata = fs::symlink_metadata(entry.path())?;
                ensure!(
                    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
                    "The plugin contains a junction or symbolic link"
                );
                if metadata.is_dir() {
                    directories.push(relative);
                    ensure!(
                        directory_count + directories.len() <= MAX_FILES,
                        "The plugin contains too many directories"
                    );
                } else {
                    ensure!(
                        metadata.is_file(),
                        "The plugin contains an unsupported file entry"
                    );
                    paths.push(relative);
                    ensure!(
                        paths.len() <= MAX_FILES,
                        "The plugin contains too many files"
                    );
                }
            }
        }
        paths.sort_unstable();
        let mut digest = Sha256::new();
        let mut remaining = MAX_BYTES;
        let mut files = Vec::with_capacity(paths.len());
        for path in paths {
            let mut file = configuration::open_locked_local_file(&root.join(&path))?;
            let size = file.metadata()?.len();
            ensure!(size <= remaining, "The plugin exceeds the supported size");
            remaining -= size;
            let name = serde_json::to_vec(&path)?;
            digest.update((name.len() as u64).to_le_bytes());
            digest.update(name);
            digest.update(size.to_le_bytes());
            let copied = io::copy(
                &mut (&mut file).take(size + 1),
                &mut HashWriter(&mut digest),
            )?;
            ensure!(
                size == copied,
                "The plugin file changed while it was reviewed"
            );
            files.push((path, file));
        }
        Ok(Self {
            root,
            revision: format!("{:x}", digest.finalize()),
            files,
            marketplace: None,
            _root: locked_root,
        })
    }

    pub(super) fn stage(&mut self) -> Result<tempfile::TempDir> {
        let directory = tempfile::tempdir()?;
        for (path, source) in &mut self.files {
            let destination = directory.path().join(path);
            fs::create_dir_all(
                destination
                    .parent()
                    .context("The staged plugin path has no parent")?,
            )?;
            source.seek(SeekFrom::Start(0))?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)?;
            io::copy(source, &mut file)?;
        }
        Ok(directory)
    }
}

struct HashWriter<'a>(&'a mut Sha256);

impl Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
