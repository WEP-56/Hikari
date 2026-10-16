use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct PathLocks {
    inner: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

impl PathLocks {
    pub fn acquire(&self, path: &Path) -> Arc<Mutex<()>> {
        #[cfg(windows)]
        let key = PathBuf::from(path.to_string_lossy().to_lowercase());
        #[cfg(not(windows))]
        let key = path.to_path_buf();
        self.inner.lock().unwrap().entry(key).or_default().clone()
    }
}
