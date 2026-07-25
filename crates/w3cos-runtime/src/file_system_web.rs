//! Origin-private File System Access compatibility backed by a sandboxed temp root.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use w3cos_core::Value;

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
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
        Value::function(|_, args| observer_value(args.first().cloned().unwrap_or(Value::Undefined)))
    } else {
        Value::function(move |_, _| illegal(name))
    };
    class.set_property("name", Value::string(name));
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", class.clone());
    let members: &[&str] = match name {
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
    };
    for member in members {
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
            Value::function(|_, _| {
                warning();
                w3cos_core::promise::resolve(vec![Value::string("granted")])
            }),
        ),
        (
            "requestPermission".into(),
            Value::function(|_, _| {
                warning();
                w3cos_core::promise::resolve(vec![Value::string("granted")])
            }),
        ),
    ]));
    let own_path = path.to_path_buf();
    value.set_property(
        "isSameEntry",
        Value::function(move |_, args| {
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
        Value::function(move |_, options| {
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
        Value::function(move |_, _| match std::fs::read(&get_path) {
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
        Value::function(move |_, options| {
            let keep = options
                .first()
                .is_some_and(|options| options.get_property("keepExistingData").to_bool());
            w3cos_core::promise::resolve(vec![writable_value(writable_path.clone(), keep)])
        }),
    );
    let move_path = path.clone();
    value.set_property(
        "move",
        Value::function(move |_, args| {
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
        Value::function(move |_, args| {
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
            Value::function(move |_, args| {
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
        Value::function(|_, _| w3cos_core::promise::resolve(vec![Value::Undefined])),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("FileSystemWritableFileStream").get_property("prototype"),
    );
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
        Value::function(move |_, _| {
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
    let iterator_self = iterator.clone();
    iterator.set_property(
        "__w3cos_symbol_asyncIterator",
        Value::function(move |_, _| iterator_self.clone()),
    );
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
            Value::function(move |_, args| {
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
            Value::function(move |_, _| directory_entries(&directory, mode)),
        );
    }
    let remove_parent = path.clone();
    value.set_property(
        "removeEntry",
        Value::function(move |_, args| {
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
        Value::function(move |_, args| {
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
        Value::function(move |_, _| {
            warning();
            w3cos_core::promise::resolve(vec![Value::Undefined])
        }),
    );
    observer.set_property("disconnect", Value::function(|_, _| Value::Undefined));
    w3cos_core::class::set_prototype_of(
        &observer,
        &class_for("FileSystemObserver").get_property("prototype"),
    );
    observer
}

pub fn opfs_root_value() -> Result<Value, Value> {
    warning();
    let path = opfs_root_path();
    std::fs::create_dir_all(&path)
        .map(|_| directory_value(path))
        .map_err(|err| error("NotAllowedError", &err.to_string()))
}

pub fn reset() {
    CLASSES.with(|classes| classes.borrow_mut().clear());
    WARNING_EMITTED.with(|warned| warned.set(false));
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
}
