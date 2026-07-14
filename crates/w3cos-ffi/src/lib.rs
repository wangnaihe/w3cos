//! `w3cos.ffi` — dynamic library loading and C ABI calls.
//!
//! Provides `dlopen`-style capability for W3C OS applications running at
//! `system` permission level. Wraps `libloading` for cross-platform
//! dynamic library loading with a safe Rust API surface.
//!
//! # Example
//! ```ignore
//! let lib = FfiLib::open("libm.so.6")?;
//! // call sin(1.0_f64)
//! let result: f64 = unsafe {
//!     lib.call_f64_f64("sin", 1.0_f64)?
//! };
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use libloading::{Library, Symbol};

// ── Error type ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FfiError(pub String);

impl std::fmt::Display for FfiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "w3cos.ffi: {}", self.0)
    }
}

impl std::error::Error for FfiError {}

impl From<libloading::Error> for FfiError {
    fn from(e: libloading::Error) -> Self {
        FfiError(e.to_string())
    }
}

pub type FfiResult<T> = Result<T, FfiError>;

// ── C type descriptors ──────────────────────────────────────────────────────

/// Primitive C types supported in call signatures.
#[derive(Debug, Clone, PartialEq)]
pub enum CType {
    Void,
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
    Ptr, // *mut c_void
    Bool,
}

impl CType {
    pub fn from_str(s: &str) -> FfiResult<Self> {
        match s.trim() {
            "void" => Ok(CType::Void),
            "i8" => Ok(CType::I8),
            "i16" => Ok(CType::I16),
            "i32" => Ok(CType::I32),
            "i64" => Ok(CType::I64),
            "u8" => Ok(CType::U8),
            "u16" => Ok(CType::U16),
            "u32" => Ok(CType::U32),
            "u64" => Ok(CType::U64),
            "f32" => Ok(CType::F32),
            "f64" => Ok(CType::F64),
            "ptr" => Ok(CType::Ptr),
            "bool" => Ok(CType::Bool),
            other => Err(FfiError(format!("unknown C type: {other}"))),
        }
    }
}

/// A C value — used for passing arguments and receiving return values.
#[derive(Debug, Clone)]
pub enum CValue {
    Void,
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    F32(f32),
    F64(f64),
    Ptr(*mut libc::c_void),
    Bool(bool),
}

// SAFETY: CValue::Ptr is only used in unsafe call sites; the pointer itself
// is not dereferenced by the Rust type system.
unsafe impl Send for CValue {}
unsafe impl Sync for CValue {}

impl CValue {
    pub fn type_of(&self) -> CType {
        match self {
            CValue::Void => CType::Void,
            CValue::I8(_) => CType::I8,
            CValue::I16(_) => CType::I16,
            CValue::I32(_) => CType::I32,
            CValue::I64(_) => CType::I64,
            CValue::U8(_) => CType::U8,
            CValue::U16(_) => CType::U16,
            CValue::U32(_) => CType::U32,
            CValue::U64(_) => CType::U64,
            CValue::F32(_) => CType::F32,
            CValue::F64(_) => CType::F64,
            CValue::Ptr(_) => CType::Ptr,
            CValue::Bool(_) => CType::Bool,
        }
    }
}

// ── FfiLib ──────────────────────────────────────────────────────────────────

/// A loaded dynamic library handle.
///
/// The underlying `Library` is kept alive as long as this struct exists.
/// Dropping `FfiLib` unloads the library.
pub struct FfiLib {
    lib: Library,
    path: String,
}

impl FfiLib {
    /// Load a dynamic library by path or name.
    ///
    /// On Linux: `"libm.so.6"` or `"/usr/lib/libm.so.6"`
    /// On macOS: `"libm.dylib"` or `"/usr/lib/libm.dylib"`
    pub fn open(path: &str) -> FfiResult<Self> {
        let lib = unsafe { Library::new(path) }?;
        Ok(Self {
            lib,
            path: path.to_string(),
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// Look up a symbol by name and call it with no arguments, returning i32.
    pub unsafe fn call_void_i32(&self, symbol: &str) -> FfiResult<i32> {
        let func: Symbol<unsafe extern "C" fn() -> i32> = self.lib.get(symbol.as_bytes())?;
        Ok(func())
    }

    /// Call a `(f64) -> f64` function (e.g. `sin`, `cos`, `sqrt`).
    pub unsafe fn call_f64_f64(&self, symbol: &str, arg: f64) -> FfiResult<f64> {
        let func: Symbol<unsafe extern "C" fn(f64) -> f64> = self.lib.get(symbol.as_bytes())?;
        Ok(func(arg))
    }

    /// Call a `(f64, f64) -> f64` function (e.g. `pow`, `atan2`).
    pub unsafe fn call_f64f64_f64(&self, symbol: &str, a: f64, b: f64) -> FfiResult<f64> {
        let func: Symbol<unsafe extern "C" fn(f64, f64) -> f64> =
            self.lib.get(symbol.as_bytes())?;
        Ok(func(a, b))
    }

    /// Call a `(i32) -> i32` function.
    pub unsafe fn call_i32_i32(&self, symbol: &str, arg: i32) -> FfiResult<i32> {
        let func: Symbol<unsafe extern "C" fn(i32) -> i32> = self.lib.get(symbol.as_bytes())?;
        Ok(func(arg))
    }

    /// Call a `(*const c_char) -> i32` function (e.g. `strlen` variant).
    pub unsafe fn call_cstr_i32(&self, symbol: &str, s: &str) -> FfiResult<i32> {
        use std::ffi::CString;
        let cs = CString::new(s).map_err(|e| FfiError(e.to_string()))?;
        let func: Symbol<unsafe extern "C" fn(*const libc::c_char) -> i32> =
            self.lib.get(symbol.as_bytes())?;
        Ok(func(cs.as_ptr()))
    }

    /// Call a `() -> *mut c_void` function.
    pub unsafe fn call_void_ptr(&self, symbol: &str) -> FfiResult<*mut libc::c_void> {
        let func: Symbol<unsafe extern "C" fn() -> *mut libc::c_void> =
            self.lib.get(symbol.as_bytes())?;
        Ok(func())
    }

    /// Call a `(*mut c_void) -> void` function (e.g. `free`-style cleanup).
    pub unsafe fn call_ptr_void(&self, symbol: &str, ptr: *mut libc::c_void) -> FfiResult<()> {
        let func: Symbol<unsafe extern "C" fn(*mut libc::c_void)> =
            self.lib.get(symbol.as_bytes())?;
        func(ptr);
        Ok(())
    }

    /// Check whether a symbol exists in the library without calling it.
    pub fn has_symbol(&self, symbol: &str) -> bool {
        unsafe {
            self.lib
                .get::<unsafe extern "C" fn()>(symbol.as_bytes())
                .is_ok()
        }
    }
}

// ── FfiRegistry ─────────────────────────────────────────────────────────────

/// A thread-safe registry of loaded libraries, keyed by path.
///
/// Mirrors the `w3cos.ffi.open` / `w3cos.ffi.close` JS API surface.
pub struct FfiRegistry {
    libs: Mutex<HashMap<String, Arc<Mutex<FfiLib>>>>,
}

impl FfiRegistry {
    pub fn new() -> Self {
        Self {
            libs: Mutex::new(HashMap::new()),
        }
    }

    /// Open (or return cached) a library handle.
    pub fn open(&self, path: &str) -> FfiResult<Arc<Mutex<FfiLib>>> {
        let mut map = self.libs.lock().map_err(|e| FfiError(e.to_string()))?;
        if let Some(lib) = map.get(path) {
            return Ok(Arc::clone(lib));
        }
        let lib = FfiLib::open(path)?;
        let handle = Arc::new(Mutex::new(lib));
        map.insert(path.to_string(), Arc::clone(&handle));
        Ok(handle)
    }

    /// Unload a library by path.
    pub fn close(&self, path: &str) -> bool {
        self.libs
            .lock()
            .map(|mut m| m.remove(path).is_some())
            .unwrap_or(false)
    }

    /// List all currently loaded library paths.
    pub fn loaded(&self) -> Vec<String> {
        self.libs
            .lock()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }
}

impl Default for FfiRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── RawBuffer ───────────────────────────────────────────────────────────────

/// A heap-allocated byte buffer that can be passed as a raw pointer to C code.
///
/// Mirrors `ArrayBuffer` in the TS API: `w3cos.ffi.buffer(size)`.
pub struct RawBuffer {
    data: Vec<u8>,
}

impl RawBuffer {
    pub fn new(size: usize) -> Self {
        Self {
            data: vec![0u8; size],
        }
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self { data: bytes }
    }

    pub fn as_ptr(&self) -> *const libc::c_void {
        self.data.as_ptr() as *const libc::c_void
    }

    pub fn as_mut_ptr(&mut self) -> *mut libc::c_void {
        self.data.as_mut_ptr() as *mut libc::c_void
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Read a null-terminated C string from offset 0.
    pub fn read_cstr(&self) -> String {
        let end = self
            .data
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.data.len());
        String::from_utf8_lossy(&self.data[..end]).to_string()
    }

    /// Write a Rust string as null-terminated C string into the buffer.
    pub fn write_cstr(&mut self, s: &str) {
        let bytes = s.as_bytes();
        let len = bytes.len().min(self.data.len().saturating_sub(1));
        self.data[..len].copy_from_slice(&bytes[..len]);
        if len < self.data.len() {
            self.data[len] = 0;
        }
    }
}

// ── StructLayout ────────────────────────────────────────────────────────────

/// Describes the memory layout of a C struct field.
#[derive(Debug, Clone)]
pub struct StructField {
    pub name: String,
    pub ty: CType,
    pub offset: usize,
    pub size: usize,
}

/// A C struct layout descriptor — used to read/write fields in a `RawBuffer`.
#[derive(Debug, Clone)]
pub struct StructLayout {
    pub name: String,
    pub fields: Vec<StructField>,
    pub total_size: usize,
}

impl StructLayout {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            fields: Vec::new(),
            total_size: 0,
        }
    }

    /// Append a field, auto-computing offset with natural alignment.
    pub fn field(mut self, name: impl Into<String>, ty: CType) -> Self {
        let size = c_type_size(&ty);
        let align = size.max(1);
        let offset = align_up(self.total_size, align);
        self.fields.push(StructField {
            name: name.into(),
            ty,
            offset,
            size,
        });
        self.total_size = offset + size;
        self
    }

    /// Read a field value from a `RawBuffer`.
    pub fn read_field(&self, buf: &RawBuffer, field_name: &str) -> FfiResult<CValue> {
        let f = self.find_field(field_name)?;
        if f.offset + f.size > buf.len() {
            return Err(FfiError(format!("buffer too small for field {field_name}")));
        }
        let slice = &buf.as_slice()[f.offset..f.offset + f.size];
        Ok(read_c_value(&f.ty, slice))
    }

    /// Write a field value into a `RawBuffer`.
    pub fn write_field(&self, buf: &mut RawBuffer, field_name: &str, val: CValue) -> FfiResult<()> {
        let f = self.find_field(field_name)?;
        if f.offset + f.size > buf.len() {
            return Err(FfiError(format!("buffer too small for field {field_name}")));
        }
        let slice = &mut buf.as_mut_slice()[f.offset..f.offset + f.size];
        write_c_value(&f.ty, slice, val)
    }

    fn find_field(&self, name: &str) -> FfiResult<&StructField> {
        self.fields
            .iter()
            .find(|f| f.name == name)
            .ok_or_else(|| FfiError(format!("field not found: {name}")))
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn c_type_size(ty: &CType) -> usize {
    match ty {
        CType::Void => 0,
        CType::I8 | CType::U8 | CType::Bool => 1,
        CType::I16 | CType::U16 => 2,
        CType::I32 | CType::U32 | CType::F32 => 4,
        CType::I64 | CType::U64 | CType::F64 => 8,
        CType::Ptr => std::mem::size_of::<usize>(),
    }
}

fn align_up(offset: usize, align: usize) -> usize {
    if align == 0 {
        return offset;
    }
    (offset + align - 1) & !(align - 1)
}

fn read_c_value(ty: &CType, bytes: &[u8]) -> CValue {
    match ty {
        CType::Void => CValue::Void,
        CType::I8 => CValue::I8(bytes[0] as i8),
        CType::U8 => CValue::U8(bytes[0]),
        CType::Bool => CValue::Bool(bytes[0] != 0),
        CType::I16 => CValue::I16(i16::from_le_bytes(bytes[..2].try_into().unwrap())),
        CType::U16 => CValue::U16(u16::from_le_bytes(bytes[..2].try_into().unwrap())),
        CType::I32 => CValue::I32(i32::from_le_bytes(bytes[..4].try_into().unwrap())),
        CType::U32 => CValue::U32(u32::from_le_bytes(bytes[..4].try_into().unwrap())),
        CType::F32 => CValue::F32(f32::from_le_bytes(bytes[..4].try_into().unwrap())),
        CType::I64 => CValue::I64(i64::from_le_bytes(bytes[..8].try_into().unwrap())),
        CType::U64 => CValue::U64(u64::from_le_bytes(bytes[..8].try_into().unwrap())),
        CType::F64 => CValue::F64(f64::from_le_bytes(bytes[..8].try_into().unwrap())),
        CType::Ptr => {
            let addr =
                usize::from_le_bytes(bytes[..std::mem::size_of::<usize>()].try_into().unwrap());
            CValue::Ptr(addr as *mut libc::c_void)
        }
    }
}

fn write_c_value(ty: &CType, bytes: &mut [u8], val: CValue) -> FfiResult<()> {
    match (ty, val) {
        (CType::I8, CValue::I8(v)) => bytes[0] = v as u8,
        (CType::U8, CValue::U8(v)) => bytes[0] = v,
        (CType::Bool, CValue::Bool(v)) => bytes[0] = v as u8,
        (CType::I16, CValue::I16(v)) => bytes[..2].copy_from_slice(&v.to_le_bytes()),
        (CType::U16, CValue::U16(v)) => bytes[..2].copy_from_slice(&v.to_le_bytes()),
        (CType::I32, CValue::I32(v)) => bytes[..4].copy_from_slice(&v.to_le_bytes()),
        (CType::U32, CValue::U32(v)) => bytes[..4].copy_from_slice(&v.to_le_bytes()),
        (CType::F32, CValue::F32(v)) => bytes[..4].copy_from_slice(&v.to_le_bytes()),
        (CType::I64, CValue::I64(v)) => bytes[..8].copy_from_slice(&v.to_le_bytes()),
        (CType::U64, CValue::U64(v)) => bytes[..8].copy_from_slice(&v.to_le_bytes()),
        (CType::F64, CValue::F64(v)) => bytes[..8].copy_from_slice(&v.to_le_bytes()),
        (CType::Ptr, CValue::Ptr(v)) => {
            let addr = v as usize;
            bytes[..std::mem::size_of::<usize>()].copy_from_slice(&addr.to_le_bytes());
        }
        (ty, val) => return Err(FfiError(format!("type mismatch: {ty:?} vs {val:?}"))),
    }
    Ok(())
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_layout_read_write() {
        let layout = StructLayout::new("Point")
            .field("x", CType::F32)
            .field("y", CType::F32);

        assert_eq!(layout.total_size, 8);

        let mut buf = RawBuffer::new(layout.total_size);
        layout.write_field(&mut buf, "x", CValue::F32(1.5)).unwrap();
        layout.write_field(&mut buf, "y", CValue::F32(2.5)).unwrap();

        let x = layout.read_field(&buf, "x").unwrap();
        let y = layout.read_field(&buf, "y").unwrap();
        assert!(matches!(x, CValue::F32(v) if (v - 1.5).abs() < 1e-6));
        assert!(matches!(y, CValue::F32(v) if (v - 2.5).abs() < 1e-6));
    }

    #[test]
    fn raw_buffer_cstr() {
        let mut buf = RawBuffer::new(32);
        buf.write_cstr("hello");
        assert_eq!(buf.read_cstr(), "hello");
    }

    #[test]
    fn registry_open_close() {
        let reg = FfiRegistry::new();
        // libm is available on Linux/macOS
        #[cfg(target_os = "linux")]
        {
            let handle = reg.open("libm.so.6");
            if handle.is_ok() {
                assert!(reg.loaded().contains(&"libm.so.6".to_string()));
                assert!(reg.close("libm.so.6"));
                assert!(reg.loaded().is_empty());
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn call_libm_sin() {
        let lib = FfiLib::open("libm.so.6");
        if let Ok(lib) = lib {
            let result = unsafe { lib.call_f64_f64("sin", 0.0) }.unwrap();
            assert!((result - 0.0).abs() < 1e-10);
        }
    }
}
