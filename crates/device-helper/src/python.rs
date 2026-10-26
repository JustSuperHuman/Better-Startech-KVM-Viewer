use anyhow::{Context, Result, bail, ensure};
use libloading::Library;
use std::ffi::{CString, c_char, c_int, c_void};
use std::path::Path;

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetDefaultDllDirectories(flags: u32) -> i32;
    fn AddDllDirectory(path: *const u16) -> *mut c_void;
    fn RemoveDllDirectory(cookie: *mut c_void) -> i32;
}

/// Owns a single interpreter on the creating thread. No Python objects cross threads.
pub struct Python {
    library: Library,
    finalize: unsafe extern "C" fn(),
    save_thread: unsafe extern "C" fn() -> *mut c_void,
    restore_thread: unsafe extern "C" fn(*mut c_void),
    thread_state: std::cell::Cell<*mut c_void>,
    _home: CString,
    _program: CString,
    dll_directory: *mut c_void,
    _not_send: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl Python {
    pub fn load(vendor: &Path) -> Result<Self> {
        ensure!(
            cfg!(all(windows, target_pointer_width = "32")),
            "The vendor bridge must be built for i686-pc-windows-msvc (32-bit Windows)."
        );
        for file in [
            "python27.dll",
            "fbext_windows.pyd",
            "library.zip",
            "usbiocom.dll",
        ] {
            ensure!(
                vendor.join(file).is_file(),
                "Missing installed vendor dependency: {}",
                vendor.join(file).display()
            );
        }
        let home_text = vendor
            .to_str()
            .context("Vendor path is not valid Unicode")?;
        ensure!(
            home_text.is_ascii(),
            "The legacy runtime currently requires an ASCII installation path"
        );
        let home = CString::new(home_text)?;
        let program = CString::new("cart-device-helper")?;

        #[cfg(windows)]
        let dll_directory = {
            use std::os::windows::ffi::OsStrExt;
            let wide: Vec<u16> = vendor.as_os_str().encode_wide().chain(Some(0)).collect();
            // Exclude the current directory and PATH from dependency resolution.
            ensure!(
                unsafe { SetDefaultDllDirectories(0x00001000) } != 0,
                "Could not restrict DLL search directories: {}",
                std::io::Error::last_os_error()
            );
            let cookie = unsafe { AddDllDirectory(wide.as_ptr()) };
            ensure!(
                !cookie.is_null(),
                "Could not add vendor DLL directory: {}",
                std::io::Error::last_os_error()
            );
            cookie
        };
        #[cfg(not(windows))]
        let dll_directory = std::ptr::null_mut();

        // The path is explicit and DLL dependency search was restricted above.
        let library = unsafe { Library::new(vendor.join("python27.dll")) }
            .context("Load installed 32-bit Python runtime")?;
        unsafe {
            for flag in [
                b"Py_NoSiteFlag\0".as_slice(),
                b"Py_IgnoreEnvironmentFlag\0",
                b"Py_DontWriteBytecodeFlag\0",
                b"Py_NoUserSiteDirectory\0",
            ] {
                let value = library
                    .get::<*mut c_int>(flag)
                    .context("Missing Python isolation control")?;
                **value = 1;
            }
            **library.get::<*mut c_int>(b"Py_OptimizeFlag\0")? = 2;
            let set_home =
                library.get::<unsafe extern "C" fn(*mut c_char)>(b"Py_SetPythonHome\0")?;
            let set_program =
                library.get::<unsafe extern "C" fn(*mut c_char)>(b"Py_SetProgramName\0")?;
            set_home(home.as_ptr().cast_mut());
            set_program(program.as_ptr().cast_mut());
            let initialize = library.get::<unsafe extern "C" fn(c_int)>(b"Py_InitializeEx\0")?;
            initialize(0);
            let initialized =
                library.get::<unsafe extern "C" fn() -> c_int>(b"Py_IsInitialized\0")?;
            ensure!(initialized() != 0, "Embedded Python initialization failed");
            let search = CString::new(format!(
                "{};{}",
                vendor.join("library.zip").display(),
                vendor.display()
            ))?;
            library.get::<unsafe extern "C" fn(*mut c_char)>(b"PySys_SetPath\0")?(
                search.as_ptr().cast_mut(),
            );
        }
        let finalize = unsafe { *library.get::<unsafe extern "C" fn()>(b"Py_Finalize\0")? };
        let save_thread = unsafe {
            *library.get::<unsafe extern "C" fn() -> *mut c_void>(b"PyEval_SaveThread\0")?
        };
        let restore_thread = unsafe {
            *library.get::<unsafe extern "C" fn(*mut c_void)>(b"PyEval_RestoreThread\0")?
        };
        unsafe {
            library.get::<unsafe extern "C" fn()>(b"PyEval_InitThreads\0")?();
        }
        let thread_state = std::cell::Cell::new(unsafe { save_thread() });
        Ok(Self {
            library,
            finalize,
            save_thread,
            restore_thread,
            thread_state,
            _home: home,
            _program: program,
            dll_directory,
            _not_send: std::marker::PhantomData,
        })
    }

    fn acquire_gil(&self) -> GilGuard<'_> {
        let state = self.thread_state.replace(std::ptr::null_mut());
        assert!(
            !state.is_null(),
            "Nested interpreter entry is not supported"
        );
        unsafe {
            (self.restore_thread)(state);
        }
        GilGuard(self)
    }

    pub fn bytes(&self, expression: &str) -> Result<Vec<u8>> {
        let started = std::time::Instant::now();
        let _gil = self.acquire_gil();
        let gil_wait = started.elapsed();
        let source = CString::new(expression)?;
        unsafe {
            let main = self
                .library
                .get::<unsafe extern "C" fn(*const c_char) -> *mut c_void>(
                    b"PyImport_AddModule\0",
                )?(c"__main__".as_ptr());
            ensure!(!main.is_null(), "Missing Python main module");
            let globals = self
                .library
                .get::<unsafe extern "C" fn(*mut c_void) -> *mut c_void>(b"PyModule_GetDict\0")?(
                main,
            );
            let object = self.library.get::<unsafe extern "C" fn(
                *const c_char,
                c_int,
                *mut c_void,
                *mut c_void,
                *mut c_void,
            ) -> *mut c_void>(b"PyRun_StringFlags\0")?(
                source.as_ptr(),
                258,
                globals,
                globals,
                std::ptr::null_mut(),
            );
            if object.is_null() {
                self.library
                    .get::<unsafe extern "C" fn()>(b"PyErr_Print\0")?();
                bail!("Embedded bridge expression failed");
            }
            let decref = self
                .library
                .get::<unsafe extern "C" fn(*mut c_void)>(b"Py_DecRef\0")?;
            let mut data: *mut c_char = std::ptr::null_mut();
            let mut length: isize = 0;
            let result = self.library.get::<unsafe extern "C" fn(
                *mut c_void,
                *mut *mut c_char,
                *mut isize,
            ) -> c_int>(b"PyString_AsStringAndSize\0")?(
                object, &mut data, &mut length
            );
            if result != 0 || !(0..=16_000_000).contains(&length) || (length > 0 && data.is_null())
            {
                decref(object);
                self.library
                    .get::<unsafe extern "C" fn()>(b"PyErr_Clear\0")?();
                bail!("Invalid bridge byte-buffer result");
            }
            let bytes = if length == 0 {
                Vec::new()
            } else {
                std::slice::from_raw_parts(data.cast::<u8>(), length as usize).to_vec()
            };
            decref(object);
            trace_latency("buffer", started.elapsed(), gil_wait);
            Ok(bytes)
        }
    }

    pub fn run(&self, source: &str) -> Result<()> {
        let started = std::time::Instant::now();
        let _gil = self.acquire_gil();
        let gil_wait = started.elapsed();
        let source = CString::new(source).context("Embedded script contains a NUL byte")?;
        let result = unsafe {
            self.library
                .get::<unsafe extern "C" fn(*const c_char, *mut c_void) -> c_int>(
                    b"PyRun_SimpleStringFlags\0",
                )?(source.as_ptr(), std::ptr::null_mut())
        };
        trace_latency("statement", started.elapsed(), gil_wait);
        if result != 0 {
            bail!(
                "Vendor bridge operation failed; the Python traceback above identifies the boundary"
            );
        }
        Ok(())
    }
}

fn trace_latency(kind: &str, total: std::time::Duration, gil: std::time::Duration) {
    if total.as_millis() >= 100 && std::env::var_os("CART_TRACE_IO").is_some() {
        eprintln!(
            "Bridge {kind}: total={} ms, GIL wait={} ms",
            total.as_millis(),
            gil.as_millis()
        );
    }
}

struct GilGuard<'a>(&'a Python);

impl Drop for GilGuard<'_> {
    fn drop(&mut self) {
        self.0.thread_state.set(unsafe { (self.0.save_thread)() });
    }
}

impl Drop for Python {
    fn drop(&mut self) {
        unsafe {
            (self.restore_thread)(self.thread_state.get());
            (self.finalize)();
        }
        #[cfg(windows)]
        if !self.dll_directory.is_null() {
            unsafe {
                RemoveDllDirectory(self.dll_directory);
            }
        }
    }
}
