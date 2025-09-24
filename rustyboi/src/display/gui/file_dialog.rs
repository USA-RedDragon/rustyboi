use std::path::PathBuf;
use super::actions::FileData;

/// Trait for file dialog operations that can be either sync or async
pub trait FileDialogBuilder {
    /// Add a file filter to the dialog
    fn add_filter(self, name: &str, extensions: &[&str]) -> Self;
    
    /// Set the default directory for the dialog
    fn set_directory<P: AsRef<std::path::Path>>(self, path: P) -> Self;
    
    /// Set the default filename for save dialogs
    fn set_file_name<S: AsRef<str>>(self, name: S) -> Self;
    
    /// Show the file picker dialog and execute callback with result
    fn pick_file<F>(self, callback: F) 
    where 
        F: FnOnce(Option<FileData>) + Send + 'static;
    
    /// Show the save file dialog and execute callback with result
    fn save_file<F>(self, callback: F) 
    where 
        F: FnOnce(Option<PathBuf>) + Send + 'static;
}

/// Factory function to create a new file dialog builder
pub fn new() -> impl FileDialogBuilder {
    FileDialogBuilderImpl::new()
}

#[cfg(not(target_arch = "wasm32"))]
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

#[cfg(not(target_arch = "wasm32"))]
use sync_impl::FileDialogBuilderImpl;

#[cfg(target_arch = "wasm32")]
use async_impl::FileDialogBuilderImpl;