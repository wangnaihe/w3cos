//! Origin-private File System Access compatibility backed by a sandboxed temp root.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use w3cos_core::Value;

use crate::jsdom::{
    WeakRealmObject, disconnect_realm_class, realm_function, register_weak_realm_object,
    upgrade_realm_object,
};

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static VALUES: RefCell<Vec<WeakRealmObject>> = const { RefCell::new(Vec::new()) };
    static WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
    static NATIVE_ROOT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    static NATIVE_QUOTA: Cell<Option<u64>> = const { Cell::new(None) };
}

const CLASS_NAMES: &[&str] = &[
    "FileSystemHandle",
    "FileSystemFileHandle",
    "FileSystemDirectoryHandle",
    "FileSystemObserver",
    "FileSystemWritableFileStream",
];

fn realm_file_system_function(f: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Value {
    realm_function(crate::jsdom::realm_generation(), f)
}

fn register_file_system_value(value: &Value) {
    register_weak_realm_object(&VALUES, value);
}

fn warning() {
    WARNING_EMITTED.with(|warned| {
        if !warned.replace(true) {
            eprintln!(
                "[w3cos] warning: File System Access uses a runtime-local OPFS directory; native \
                 picker UI, origin quotas and cross-process observation require a host adapter"
            );
        }
    });
}

fn error(name: &str, message: &str) -> Value {
    w3cos_core::web::dom_exception_instance(message, name)
}

fn rejected(name: &str, message: &str) -> Value {
    w3cos_core::promise::reject(vec![error(name, message)])
}

fn illegal(name: &'static str) -> Value {
    w3cos_core::throw_value(w3cos_core::error_instance(
        "TypeError",
        vec![Value::string(&format!("Illegal constructor: {name}"))],
    ))
}

fn build_class(name: &'static str) -> Value {
    let class = if name == "FileSystemObserver" {
        realm_file_system_function(|_, args| {
            observer_value(args.first().cloned().unwrap_or(Value::Undefined))
        })
    } else {
        realm_file_system_function(move |_, _| illegal(name))
    };
    class.set_property("name", Value::string(name));
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", class.clone());
    for member in class_members(name) {
        prototype.set_property(member, Value::Undefined);
    }
    let parent = match name {
        "FileSystemFileHandle" | "FileSystemDirectoryHandle" => {
            Some(class_for("FileSystemHandle").get_property("prototype"))
        }
        "FileSystemWritableFileStream" => {
            Some(crate::streams_web::writable_stream_class().get_property("prototype"))
        }
        _ => None,
    };
    if let Some(parent) = parent {
        w3cos_core::class::set_prototype_of(&prototype, &parent);
    }
    class.set_property("prototype", prototype);
    class
}

fn class_members(name: &str) -> &'static [&'static str] {
    match name {
        "FileSystemHandle" => &[
            "isSameEntry",
            "kind",
            "name",
            "queryPermission",
            "remove",
            "requestPermission",
        ],
        "FileSystemFileHandle" => &["createWritable", "getFile", "move"],
        "FileSystemDirectoryHandle" => &[
            "entries",
            "getDirectoryHandle",
            "getFileHandle",
            "keys",
            "removeEntry",
            "resolve",
            "values",
        ],
        "FileSystemObserver" => &["disconnect", "observe"],
        "FileSystemWritableFileStream" => &["mode", "seek", "truncate", "write"],
        _ => &[],
    }
}

pub fn class_for(name: &'static str) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = build_class(name);
        classes.borrow_mut().insert(name.to_string(), class.clone());
        class
    })
}

fn opfs_root_path() -> PathBuf {
    std::env::temp_dir().join("w3cos-opfs")
}

fn valid_child_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0'])
}

fn value_path(value: &Value) -> Option<PathBuf> {
    let path = value.get_property("__w3cos_path").to_js_string();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

fn base_handle(path: &Path, kind: &str) -> Value {
    let value = Value::object(HashMap::from([
        (
            "__w3cos_path".into(),
            Value::string(&path.to_string_lossy()),
        ),
        ("kind".into(), Value::string(kind)),
        (
            "name".into(),
            Value::string(
                &path
                    .file_name()
                    .map(|name| name.to_string_lossy())
                    .unwrap_or_default(),
            ),
        ),
        (
            "queryPermission".into(),
            realm_file_system_function(|_, _| {
                warning();
                w3cos_core::promise::resolve(vec![Value::string("granted")])
            }),
        ),
        (
            "requestPermission".into(),
            realm_file_system_function(|_, _| {
                warning();
                w3cos_core::promise::resolve(vec![Value::string("granted")])
            }),
        ),
    ]));
    let own_path = path.to_path_buf();
    value.set_property(
        "isSameEntry",
        realm_file_system_function(move |_, args| {
            let same = args
                .first()
                .and_then(value_path)
                .is_some_and(|other| other == own_path);
            w3cos_core::promise::resolve(vec![Value::Bool(same)])
        }),
    );
    let remove_path = path.to_path_buf();
    value.set_property(
        "remove",
        realm_file_system_function(move |_, options| {
            let recursive = options
                .first()
                .is_some_and(|value| value.get_property("recursive").to_bool());
            let result = if remove_path.is_dir() {
                if recursive {
                    std::fs::remove_dir_all(&remove_path)
                } else {
                    std::fs::remove_dir(&remove_path)
                }
            } else {
                std::fs::remove_file(&remove_path)
            };
            match result {
                Ok(()) => w3cos_core::promise::resolve(vec![Value::Undefined]),
                Err(err) => rejected("NotAllowedError", &err.to_string()),
            }
        }),
    );
    register_file_system_value(&value);
    value
}

fn bytes_of(value: &Value) -> Vec<u8> {
    crate::files::blob_bytes(value)
        .or_else(|| w3cos_core::binary::bytes_of(value))
        .unwrap_or_else(|| value.to_js_string().into_bytes())
}

fn file_value(path: PathBuf) -> Value {
    let value = base_handle(&path, "file");
    let get_path = path.clone();
    value.set_property(
        "getFile",
        realm_file_system_function(move |_, _| match std::fs::read(&get_path) {
            Ok(bytes) => {
                let data = w3cos_core::binary::typed_array_value(
                    bytes
                        .into_iter()
                        .map(|byte| Value::Number(byte as f64))
                        .collect(),
                );
                let file = w3cos_core::class::construct(
                    &crate::files::file_class(),
                    vec![
                        Value::array(vec![data]),
                        Value::string(
                            &get_path
                                .file_name()
                                .map(|name| name.to_string_lossy())
                                .unwrap_or_default(),
                        ),
                    ],
                );
                w3cos_core::promise::resolve(vec![file])
            }
            Err(err) => rejected("NotReadableError", &err.to_string()),
        }),
    );
    let writable_path = path.clone();
    value.set_property(
        "createWritable",
        realm_file_system_function(move |_, options| {
            let keep = options
                .first()
                .is_some_and(|options| options.get_property("keepExistingData").to_bool());
            w3cos_core::promise::resolve(vec![writable_value(writable_path.clone(), keep)])
        }),
    );
    let move_path = path.clone();
    value.set_property(
        "move",
        realm_file_system_function(move |_, args| {
            let Some(name) = args.last().map(Value::to_js_string) else {
                return rejected("TypeError", "A destination name is required");
            };
            if !valid_child_name(&name) {
                return rejected("TypeMismatchError", "Invalid file name");
            }
            let destination = move_path.with_file_name(name);
            match std::fs::rename(&move_path, &destination) {
                Ok(()) => w3cos_core::promise::resolve(vec![Value::Undefined]),
                Err(err) => rejected("NotAllowedError", &err.to_string()),
            }
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("FileSystemFileHandle").get_property("prototype"),
    );
    value
}

fn writable_value(path: PathBuf, keep_existing: bool) -> Value {
    let initial = if keep_existing {
        std::fs::read(&path).unwrap_or_default()
    } else {
        Vec::new()
    };
    let bytes = Rc::new(RefCell::new(initial));
    let position = Rc::new(Cell::new(0usize));
    let value = Value::object(HashMap::from([("mode".into(), Value::string("siloed"))]));
    let write_bytes = Rc::clone(&bytes);
    let write_position = Rc::clone(&position);
    let write_path = path.clone();
    value.set_property(
        "write",
        realm_file_system_function(move |_, args| {
            let input = args.first().cloned().unwrap_or(Value::Undefined);
            let kind = input.get_property("type").to_js_string();
            if kind == "seek" {
                write_position.set(input.get_property("position").to_u32() as usize);
            } else if kind == "truncate" {
                write_bytes
                    .borrow_mut()
                    .resize(input.get_property("size").to_u32() as usize, 0);
            } else {
                let data = if kind == "write" {
                    if !input.get_property("position").is_undefined() {
                        write_position.set(input.get_property("position").to_u32() as usize);
                    }
                    bytes_of(&input.get_property("data"))
                } else {
                    bytes_of(&input)
                };
                let start = write_position.get();
                let mut target = write_bytes.borrow_mut();
                if target.len() < start + data.len() {
                    target.resize(start + data.len(), 0);
                }
                target[start..start + data.len()].copy_from_slice(&data);
                write_position.set(start + data.len());
            }
            if let Some(parent) = write_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Some(quota_error) = quota_exceeded(&write_path, write_bytes.borrow().len()) {
                return w3cos_core::promise::reject(vec![quota_error]);
            }
            match std::fs::write(&write_path, &*write_bytes.borrow()) {
                Ok(()) => w3cos_core::promise::resolve(vec![Value::Undefined]),
                Err(err) => rejected("NotAllowedError", &err.to_string()),
            }
        }),
    );
    for (method, truncate) in [("seek", false), ("truncate", true)] {
        let position = Rc::clone(&position);
        let bytes = Rc::clone(&bytes);
        let path = path.clone();
        value.set_property(
            method,
            realm_file_system_function(move |_, args| {
                let amount = args.first().map(Value::to_u32).unwrap_or_default() as usize;
                if truncate {
                    bytes.borrow_mut().resize(amount, 0);
                    if let Err(err) = std::fs::write(&path, &*bytes.borrow()) {
                        return rejected("NotAllowedError", &err.to_string());
                    }
                } else {
                    position.set(amount);
                }
                w3cos_core::promise::resolve(vec![Value::Undefined])
            }),
        );
    }
    value.set_property(
        "close",
        realm_file_system_function(|_, _| w3cos_core::promise::resolve(vec![Value::Undefined])),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("FileSystemWritableFileStream").get_property("prototype"),
    );
    register_file_system_value(&value);
    value
}

fn async_iterator(values: Vec<Value>) -> Value {
    let values = Rc::new(values);
    let index = Rc::new(Cell::new(0usize));
    let iterator = Value::object(HashMap::new());
    let next_values = Rc::clone(&values);
    let next_index = Rc::clone(&index);
    iterator.set_property(
        "next",
        realm_file_system_function(move |_, _| {
            let index = next_index.get();
            next_index.set(index + 1);
            w3cos_core::promise::resolve(vec![Value::object(HashMap::from([
                (
                    "value".into(),
                    next_values.get(index).cloned().unwrap_or(Value::Undefined),
                ),
                ("done".into(), Value::Bool(index >= next_values.len())),
            ]))])
        }),
    );
    // Return the receiver rather than capturing `iterator`, which would form
    // an uncollectable iterator -> function -> iterator cycle.
    let async_iterator = realm_file_system_function(|this, _| this);
    iterator.set_property("__w3cos_symbol_async_iterator", async_iterator.clone());
    iterator.set_property("__w3cos_symbol_asyncIterator", async_iterator);
    register_file_system_value(&iterator);
    iterator
}

fn directory_entries(path: &Path, mode: &str) -> Value {
    let Ok(entries) = std::fs::read_dir(path) else {
        return async_iterator(Vec::new());
    };
    let values = entries
        .flatten()
        .map(|entry| {
            let path = entry.path();
            let handle = if path.is_dir() {
                directory_value(path)
            } else {
                file_value(path)
            };
            match mode {
                "keys" => handle.get_property("name"),
                "values" => handle,
                _ => Value::array(vec![handle.get_property("name"), handle]),
            }
        })
        .collect();
    async_iterator(values)
}

fn directory_value(path: PathBuf) -> Value {
    let value = base_handle(&path, "directory");
    for (method, directory) in [("getFileHandle", false), ("getDirectoryHandle", true)] {
        let parent = path.clone();
        value.set_property(
            method,
            realm_file_system_function(move |_, args| {
                let name = args.first().map(Value::to_js_string).unwrap_or_default();
                if !valid_child_name(&name) {
                    return rejected("TypeMismatchError", "Invalid child name");
                }
                let target = parent.join(name);
                let create = args
                    .get(1)
                    .is_some_and(|options| options.get_property("create").to_bool());
                if create {
                    let result = if directory {
                        std::fs::create_dir_all(&target)
                    } else {
                        std::fs::OpenOptions::new()
                            .create(true)
                            .write(true)
                            .truncate(false)
                            .open(&target)
                            .map(|_| ())
                    };
                    if let Err(err) = result {
                        return rejected("NotAllowedError", &err.to_string());
                    }
                } else if !target.exists() {
                    return rejected("NotFoundError", "The requested entry does not exist");
                }
                w3cos_core::promise::resolve(vec![if directory {
                    directory_value(target)
                } else {
                    file_value(target)
                }])
            }),
        );
    }
    for (method, mode) in [
        ("entries", "entries"),
        ("keys", "keys"),
        ("values", "values"),
    ] {
        let directory = path.clone();
        value.set_property(
            method,
            realm_file_system_function(move |_, _| directory_entries(&directory, mode)),
        );
    }
    let remove_parent = path.clone();
    value.set_property(
        "removeEntry",
        realm_file_system_function(move |_, args| {
            let name = args.first().map(Value::to_js_string).unwrap_or_default();
            if !valid_child_name(&name) {
                return rejected("TypeMismatchError", "Invalid child name");
            }
            let target = remove_parent.join(name);
            let recursive = args
                .get(1)
                .is_some_and(|options| options.get_property("recursive").to_bool());
            let result = if target.is_dir() {
                if recursive {
                    std::fs::remove_dir_all(target)
                } else {
                    std::fs::remove_dir(target)
                }
            } else {
                std::fs::remove_file(target)
            };
            match result {
                Ok(()) => w3cos_core::promise::resolve(vec![Value::Undefined]),
                Err(err) => rejected("NotAllowedError", &err.to_string()),
            }
        }),
    );
    let resolve_parent = path.clone();
    value.set_property(
        "resolve",
        realm_file_system_function(move |_, args| {
            let result = args
                .first()
                .and_then(value_path)
                .and_then(|child| {
                    child
                        .strip_prefix(&resolve_parent)
                        .ok()
                        .map(Path::to_path_buf)
                })
                .map(|relative| {
                    Value::array(
                        relative
                            .components()
                            .map(|part| Value::string(&part.as_os_str().to_string_lossy()))
                            .collect(),
                    )
                })
                .unwrap_or(Value::Null);
            w3cos_core::promise::resolve(vec![result])
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("FileSystemDirectoryHandle").get_property("prototype"),
    );
    value
}

fn observer_value(callback: Value) -> Value {
    if !callback.is_function() {
        w3cos_core::throw_value(w3cos_core::error_instance(
            "TypeError",
            vec![Value::string("FileSystemObserver requires a callback")],
        ));
    }
    let observer = Value::object(HashMap::new());
    observer.set_property(
        "observe",
        realm_file_system_function(move |_, _| {
            warning();
            w3cos_core::promise::resolve(vec![Value::Undefined])
        }),
    );
    observer.set_property(
        "disconnect",
        realm_file_system_function(|_, _| Value::Undefined),
    );
    w3cos_core::class::set_prototype_of(
        &observer,
        &class_for("FileSystemObserver").get_property("prototype"),
    );
    register_file_system_value(&observer);
    observer
}

fn quota_exceeded(path: &Path, len: usize) -> Option<Value> {
    let quota = NATIVE_QUOTA.with(Cell::get)?;
    let root = NATIVE_ROOT.with(|slot| slot.borrow().clone())?;
    if !path.starts_with(&root) {
        return None;
    }
    (len as u64 > quota).then(|| {
        error(
            "QuotaExceededError",
            "the write exceeds the granted native file-system quota",
        )
    })
}

pub fn set_native_directory(path: Option<PathBuf>) {
    NATIVE_ROOT.with(|slot| *slot.borrow_mut() = path);
}

pub fn set_native_quota(bytes: Option<u64>) {
    NATIVE_QUOTA.with(|quota| quota.set(bytes));
}

fn require_native_directory() -> Result<PathBuf, Value> {
    if let Err(error) = crate::capability_gate::require("file-system") {
        return Err(w3cos_core::promise::reject(vec![error]));
    }
    NATIVE_ROOT
        .with(|slot| slot.borrow().clone())
        .ok_or_else(|| {
            rejected(
                "AbortError",
                "the file picker was dismissed; no directory was chosen",
            )
        })
}

pub fn show_directory_picker() -> Value {
    match require_native_directory() {
        Ok(path) if path.is_dir() => w3cos_core::promise::resolve(vec![directory_value(path)]),
        Ok(path) => rejected(
            "NotFoundError",
            &format!("{} is not a directory", path.display()),
        ),
        Err(rejected) => rejected,
    }
}

pub fn show_open_file_picker() -> Value {
    let root = match require_native_directory() {
        Ok(path) => path,
        Err(rejected) => return rejected,
    };
    let file = std::fs::read_dir(&root).ok().and_then(|entries| {
        entries
            .flatten()
            .map(|entry| entry.path())
            .find(|path| path.is_file())
    });
    match file {
        Some(path) => w3cos_core::promise::resolve(vec![Value::array(vec![file_value(path)])]),
        None => rejected(
            "AbortError",
            "the file picker found no files in the chosen directory",
        ),
    }
}

pub fn show_save_file_picker(options: Value) -> Value {
    let root = match require_native_directory() {
        Ok(path) => path,
        Err(rejected) => return rejected,
    };
    let name = options.get_property("suggestedName").to_js_string();
    let name = if valid_child_name(&name) {
        name
    } else {
        "untitled".into()
    };
    let path = root.join(name);
    if let Err(err) = std::fs::write(&path, []) {
        return rejected("NotAllowedError", &err.to_string());
    }
    w3cos_core::promise::resolve(vec![file_value(path)])
}

pub fn opfs_root_value() -> Result<Value, Value> {
    warning();
    let path = opfs_root_path();
    std::fs::create_dir_all(&path)
        .map(|_| directory_value(path))
        .map_err(|err| error("NotAllowedError", &err.to_string()))
}

pub fn reset() {
    VALUES.with(|values| {
        for value in values
            .borrow_mut()
            .drain(..)
            .filter_map(|value| upgrade_realm_object(&value))
        {
            for name in CLASS_NAMES {
                for member in class_members(name) {
                    value.set_property(member, Value::Undefined);
                }
            }
            for reference in [
                "__w3cos_path",
                "__w3cos_symbol_async_iterator",
                "__w3cos_symbol_asyncIterator",
                "close",
                "next",
            ] {
                value.set_property(reference, Value::Undefined);
            }
        }
    });
    let classes = CLASSES.with(|classes| std::mem::take(&mut *classes.borrow_mut()));
    for class in classes.into_values() {
        disconnect_realm_class(class);
    }
    WARNING_EMITTED.with(|warned| warned.set(false));
    NATIVE_ROOT.with(|slot| *slot.borrow_mut() = None);
    NATIVE_QUOTA.with(|quota| quota.set(None));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opfs_handles_write_and_read_files() {
        let root = opfs_root_value().unwrap();
        let file = Rc::new(RefCell::new(Value::Undefined));
        let file_for_callback = Rc::clone(&file);
        root.call_method(
            "getFileHandle",
            vec![
                Value::string("behavior-test.txt"),
                Value::object(HashMap::from([("create".into(), Value::Bool(true))])),
            ],
        )
        .call_method(
            "then",
            vec![Value::function(move |_, args| {
                *file_for_callback.borrow_mut() = args[0].clone();
                Value::Undefined
            })],
        );
        w3cos_core::promise::drain_microtasks();
        let writable = Rc::new(RefCell::new(Value::Undefined));
        let writable_for_callback = Rc::clone(&writable);
        file.borrow()
            .call_method("createWritable", Vec::new())
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *writable_for_callback.borrow_mut() = args[0].clone();
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        writable
            .borrow()
            .call_method("write", vec![Value::string("hello")]);
        assert_eq!(
            std::fs::read_to_string(opfs_root_path().join("behavior-test.txt")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn handles_streams_iterators_and_classes_are_realm_owned() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let old_directory_class = class_for("FileSystemDirectoryHandle");
        let old_stream_class = class_for("FileSystemWritableFileStream");
        let root = opfs_root_value().unwrap();
        let path = opfs_root_path().join("realm-owned-test.txt");
        let file = file_value(path.clone());
        let writable = writable_value(path, false);
        let iterator = async_iterator(vec![file.clone()]);
        let file_weak = crate::jsdom::weak_realm_object(&file);
        let iterator_weak = crate::jsdom::weak_realm_object(&iterator);

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        assert!(old_directory_class.get_property("prototype").is_undefined());
        assert!(old_stream_class.get_property("prototype").is_undefined());
        assert!(!old_directory_class.strict_eq(&class_for("FileSystemDirectoryHandle")));
        assert!(root.get_property("getFileHandle").is_undefined());
        assert!(root.get_property("__w3cos_path").is_undefined());
        assert!(file.get_property("getFile").is_undefined());
        assert!(file.get_property("move").is_undefined());
        assert!(writable.get_property("write").is_undefined());
        assert!(writable.get_property("close").is_undefined());
        assert!(iterator.get_property("next").is_undefined());

        drop(root);
        drop(file);
        drop(writable);
        drop(iterator);
        assert!(file_weak.upgrade().is_none());
        assert!(iterator_weak.upgrade().is_none());
    }

    #[test]
    fn native_directory_picker_is_permissioned_and_enforces_quota() {
        crate::capability_gate::reset();
        let root = std::env::temp_dir().join(format!("w3cos-native-fs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("note.txt"), "kept").unwrap();
        set_native_directory(Some(root.clone()));
        set_native_quota(Some(4));

        let denied = std::rc::Rc::new(RefCell::new(String::new()));
        let denied_for_catch = std::rc::Rc::clone(&denied);
        show_directory_picker().call_method(
            "catch",
            vec![Value::function(move |_, args| {
                *denied_for_catch.borrow_mut() = args[0].get_property("name").to_js_string();
                Value::Undefined
            })],
        );
        w3cos_core::promise::drain_microtasks();
        assert_eq!(&*denied.borrow(), "NotAllowedError");

        crate::capability_gate::grant("file-system");
        let directory = std::rc::Rc::new(RefCell::new(Value::Undefined));
        let directory_for_then = std::rc::Rc::clone(&directory);
        show_directory_picker().call_method(
            "then",
            vec![Value::function(move |_, args| {
                *directory_for_then.borrow_mut() = args[0].clone();
                Value::Undefined
            })],
        );
        w3cos_core::promise::drain_microtasks();
        assert_eq!(
            directory.borrow().get_property("kind").to_js_string(),
            "directory"
        );

        let files = std::rc::Rc::new(RefCell::new(Value::Undefined));
        let files_for_then = std::rc::Rc::clone(&files);
        show_open_file_picker().call_method(
            "then",
            vec![Value::function(move |_, args| {
                *files_for_then.borrow_mut() = args[0].clone();
                Value::Undefined
            })],
        );
        w3cos_core::promise::drain_microtasks();
        assert_eq!(
            files
                .borrow()
                .get_property("0")
                .get_property("name")
                .to_js_string(),
            "note.txt"
        );

        let saved = std::rc::Rc::new(RefCell::new(Value::Undefined));
        let saved_for_then = std::rc::Rc::clone(&saved);
        show_save_file_picker(Value::object(HashMap::from([(
            "suggestedName".into(),
            Value::string("fresh.txt"),
        )])))
        .call_method(
            "then",
            vec![Value::function(move |_, args| {
                *saved_for_then.borrow_mut() = args[0].clone();
                Value::Undefined
            })],
        );
        w3cos_core::promise::drain_microtasks();
        let writable = std::rc::Rc::new(RefCell::new(Value::Undefined));
        let writable_for_then = std::rc::Rc::clone(&writable);
        saved
            .borrow()
            .call_method("createWritable", vec![])
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *writable_for_then.borrow_mut() = args[0].clone();
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        let quota = std::rc::Rc::new(RefCell::new(String::new()));
        let quota_for_catch = std::rc::Rc::clone(&quota);
        writable
            .borrow()
            .call_method("write", vec![Value::string("too-big")])
            .call_method(
                "catch",
                vec![Value::function(move |_, args| {
                    *quota_for_catch.borrow_mut() = args[0].get_property("name").to_js_string();
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        assert_eq!(&*quota.borrow(), "QuotaExceededError");
        set_native_directory(None);
        set_native_quota(None);
        crate::capability_gate::reset();
        let _ = std::fs::remove_dir_all(&root);
    }
}
