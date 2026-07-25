//! Web MIDI API with host-injectable ports and message delivery.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;

use w3cos_core::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MidiPortKind {
    Input,
    Output,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiPortState {
    pub id: String,
    pub kind: MidiPortKind,
    pub manufacturer: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    pub connected: bool,
}

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static PORTS: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static ACCESS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static SENT_MESSAGES: RefCell<Vec<(String, Vec<u8>, f64)>> = const { RefCell::new(Vec::new()) };
}

fn error(name: &str, message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("message".into(), Value::string(message)),
    ]))
}

fn illegal_class(name: &'static str, parent: Option<Value>) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = Value::function(move |_, _| {
            w3cos_core::throw_value(error("TypeError", &format!("Illegal constructor: {name}")))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        if let Some(parent) = parent {
            w3cos_core::class::set_prototype_of(&prototype, &parent.get_property("prototype"));
        }
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

fn class_with_members(name: &'static str, parent: Option<Value>, members: &[&str]) -> Value {
    let class = illegal_class(name, parent);
    let prototype = class.get_property("prototype");
    for member in members {
        prototype.set_property(member, Value::Undefined);
    }
    class
}

pub fn midi_access_class() -> Value {
    class_with_members(
        "MIDIAccess",
        Some(crate::web_events::event_target_class()),
        &["inputs", "onstatechange", "outputs", "sysexEnabled"],
    )
}

pub fn midi_port_class() -> Value {
    class_with_members(
        "MIDIPort",
        Some(crate::web_events::event_target_class()),
        &[
            "close",
            "connection",
            "id",
            "manufacturer",
            "name",
            "onstatechange",
            "open",
            "state",
            "type",
            "version",
        ],
    )
}

pub fn midi_input_class() -> Value {
    class_with_members("MIDIInput", Some(midi_port_class()), &["onmidimessage"])
}

pub fn midi_output_class() -> Value {
    class_with_members("MIDIOutput", Some(midi_port_class()), &["send"])
}

pub fn midi_input_map_class() -> Value {
    class_with_members(
        "MIDIInputMap",
        None,
        &["entries", "forEach", "get", "has", "keys", "size", "values"],
    )
}

pub fn midi_output_map_class() -> Value {
    class_with_members(
        "MIDIOutputMap",
        None,
        &["entries", "forEach", "get", "has", "keys", "size", "values"],
    )
}

fn event_class(name: &'static str, field: &'static str) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = Value::function(move |this, args| {
            crate::web_events::event_class().call(this.clone(), args.clone());
            let init = args.get(1).cloned().unwrap_or(Value::Undefined);
            this.set_property(field, init.get_property(field));
            Value::Undefined
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property(field, Value::Undefined);
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

pub fn midi_connection_event_class() -> Value {
    event_class("MIDIConnectionEvent", "port")
}

pub fn midi_message_event_class() -> Value {
    event_class("MIDIMessageEvent", "data")
}

fn ports_for(kind: MidiPortKind) -> Vec<(String, Value)> {
    PORTS.with(|ports| {
        ports
            .borrow()
            .iter()
            .filter(|(_, port)| {
                port.get_property("type").to_js_string()
                    == match kind {
                        MidiPortKind::Input => "input",
                        MidiPortKind::Output => "output",
                    }
            })
            .map(|(id, port)| (id.clone(), port.clone()))
            .collect()
    })
}

fn port_map(kind: MidiPortKind) -> Value {
    let map = Value::object(HashMap::new());
    let class = match kind {
        MidiPortKind::Input => midi_input_map_class(),
        MidiPortKind::Output => midi_output_map_class(),
    };
    w3cos_core::class::set_prototype_of(&map, &class.get_property("prototype"));
    map.set_property(
        "__w3cos_getter_size",
        Value::function(move |_, _| Value::Number(ports_for(kind).len() as f64)),
    );
    map.set_property(
        "get",
        Value::function(move |_, args| {
            let id = args
                .first()
                .cloned()
                .unwrap_or(Value::Undefined)
                .to_js_string();
            ports_for(kind)
                .into_iter()
                .find_map(|(key, port)| (key == id).then_some(port))
                .unwrap_or(Value::Undefined)
        }),
    );
    map.set_property(
        "has",
        Value::function(move |_, args| {
            let id = args
                .first()
                .cloned()
                .unwrap_or(Value::Undefined)
                .to_js_string();
            Value::Bool(ports_for(kind).iter().any(|(key, _)| key == &id))
        }),
    );
    let map_for_each = map.clone();
    map.set_property(
        "forEach",
        Value::function(move |_, args| {
            let callback = args.first().cloned().unwrap_or(Value::Undefined);
            if !callback.is_function() {
                w3cos_core::throw_value(error("TypeError", "MIDI map callback must be callable"));
            }
            let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
            for (key, port) in ports_for(kind) {
                callback.call(
                    this_arg.clone(),
                    vec![port, Value::string(&key), map_for_each.clone()],
                );
            }
            Value::Undefined
        }),
    );
    for (method, entry_index) in [("entries", None), ("keys", Some(0)), ("values", Some(1))] {
        map.set_property(
            method,
            Value::function(move |_, _| {
                let values = ports_for(kind)
                    .into_iter()
                    .map(|(key, port)| match entry_index {
                        Some(0) => Value::string(&key),
                        Some(_) => port,
                        None => Value::array(vec![Value::string(&key), port]),
                    })
                    .collect();
                Value::array(values)
            }),
        );
    }
    map
}

fn access_value() -> Value {
    ACCESS.with(|slot| {
        if let Some(access) = slot.borrow().clone() {
            return access;
        }
        let access = w3cos_core::class::construct(&crate::web_events::event_target_class(), vec![]);
        w3cos_core::class::set_prototype_of(
            &access,
            &midi_access_class().get_property("prototype"),
        );
        access.set_property("inputs", port_map(MidiPortKind::Input));
        access.set_property("outputs", port_map(MidiPortKind::Output));
        access.set_property("sysexEnabled", Value::Bool(false));
        access.set_property("onstatechange", Value::Null);
        *slot.borrow_mut() = Some(access.clone());
        access
    })
}

pub fn request_midi_access_value() -> Value {
    Value::function(|_, args| {
        let options = args.first().cloned().unwrap_or(Value::Undefined);
        if options.get_property("sysex").to_bool() {
            return w3cos_core::promise::reject(vec![error(
                "SecurityError",
                "system-exclusive MIDI access requires explicit platform permission",
            )]);
        }
        static WARNING: Once = Once::new();
        WARNING.call_once(|| {
            eprintln!(
                "[w3cos] warning: requestMIDIAccess exposes host-injectable MIDI ports; \
                 physical device discovery and output require a platform MIDI adapter"
            );
        });
        w3cos_core::promise::resolve(vec![access_value()])
    })
}

fn nullable_string(value: &Option<String>) -> Value {
    value.as_deref().map(Value::string).unwrap_or(Value::Null)
}

fn parse_bytes(value: &Value) -> Result<Vec<u8>, Value> {
    let length = value.get_property("length").to_u32();
    let mut bytes = Vec::with_capacity(length as usize);
    for index in 0..length {
        let number = value.get_property(&index.to_string()).to_number();
        if !number.is_finite() || number.fract() != 0.0 || !(0.0..=255.0).contains(&number) {
            return Err(error(
                "TypeError",
                "MIDI message data must contain byte values from 0 through 255",
            ));
        }
        bytes.push(number as u8);
    }
    Ok(bytes)
}

fn port_value(state: &MidiPortState) -> Value {
    let port = w3cos_core::class::construct(&crate::web_events::event_target_class(), vec![]);
    let class = match state.kind {
        MidiPortKind::Input => midi_input_class(),
        MidiPortKind::Output => midi_output_class(),
    };
    w3cos_core::class::set_prototype_of(&port, &class.get_property("prototype"));
    port.set_property("id", Value::string(&state.id));
    port.set_property(
        "type",
        Value::string(match state.kind {
            MidiPortKind::Input => "input",
            MidiPortKind::Output => "output",
        }),
    );
    port.set_property("manufacturer", nullable_string(&state.manufacturer));
    port.set_property("name", nullable_string(&state.name));
    port.set_property("version", nullable_string(&state.version));
    port.set_property("connection", Value::string("closed"));
    port.set_property("onstatechange", Value::Null);
    if state.kind == MidiPortKind::Input {
        port.set_property("onmidimessage", Value::Null);
    }
    let port_for_open = port.clone();
    port.set_property(
        "open",
        Value::function(move |_, _| {
            port_for_open.set_property("connection", Value::string("open"));
            w3cos_core::promise::resolve(vec![port_for_open.clone()])
        }),
    );
    let port_for_close = port.clone();
    port.set_property(
        "close",
        Value::function(move |_, _| {
            port_for_close.set_property("connection", Value::string("closed"));
            w3cos_core::promise::resolve(vec![port_for_close.clone()])
        }),
    );
    if state.kind == MidiPortKind::Output {
        let id = state.id.clone();
        port.set_property(
            "send",
            Value::function(move |_, args| {
                let bytes = match parse_bytes(&args.first().cloned().unwrap_or(Value::Undefined)) {
                    Ok(bytes) => bytes,
                    Err(error) => w3cos_core::throw_value(error),
                };
                let timestamp = args
                    .get(1)
                    .filter(|value| !value.is_undefined())
                    .map(Value::to_number)
                    .unwrap_or(0.0);
                if !timestamp.is_finite() || timestamp < 0.0 {
                    w3cos_core::throw_value(error(
                        "TypeError",
                        "MIDI output timestamp must be a non-negative finite number",
                    ));
                }
                static WARNING: Once = Once::new();
                WARNING.call_once(|| {
                    eprintln!(
                        "[w3cos] warning: MIDIOutput.send records compatible output; forwarding \
                         bytes to hardware requires a platform MIDI adapter"
                    );
                });
                SENT_MESSAGES.with(|messages| {
                    messages.borrow_mut().push((id.clone(), bytes, timestamp));
                });
                Value::Undefined
            }),
        );
    }
    port
}

fn dispatch_connection_event(port: Value) {
    let event = w3cos_core::class::construct(
        &midi_connection_event_class(),
        vec![
            Value::string("statechange"),
            Value::object(HashMap::from([("port".into(), port.clone())])),
        ],
    );
    event.set_property("isTrusted", Value::Bool(true));
    port.call_method("dispatchEvent", vec![event.clone()]);
    access_value().call_method("dispatchEvent", vec![event]);
}

/// Insert or update a MIDI port supplied by the platform adapter.
pub fn update_midi_port(state: MidiPortState) {
    let port = PORTS.with(|ports| ports.borrow().get(&state.id).cloned());
    let port = port.unwrap_or_else(|| port_value(&state));
    port.set_property(
        "state",
        Value::string(if state.connected {
            "connected"
        } else {
            "disconnected"
        }),
    );
    port.set_property("manufacturer", nullable_string(&state.manufacturer));
    port.set_property("name", nullable_string(&state.name));
    port.set_property("version", nullable_string(&state.version));
    PORTS.with(|ports| {
        ports.borrow_mut().insert(state.id, port.clone());
    });
    dispatch_connection_event(port);
}

/// Deliver one input packet from the platform adapter.
pub fn dispatch_midi_message(port_id: &str, bytes: &[u8]) -> bool {
    let port = PORTS.with(|ports| ports.borrow().get(port_id).cloned());
    let Some(port) = port else {
        return false;
    };
    if port.get_property("type").to_js_string() != "input"
        || port.get_property("state").to_js_string() != "connected"
    {
        return false;
    }
    let data = w3cos_core::binary::typed_array_value(
        bytes
            .iter()
            .map(|byte| Value::Number(*byte as f64))
            .collect(),
    );
    let event = w3cos_core::class::construct(
        &midi_message_event_class(),
        vec![
            Value::string("midimessage"),
            Value::object(HashMap::from([("data".into(), data)])),
        ],
    );
    event.set_property("isTrusted", Value::Bool(true));
    port.call_method("dispatchEvent", vec![event]);
    true
}

pub fn take_sent_messages() -> Vec<(String, Vec<u8>, f64)> {
    SENT_MESSAGES.with(|messages| std::mem::take(&mut *messages.borrow_mut()))
}

pub fn reset() {
    PORTS.with(|ports| ports.borrow_mut().clear());
    ACCESS.with(|access| *access.borrow_mut() = None);
    SENT_MESSAGES.with(|messages| messages.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn port(id: &str, kind: MidiPortKind) -> MidiPortState {
        MidiPortState {
            id: id.into(),
            kind,
            manufacturer: Some("W3COS".into()),
            name: Some("Virtual MIDI".into()),
            version: Some("1".into()),
            connected: true,
        }
    }

    #[test]
    fn host_ports_messages_and_output_are_standard_shaped() {
        reset();
        let access = access_value();
        let changes = Rc::new(RefCell::new(Vec::new()));
        let changes_for_listener = Rc::clone(&changes);
        access.call_method(
            "addEventListener",
            vec![
                Value::string("statechange"),
                Value::function(move |_, args| {
                    changes_for_listener.borrow_mut().push(
                        args[0]
                            .get_property("port")
                            .get_property("id")
                            .to_js_string(),
                    );
                    Value::Undefined
                }),
            ],
        );
        update_midi_port(port("in-1", MidiPortKind::Input));
        update_midi_port(port("out-1", MidiPortKind::Output));
        assert_eq!(&*changes.borrow(), &["in-1", "out-1"]);

        let input = access
            .get_property("inputs")
            .call_method("get", vec![Value::string("in-1")]);
        assert!(w3cos_core::class::instance_of(&input, &midi_input_class()));
        let received = Rc::new(RefCell::new(Vec::new()));
        let received_for_listener = Rc::clone(&received);
        input.call_method(
            "addEventListener",
            vec![
                Value::string("midimessage"),
                Value::function(move |_, args| {
                    let data = args[0].get_property("data");
                    received_for_listener
                        .borrow_mut()
                        .push(data.get_property("1").to_u32());
                    Value::Undefined
                }),
            ],
        );
        assert!(dispatch_midi_message("in-1", &[0x90, 0x40, 0x7f]));
        assert_eq!(&*received.borrow(), &[0x40]);

        let output = access
            .get_property("outputs")
            .call_method("get", vec![Value::string("out-1")]);
        output.call_method(
            "send",
            vec![
                Value::array(vec![
                    Value::Number(0x80 as f64),
                    Value::Number(0x40 as f64),
                    Value::Number(0.0),
                ]),
                Value::Number(12.0),
            ],
        );
        assert_eq!(
            take_sent_messages(),
            vec![("out-1".into(), vec![0x80, 0x40, 0], 12.0)]
        );
    }
}
