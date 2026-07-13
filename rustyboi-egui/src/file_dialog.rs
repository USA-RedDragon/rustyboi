use std::path::PathBuf;
use super::actions::FileData;

/// Trait for file dialog operations that can be either sync or async
pub trait FileDialogBuilder {
    /// Add a file filter to the dialog
    fn add_filter(self, name: &str, extensions: &[&str]) -> Self;

    /// Set the default directory for the dialog. Unused on Android,
    /// where the SAF picker decides its own starting location.
    #[allow(dead_code)]
    fn set_directory<P: AsRef<std::path::Path>>(self, path: P) -> Self;

    /// Set the default filename for save dialogs. Unused since save/load state
    /// moved to the cross-platform Import/Export path (SaveBytes), kept for the
    /// dialog abstraction.
    #[allow(dead_code)]
    fn set_file_name<S: AsRef<str>>(self, name: S) -> Self;

    /// Show the file picker dialog and execute callback with result
    fn pick_file<F>(self, callback: F)
    where
        F: FnOnce(Option<FileData>) + Send + 'static;

    /// Show the save file dialog and execute callback with result. Unused since
    /// exports route through `SaveBytes` (rfd `save_file` can't write on web).
    #[allow(dead_code)]
    fn save_file<F>(self, callback: F)
    where
        F: FnOnce(Option<PathBuf>) + Send + 'static;
}

/// Factory function to create a new file dialog builder
pub fn new() -> impl FileDialogBuilder {
    FileDialogBuilderImpl::new()
}

#[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
mod sync_impl {
    use super::*;

    pub struct FileDialogBuilderImpl {
        dialog: rfd::FileDialog,
    }

    impl FileDialogBuilderImpl {
        pub fn new() -> Self {
            Self {
                dialog: rfd::FileDialog::new(),
            }
        }
    }

    impl FileDialogBuilder for FileDialogBuilderImpl {
        fn add_filter(mut self, name: &str, extensions: &[&str]) -> Self {
            self.dialog = self.dialog.add_filter(name, extensions);
            self
        }

        fn set_directory<P: AsRef<std::path::Path>>(mut self, path: P) -> Self {
            self.dialog = self.dialog.set_directory(path);
            self
        }

        fn set_file_name<S: AsRef<str>>(mut self, name: S) -> Self {
            self.dialog = self.dialog.set_file_name(name.as_ref());
            self
        }

        fn pick_file<F>(self, callback: F)
        where
            F: FnOnce(Option<FileData>) + Send + 'static
        {
            let result = self.dialog.pick_file().map(FileData::Path);
            callback(result);
        }

        fn save_file<F>(self, callback: F)
        where
            F: FnOnce(Option<PathBuf>) + Send + 'static
        {
            let result = self.dialog.save_file();
            callback(result);
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod async_impl {
    use super::*;

    pub struct FileDialogBuilderImpl {
        dialog: rfd::AsyncFileDialog,
    }

    impl FileDialogBuilderImpl {
        pub fn new() -> Self {
            Self {
                dialog: rfd::AsyncFileDialog::new(),
            }
        }
    }

    impl FileDialogBuilder for FileDialogBuilderImpl {
        fn add_filter(mut self, name: &str, extensions: &[&str]) -> Self {
            self.dialog = self.dialog.add_filter(name, extensions);
            self
        }

        fn set_directory<P: AsRef<std::path::Path>>(mut self, path: P) -> Self {
            self.dialog = self.dialog.set_directory(path);
            self
        }

        fn set_file_name<S: AsRef<str>>(mut self, name: S) -> Self {
            self.dialog = self.dialog.set_file_name(name.as_ref());
            self
        }

        fn pick_file<F>(self, callback: F)
        where
            F: FnOnce(Option<FileData>) + Send + 'static
        {
            wasm_bindgen_futures::spawn_local(async move {
                if let Some(file) = self.dialog.pick_file().await {
                    let name = file.file_name();
                    let data = file.read().await;
                    let file_data = FileData::Contents { name, data };
                    callback(Some(file_data));
                } else {
                    callback(None);
                }
            });
        }

        fn save_file<F>(self, callback: F)
        where
            F: FnOnce(Option<PathBuf>) + Send + 'static
        {
            // For WASM, save functionality is more complex since we need to write data
            // to the FileHandle directly. For now, we'll disable save functionality in WASM.
            // TODO: Implement proper WASM save support with a different interface
            wasm_bindgen_futures::spawn_local(async move {
                callback(None);
            });
        }
    }
}

#[cfg(target_os = "android")]
mod android_impl {
    use super::*;

    /// Android file dialog: dispatches the request to a platform-installed
    /// bridge (set up from `android_main`) which fires a SAF intent and
    /// returns the bytes asynchronously via the callback.
    pub struct FileDialogBuilderImpl {
        file_name: Option<String>,
    }

    impl FileDialogBuilderImpl {
        pub fn new() -> Self {
            Self { file_name: None }
        }
    }

    impl FileDialogBuilder for FileDialogBuilderImpl {
        fn add_filter(self, _name: &str, _extensions: &[&str]) -> Self {
            // SAF mime types are not built from filename filters; ignore.
            self
        }

        fn set_directory<P: AsRef<std::path::Path>>(self, _path: P) -> Self {
            // SAF doesn't accept a starting directory in a way we can use; ignore.
            self
        }

        fn set_file_name<S: AsRef<str>>(mut self, name: S) -> Self {
            self.file_name = Some(name.as_ref().to_string());
            self
        }

        fn pick_file<F>(self, callback: F)
        where
            F: FnOnce(Option<FileData>) + Send + 'static,
        {
            crate::android_bridge::pick_file(Box::new(callback));
        }

        fn save_file<F>(self, callback: F)
        where
            F: FnOnce(Option<PathBuf>) + Send + 'static,
        {
            // Saves go to the app's internal files directory; the bridge
            // returns the chosen path (or None on failure/cancel).
            crate::android_bridge::save_file(self.file_name, Box::new(callback));
        }
    }
}

#[cfg(target_os = "ios")]
mod ios_impl {
    use super::*;

    /// iOS file dialog. `pick_file` forwards to the platform layer's
    /// `UIDocumentPicker` bridge (installed on `ios_bridge` from `run_ios`);
    /// `save_file` is unused (exports route through `SaveBytes` / the Documents
    /// dir, never a save dialog — the same as web).
    pub struct FileDialogBuilderImpl;

    impl FileDialogBuilderImpl {
        pub fn new() -> Self {
            Self
        }
    }

    impl FileDialogBuilder for FileDialogBuilderImpl {
        fn add_filter(self, _name: &str, _extensions: &[&str]) -> Self {
            self
        }
        fn set_directory<P: AsRef<std::path::Path>>(self, _path: P) -> Self {
            self
        }
        fn set_file_name<S: AsRef<str>>(self, _name: S) -> Self {
            self
        }
        fn pick_file<F>(self, callback: F)
        where
            F: FnOnce(Option<FileData>) + Send + 'static,
        {
            crate::ios_bridge::pick_file(Box::new(callback));
        }
        fn save_file<F>(self, callback: F)
        where
            F: FnOnce(Option<PathBuf>) + Send + 'static,
        {
            callback(None);
        }
    }
}

#[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
use sync_impl::FileDialogBuilderImpl;

#[cfg(target_arch = "wasm32")]
use async_impl::FileDialogBuilderImpl;

#[cfg(target_os = "android")]
use android_impl::FileDialogBuilderImpl;

#[cfg(target_os = "ios")]
use ios_impl::FileDialogBuilderImpl;
