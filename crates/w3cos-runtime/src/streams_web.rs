//! JavaScript-facing WHATWG readable-stream facades.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::rc::Rc;

use w3cos_core::Value;

struct PendingRead {
    resolve: Value,
    reject: Value,
}

struct ReadableState {
    queue: VecDeque<Value>,
    pending: VecDeque<PendingRead>,
    closed: bool,
    error: Option<Value>,
    locked: bool,
    source: Value,
    controller: Value,
    on_disturb: Value,
    disturbed: bool,
}

struct WritableState {
    sink: Value,
    controller: Value,
    locked: bool,
    closed: bool,
    error: Option<Value>,
}

struct TeeCancelWaiter {
    resolve: Value,
    reject: Value,
}

struct TeeState {
    controllers: Vec<Value>,
    canceled: [bool; 2],
    reasons: Vec<Value>,
    cancel_waiters: Vec<TeeCancelWaiter>,
    finished: bool,
}

thread_local! {
    static READABLE_STREAM_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static DEFAULT_READER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static DEFAULT_CONTROLLER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static BYOB_READER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static BYOB_REQUEST_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static BYTE_CONTROLLER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static WRITABLE_STREAM_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static DEFAULT_WRITER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static WRITABLE_CONTROLLER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static TRANSFORM_STREAM_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static TRANSFORM_CONTROLLER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static COUNT_QUEUING_STRATEGY_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static BYTE_LENGTH_QUEUING_STRATEGY_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static TEXT_ENCODER_STREAM_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static TEXT_DECODER_STREAM_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static COMPRESSION_STREAM_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static DECOMPRESSION_STREAM_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static BYOB_WARNING_EMITTED: RefCell<bool> = const { RefCell::new(false) };
    static COMPRESSION_BUFFERING_WARNING_EMITTED: RefCell<bool> = const { RefCell::new(false) };
}

fn read_result(value: Value, done: bool) -> Value {
    Value::object(HashMap::from([
        ("value".to_string(), value),
        ("done".to_string(), Value::Bool(done)),
    ]))
}

fn type_error(message: &str) -> ! {
    w3cos_core::throw_value(Value::object(HashMap::from([
        ("name".into(), Value::string("TypeError")),
        ("message".into(), Value::string(message)),
    ])))
}

fn disturb(state: &Rc<RefCell<ReadableState>>) {
    let callback = {
        let mut state = state.borrow_mut();
        if state.disturbed {
            return;
        }
        state.disturbed = true;
        state.on_disturb.clone()
    };
    if callback.is_function() {
        callback.call(Value::Undefined, vec![]);
    }
}

fn controller_value(state: &Rc<RefCell<ReadableState>>) -> Value {
    let state_for_enqueue = Rc::clone(state);
    let enqueue = Value::function(move |_, args| {
        let chunk = args.first().cloned().unwrap_or(Value::Undefined);
        let pending = {
            let mut state = state_for_enqueue.borrow_mut();
            if state.closed || state.error.is_some() {
                type_error("ReadableStreamDefaultController cannot enqueue after close");
            }
            state.pending.pop_front()
        };
        if let Some(pending) = pending {
            pending
                .resolve
                .call(Value::Undefined, vec![read_result(chunk, false)]);
        } else {
            state_for_enqueue.borrow_mut().queue.push_back(chunk);
        }
        Value::Undefined
    });

    let state_for_close = Rc::clone(state);
    let close = Value::function(move |_, _| {
        let pending = {
            let mut state = state_for_close.borrow_mut();
            if state.closed || state.error.is_some() {
                type_error("ReadableStreamDefaultController is already closed");
            }
            state.closed = true;
            state.pending.drain(..).collect::<Vec<_>>()
        };
        for pending in pending {
            pending
                .resolve
                .call(Value::Undefined, vec![read_result(Value::Undefined, true)]);
        }
        Value::Undefined
    });

    let state_for_error = Rc::clone(state);
    let error = Value::function(move |_, args| {
        let reason = args.first().cloned().unwrap_or(Value::Undefined);
        let pending = {
            let mut state = state_for_error.borrow_mut();
            if state.closed || state.error.is_some() {
                return Value::Undefined;
            }
            state.error = Some(reason.clone());
            state.queue.clear();
            state.pending.drain(..).collect::<Vec<_>>()
        };
        for pending in pending {
            pending.reject.call(Value::Undefined, vec![reason.clone()]);
        }
        Value::Undefined
    });

    let state_for_size = Rc::clone(state);
    let controller = Value::object(HashMap::from([
        ("enqueue".into(), enqueue),
        ("close".into(), close),
        ("error".into(), error),
        (
            "__w3cos_getter_desiredSize".into(),
            Value::function(move |_, _| {
                let state = state_for_size.borrow();
                if state.error.is_some() {
                    Value::Null
                } else {
                    Value::Number((1_i64 - state.queue.len() as i64) as f64)
                }
            }),
        ),
        ("byobRequest".into(), Value::Null),
    ]));
    let byte_stream = state.borrow().source.get_property("type").to_js_string() == "bytes";
    w3cos_core::class::set_prototype_of(
        &controller,
        &if byte_stream {
            readable_byte_stream_controller_class()
        } else {
            readable_stream_default_controller_class()
        }
        .get_property("prototype"),
    );
    controller
}

fn reader_value(state: Rc<RefCell<ReadableState>>) -> Value {
    let state_for_read = Rc::clone(&state);
    let read = Value::function(move |_, _| {
        disturb(&state_for_read);
        let immediate = {
            let mut state = state_for_read.borrow_mut();
            if let Some(reason) = state.error.clone() {
                Some(Err(reason))
            } else if let Some(chunk) = state.queue.pop_front() {
                Some(Ok(read_result(chunk, false)))
            } else if state.closed {
                Some(Ok(read_result(Value::Undefined, true)))
            } else {
                None
            }
        };
        if let Some(result) = immediate {
            return match result {
                Ok(value) => w3cos_core::promise::resolve(vec![value]),
                Err(reason) => w3cos_core::promise::reject(vec![reason]),
            };
        }

        let (source, controller) = {
            let state = state_for_read.borrow();
            (state.source.clone(), state.controller.clone())
        };
        let pull = source.get_property("pull");
        if pull.is_function() {
            pull.call(source, vec![controller]);
        }
        let after_pull = {
            let mut state = state_for_read.borrow_mut();
            if let Some(reason) = state.error.clone() {
                Some(Err(reason))
            } else if let Some(chunk) = state.queue.pop_front() {
                Some(Ok(read_result(chunk, false)))
            } else if state.closed {
                Some(Ok(read_result(Value::Undefined, true)))
            } else {
                None
            }
        };
        if let Some(result) = after_pull {
            return match result {
                Ok(value) => w3cos_core::promise::resolve(vec![value]),
                Err(reason) => w3cos_core::promise::reject(vec![reason]),
            };
        }

        let state_for_executor = Rc::clone(&state_for_read);
        w3cos_core::promise::new(vec![Value::function(move |_, args| {
            state_for_executor
                .borrow_mut()
                .pending
                .push_back(PendingRead {
                    resolve: args.first().cloned().unwrap_or(Value::Undefined),
                    reject: args.get(1).cloned().unwrap_or(Value::Undefined),
                });
            Value::Undefined
        })])
    });

    let state_for_cancel = Rc::clone(&state);
    let cancel = Value::function(move |_, args| {
        cancel_stream(
            &state_for_cancel,
            args.first().cloned().unwrap_or(Value::Undefined),
        )
    });
    let state_for_release = Rc::clone(&state);
    let release = Value::function(move |_, _| {
        let mut state = state_for_release.borrow_mut();
        if !state.pending.is_empty() {
            type_error("Cannot release a reader with pending read requests");
        }
        state.locked = false;
        Value::Undefined
    });
    let reader = Value::object(HashMap::from([
        ("read".into(), read),
        ("cancel".into(), cancel),
        ("releaseLock".into(), release),
    ]));
    w3cos_core::class::set_prototype_of(
        &reader,
        &readable_stream_default_reader_class().get_property("prototype"),
    );
    reader
}

fn cancel_stream(state: &Rc<RefCell<ReadableState>>, reason: Value) -> Value {
    disturb(state);
    if state.borrow().closed {
        return w3cos_core::promise::resolve(vec![Value::Undefined]);
    }
    let (source, pending) = {
        let mut state = state.borrow_mut();
        state.closed = true;
        state.queue.clear();
        (
            state.source.clone(),
            state.pending.drain(..).collect::<Vec<_>>(),
        )
    };
    for pending in pending {
        pending
            .resolve
            .call(Value::Undefined, vec![read_result(Value::Undefined, true)]);
    }
    let cancel = source.get_property("cancel");
    let result = if cancel.is_function() {
        cancel.call(source, vec![reason])
    } else {
        Value::Undefined
    };
    w3cos_core::promise::resolve(vec![result])
}

fn acquire_reader(state: &Rc<RefCell<ReadableState>>) -> Value {
    {
        let mut state = state.borrow_mut();
        if state.locked {
            type_error("ReadableStream is already locked");
        }
        state.locked = true;
    }
    reader_value(Rc::clone(state))
}

fn readable_stream_async_iterator(
    state: &Rc<RefCell<ReadableState>>,
    prevent_cancel: bool,
) -> Value {
    let reader = acquire_reader(state);
    let finished = Rc::new(Cell::new(false));
    let iterator_slot = Rc::new(RefCell::new(Value::Undefined));
    let iterator = Value::object(HashMap::new());

    let reader_for_next = reader.clone();
    let finished_for_next = Rc::clone(&finished);
    iterator.set_property(
        "next",
        Value::function(move |_, _| {
            if finished_for_next.get() {
                return w3cos_core::promise::resolve(vec![read_result(Value::Undefined, true)]);
            }
            let reader = reader_for_next.clone();
            let finished = Rc::clone(&finished_for_next);
            w3cos_core::promise::new(vec![Value::function(move |_, args| {
                let resolve = args.first().cloned().unwrap_or(Value::Undefined);
                let reject = args.get(1).cloned().unwrap_or(Value::Undefined);
                let reader_for_success = reader.clone();
                let finished_for_success = Rc::clone(&finished);
                let reader_for_failure = reader.clone();
                let finished_for_failure = Rc::clone(&finished);
                reader.call_method("read", vec![]).call_method(
                    "then",
                    vec![
                        Value::function(move |_, args| {
                            let result = args.first().cloned().unwrap_or(Value::Undefined);
                            if result.get_property("done").to_bool() {
                                finished_for_success.set(true);
                                reader_for_success.call_method("releaseLock", vec![]);
                            }
                            resolve.call(Value::Undefined, vec![result]);
                            Value::Undefined
                        }),
                        Value::function(move |_, args| {
                            let reason = args.first().cloned().unwrap_or(Value::Undefined);
                            finished_for_failure.set(true);
                            reader_for_failure.call_method("releaseLock", vec![]);
                            reject.call(Value::Undefined, vec![reason]);
                            Value::Undefined
                        }),
                    ],
                );
                Value::Undefined
            })])
        }),
    );

    let reader_for_return = reader;
    let finished_for_return = Rc::clone(&finished);
    iterator.set_property(
        "return",
        Value::function(move |_, args| {
            let result = read_result(Value::Undefined, true);
            if finished_for_return.replace(true) {
                return w3cos_core::promise::resolve(vec![result]);
            }
            let reason = args.first().cloned().unwrap_or(Value::Undefined);
            if prevent_cancel {
                reader_for_return.call_method("releaseLock", vec![]);
                return w3cos_core::promise::resolve(vec![result]);
            }
            let reader = reader_for_return.clone();
            w3cos_core::promise::new(vec![Value::function(move |_, args| {
                let resolve = args.first().cloned().unwrap_or(Value::Undefined);
                let reject = args.get(1).cloned().unwrap_or(Value::Undefined);
                let reader_for_success = reader.clone();
                let result_for_success = result.clone();
                let reader_for_failure = reader.clone();
                reader
                    .call_method("cancel", vec![reason.clone()])
                    .call_method(
                        "then",
                        vec![
                            Value::function(move |_, _| {
                                reader_for_success.call_method("releaseLock", vec![]);
                                resolve.call(Value::Undefined, vec![result_for_success.clone()]);
                                Value::Undefined
                            }),
                            Value::function(move |_, args| {
                                reader_for_failure.call_method("releaseLock", vec![]);
                                reject.call(
                                    Value::Undefined,
                                    vec![args.first().cloned().unwrap_or(Value::Undefined)],
                                );
                                Value::Undefined
                            }),
                        ],
                    );
                Value::Undefined
            })])
        }),
    );
    let iterator_slot_for_method = Rc::clone(&iterator_slot);
    iterator.set_property(
        "__w3cos_symbol_asyncIterator",
        Value::function(move |_, _| iterator_slot_for_method.borrow().clone()),
    );
    *iterator_slot.borrow_mut() = iterator.clone();
    iterator
}

fn pipe_to(state: &Rc<RefCell<ReadableState>>, destination: Value, options: Value) -> Value {
    let prevent_close = options.get_property("preventClose").to_bool();
    let prevent_abort = options.get_property("preventAbort").to_bool();
    let prevent_cancel = options.get_property("preventCancel").to_bool();
    let signal = options.get_property("signal");
    let reader = acquire_reader(state);
    let writer = destination.call_method("getWriter", vec![]);
    w3cos_core::promise::new(vec![Value::function(move |_, args| {
        let resolve = args.first().cloned().unwrap_or(Value::Undefined);
        let reject = args.get(1).cloned().unwrap_or(Value::Undefined);
        let settled = Rc::new(Cell::new(false));
        let settled_for_finish = Rc::clone(&settled);
        let reader_for_finish = reader.clone();
        let writer_for_finish = writer.clone();
        let finish = Value::function(move |_, args| {
            if settled_for_finish.replace(true) {
                return Value::Undefined;
            }
            reader_for_finish.call_method("releaseLock", vec![]);
            writer_for_finish.call_method("releaseLock", vec![]);
            let successful = args.first().cloned().unwrap_or(Value::Undefined).to_bool();
            let result = args.get(1).cloned().unwrap_or(Value::Undefined);
            if successful {
                resolve.call(Value::Undefined, vec![Value::Undefined]);
            } else {
                reject.call(Value::Undefined, vec![result]);
            }
            Value::Undefined
        });

        if !signal.is_undefined() {
            if !signal.get_property("addEventListener").is_function() {
                finish.call(
                    Value::Undefined,
                    vec![
                        Value::Bool(false),
                        Value::object(HashMap::from([
                            ("name".into(), Value::string("TypeError")),
                            (
                                "message".into(),
                                Value::string("pipeTo signal must be an AbortSignal"),
                            ),
                        ])),
                    ],
                );
                return Value::Undefined;
            }
            let signal_for_abort = signal.clone();
            let reader_for_abort = reader.clone();
            let writer_for_abort = writer.clone();
            let finish_for_abort = finish.clone();
            let settled_for_abort = Rc::clone(&settled);
            let abort = Value::function(move |_, _| {
                if settled_for_abort.get() {
                    return Value::Undefined;
                }
                let reason = signal_for_abort.get_property("reason");
                if !prevent_abort {
                    writer_for_abort.call_method("abort", vec![reason.clone()]);
                }
                if !prevent_cancel {
                    reader_for_abort.call_method("cancel", vec![reason.clone()]);
                }
                finish_for_abort.call(Value::Undefined, vec![Value::Bool(false), reason])
            });
            signal.call_method(
                "addEventListener",
                vec![
                    Value::string("abort"),
                    abort.clone(),
                    Value::object(HashMap::from([("once".into(), Value::Bool(true))])),
                ],
            );
            if signal.get_property("aborted").to_bool() {
                abort.call(Value::Undefined, vec![]);
                return Value::Undefined;
            }
        }

        let pump = Rc::new(RefCell::new(Value::Undefined));
        let pump_for_body = Rc::clone(&pump);
        let reader_for_body = reader.clone();
        let writer_for_body = writer.clone();
        let finish_for_body = finish.clone();
        let settled_for_body = Rc::clone(&settled);
        *pump.borrow_mut() = Value::function(move |_, _| {
            if settled_for_body.get() {
                return Value::Undefined;
            }
            let pump_for_result = Rc::clone(&pump_for_body);
            let reader_for_result = reader_for_body.clone();
            let writer_for_result = writer_for_body.clone();
            let finish_for_result = finish_for_body.clone();
            let settled_for_result = Rc::clone(&settled_for_body);
            let writer_for_read_error = writer_for_body.clone();
            let finish_for_read_error = finish_for_body.clone();
            let settled_for_read_error = Rc::clone(&settled_for_body);
            reader_for_body.call_method("read", vec![]).call_method(
                "then",
                vec![
                    Value::function(move |_, args| {
                        if settled_for_result.get() {
                            return Value::Undefined;
                        }
                        let result = args.first().cloned().unwrap_or(Value::Undefined);
                        if result.get_property("done").to_bool() {
                            if prevent_close {
                                finish_for_result.call(
                                    Value::Undefined,
                                    vec![Value::Bool(true), Value::Undefined],
                                );
                            } else {
                                let finish_for_close = finish_for_result.clone();
                                let finish_for_close_error = finish_for_result.clone();
                                let reader_for_close_error = reader_for_result.clone();
                                writer_for_result.call_method("close", vec![]).call_method(
                                    "then",
                                    vec![
                                        Value::function(move |_, _| {
                                            finish_for_close.call(
                                                Value::Undefined,
                                                vec![Value::Bool(true), Value::Undefined],
                                            )
                                        }),
                                        Value::function(move |_, args| {
                                            let reason =
                                                args.first().cloned().unwrap_or(Value::Undefined);
                                            if !prevent_cancel {
                                                reader_for_close_error
                                                    .call_method("cancel", vec![reason.clone()]);
                                            }
                                            finish_for_close_error.call(
                                                Value::Undefined,
                                                vec![Value::Bool(false), reason],
                                            )
                                        }),
                                    ],
                                );
                            }
                        } else {
                            let next = pump_for_result.borrow().clone();
                            let reader_for_write_error = reader_for_result.clone();
                            let finish_for_write_error = finish_for_result.clone();
                            writer_for_result
                                .call_method("write", vec![result.get_property("value")])
                                .call_method(
                                    "then",
                                    vec![
                                        next,
                                        Value::function(move |_, args| {
                                            let reason =
                                                args.first().cloned().unwrap_or(Value::Undefined);
                                            if !prevent_cancel {
                                                reader_for_write_error
                                                    .call_method("cancel", vec![reason.clone()]);
                                            }
                                            finish_for_write_error.call(
                                                Value::Undefined,
                                                vec![Value::Bool(false), reason],
                                            )
                                        }),
                                    ],
                                );
                        }
                        Value::Undefined
                    }),
                    Value::function(move |_, args| {
                        if settled_for_read_error.get() {
                            return Value::Undefined;
                        }
                        let reason = args.first().cloned().unwrap_or(Value::Undefined);
                        if !prevent_abort {
                            writer_for_read_error.call_method("abort", vec![reason.clone()]);
                        }
                        finish_for_read_error
                            .call(Value::Undefined, vec![Value::Bool(false), reason])
                    }),
                ],
            );
            Value::Undefined
        });
        crate::jsdom::queue_microtask_value(pump.borrow().clone());
        Value::Undefined
    })])
}

fn tee(state: &Rc<RefCell<ReadableState>>) -> Value {
    let coordination = Rc::new(RefCell::new(TeeState {
        controllers: vec![Value::Undefined, Value::Undefined],
        canceled: [false, false],
        reasons: vec![Value::Undefined, Value::Undefined],
        cancel_waiters: Vec::new(),
        finished: false,
    }));
    let reader_slot = Rc::new(RefCell::new(Value::Undefined));
    let mut branches = Vec::new();
    for index in 0..2 {
        let coordination_for_start = Rc::clone(&coordination);
        let coordination_for_cancel = Rc::clone(&coordination);
        let reader_for_cancel = Rc::clone(&reader_slot);
        let source = Value::object(HashMap::new());
        source.set_property(
            "start",
            Value::function(move |_, args| {
                coordination_for_start.borrow_mut().controllers[index] =
                    args.first().cloned().unwrap_or(Value::Undefined);
                Value::Undefined
            }),
        );
        source.set_property(
            "cancel",
            Value::function(move |_, args| {
                let reason = args.first().cloned().unwrap_or(Value::Undefined);
                {
                    let mut coordination = coordination_for_cancel.borrow_mut();
                    coordination.canceled[index] = true;
                    coordination.reasons[index] = reason;
                }
                let coordination_for_executor = Rc::clone(&coordination_for_cancel);
                let reader_for_executor = Rc::clone(&reader_for_cancel);
                w3cos_core::promise::new(vec![Value::function(move |_, args| {
                    let resolve = args.first().cloned().unwrap_or(Value::Undefined);
                    let reject = args.get(1).cloned().unwrap_or(Value::Undefined);
                    let reasons = {
                        let mut coordination = coordination_for_executor.borrow_mut();
                        if coordination.finished {
                            resolve.call(Value::Undefined, vec![Value::Undefined]);
                            return Value::Undefined;
                        }
                        coordination
                            .cancel_waiters
                            .push(TeeCancelWaiter { resolve, reject });
                        if coordination.canceled.iter().all(|canceled| *canceled) {
                            coordination.finished = true;
                            Some(coordination.reasons.clone())
                        } else {
                            None
                        }
                    };
                    if let Some(reasons) = reasons {
                        let reader = reader_for_executor.borrow().clone();
                        let coordination_for_success = Rc::clone(&coordination_for_executor);
                        let coordination_for_error = Rc::clone(&coordination_for_executor);
                        let reader_for_success = reader.clone();
                        let reader_for_error = reader.clone();
                        reader
                            .call_method("cancel", vec![Value::array(reasons)])
                            .call_method(
                                "then",
                                vec![
                                    Value::function(move |_, _| {
                                        reader_for_success.call_method("releaseLock", vec![]);
                                        let waiters = std::mem::take(
                                            &mut coordination_for_success
                                                .borrow_mut()
                                                .cancel_waiters,
                                        );
                                        for waiter in waiters {
                                            waiter
                                                .resolve
                                                .call(Value::Undefined, vec![Value::Undefined]);
                                        }
                                        Value::Undefined
                                    }),
                                    Value::function(move |_, args| {
                                        reader_for_error.call_method("releaseLock", vec![]);
                                        let reason =
                                            args.first().cloned().unwrap_or(Value::Undefined);
                                        let waiters = std::mem::take(
                                            &mut coordination_for_error.borrow_mut().cancel_waiters,
                                        );
                                        for waiter in waiters {
                                            waiter
                                                .reject
                                                .call(Value::Undefined, vec![reason.clone()]);
                                        }
                                        Value::Undefined
                                    }),
                                ],
                            );
                    }
                    Value::Undefined
                })])
            }),
        );
        branches.push(stream_value(source, Value::Undefined));
    }
    let reader = acquire_reader(state);
    *reader_slot.borrow_mut() = reader.clone();
    let pump = Rc::new(RefCell::new(Value::Undefined));
    let pump_for_body = Rc::clone(&pump);
    let coordination_for_body = Rc::clone(&coordination);
    let reader_for_body = reader.clone();
    *pump.borrow_mut() = Value::function(move |_, _| {
        if coordination_for_body.borrow().finished {
            return Value::Undefined;
        }
        let pump_for_result = Rc::clone(&pump_for_body);
        let coordination_for_result = Rc::clone(&coordination_for_body);
        let reader_for_result = reader_for_body.clone();
        let coordination_for_error = Rc::clone(&coordination_for_body);
        let reader_for_error = reader_for_body.clone();
        reader_for_body.call_method("read", vec![]).call_method(
            "then",
            vec![
                Value::function(move |_, args| {
                    if coordination_for_result.borrow().finished {
                        return Value::Undefined;
                    }
                    let result = args.first().cloned().unwrap_or(Value::Undefined);
                    if result.get_property("done").to_bool() {
                        let (controllers, waiters) = {
                            let mut coordination = coordination_for_result.borrow_mut();
                            coordination.finished = true;
                            let controllers = coordination
                                .controllers
                                .iter()
                                .enumerate()
                                .filter_map(|(index, controller)| {
                                    (!coordination.canceled[index]).then_some(controller.clone())
                                })
                                .collect::<Vec<_>>();
                            let waiters = std::mem::take(&mut coordination.cancel_waiters);
                            (controllers, waiters)
                        };
                        for controller in controllers {
                            controller.call_method("close", vec![]);
                        }
                        for waiter in waiters {
                            waiter
                                .resolve
                                .call(Value::Undefined, vec![Value::Undefined]);
                        }
                        reader_for_result.call_method("releaseLock", vec![]);
                    } else {
                        let chunk = result.get_property("value");
                        let controllers = {
                            let coordination = coordination_for_result.borrow();
                            coordination
                                .controllers
                                .iter()
                                .enumerate()
                                .filter_map(|(index, controller)| {
                                    (!coordination.canceled[index]).then_some(controller.clone())
                                })
                                .collect::<Vec<_>>()
                        };
                        for controller in controllers {
                            controller.call_method("enqueue", vec![chunk.clone()]);
                        }
                        crate::jsdom::queue_microtask_value(pump_for_result.borrow().clone());
                    }
                    Value::Undefined
                }),
                Value::function(move |_, args| {
                    let reason = args.first().cloned().unwrap_or(Value::Undefined);
                    let (controllers, waiters) = {
                        let mut coordination = coordination_for_error.borrow_mut();
                        if coordination.finished {
                            return Value::Undefined;
                        }
                        coordination.finished = true;
                        let controllers = coordination
                            .controllers
                            .iter()
                            .enumerate()
                            .filter_map(|(index, controller)| {
                                (!coordination.canceled[index]).then_some(controller.clone())
                            })
                            .collect::<Vec<_>>();
                        let waiters = std::mem::take(&mut coordination.cancel_waiters);
                        (controllers, waiters)
                    };
                    for controller in controllers {
                        controller.call_method("error", vec![reason.clone()]);
                    }
                    for waiter in waiters {
                        waiter.reject.call(Value::Undefined, vec![reason.clone()]);
                    }
                    reader_for_error.call_method("releaseLock", vec![]);
                    Value::Undefined
                }),
            ],
        );
        Value::Undefined
    });
    crate::jsdom::queue_microtask_value(pump.borrow().clone());
    Value::array(branches)
}

fn stream_value(source: Value, on_disturb: Value) -> Value {
    let state = Rc::new(RefCell::new(ReadableState {
        queue: VecDeque::new(),
        pending: VecDeque::new(),
        closed: false,
        error: None,
        locked: false,
        source: source.clone(),
        controller: Value::Undefined,
        on_disturb,
        disturbed: false,
    }));
    let controller = controller_value(&state);
    state.borrow_mut().controller = controller.clone();
    let state_for_locked = Rc::clone(&state);
    let stream = Value::object(HashMap::from([(
        "__w3cos_getter_locked".into(),
        Value::function(move |_, _| Value::Bool(state_for_locked.borrow().locked)),
    )]));

    let state_for_reader = Rc::clone(&state);
    stream.set_property(
        "getReader",
        Value::function(move |_, options| {
            if options
                .first()
                .is_some_and(|value| value.get_property("mode").to_js_string() == "byob")
            {
                BYOB_WARNING_EMITTED.with(|emitted| {
                    if !std::mem::replace(&mut *emitted.borrow_mut(), true) {
                        eprintln!(
                            "[w3cos] warning: ReadableStream BYOB reads preserve compatible \
                             locking and delivery but do not yet write chunks into the supplied view"
                        );
                    }
                });
                {
                    let mut state = state_for_reader.borrow_mut();
                    if state.locked {
                        type_error("ReadableStream is already locked");
                    }
                    if state.source.get_property("type").to_js_string() != "bytes" {
                        type_error("A BYOB reader requires a byte ReadableStream");
                    }
                    state.locked = true;
                }
                let reader = reader_value(Rc::clone(&state_for_reader));
                w3cos_core::class::set_prototype_of(
                    &reader,
                    &readable_stream_byob_reader_class().get_property("prototype"),
                );
                return reader;
            }
            {
                let mut state = state_for_reader.borrow_mut();
                if state.locked {
                    type_error("ReadableStream is already locked");
                }
                state.locked = true;
            }
            reader_value(Rc::clone(&state_for_reader))
        }),
    );
    let state_for_cancel = Rc::clone(&state);
    stream.set_property(
        "cancel",
        Value::function(move |_, args| {
            if state_for_cancel.borrow().locked {
                type_error("Cannot cancel a locked ReadableStream");
            }
            cancel_stream(
                &state_for_cancel,
                args.first().cloned().unwrap_or(Value::Undefined),
            )
        }),
    );
    let state_for_pipe = Rc::clone(&state);
    stream.set_property(
        "pipeTo",
        Value::function(move |_, args| {
            pipe_to(
                &state_for_pipe,
                args.first().cloned().unwrap_or(Value::Undefined),
                args.get(1).cloned().unwrap_or(Value::Undefined),
            )
        }),
    );
    let state_for_pipe_through = Rc::clone(&state);
    stream.set_property(
        "pipeThrough",
        Value::function(move |_, args| {
            let pair = args.first().cloned().unwrap_or(Value::Undefined);
            pipe_to(
                &state_for_pipe_through,
                pair.get_property("writable"),
                args.get(1).cloned().unwrap_or(Value::Undefined),
            );
            pair.get_property("readable")
        }),
    );
    let state_for_tee = Rc::clone(&state);
    stream.set_property("tee", Value::function(move |_, _| tee(&state_for_tee)));
    let state_for_values = Rc::clone(&state);
    stream.set_property(
        "values",
        Value::function(move |_, args| {
            let prevent_cancel = args
                .first()
                .is_some_and(|options| options.get_property("preventCancel").to_bool());
            readable_stream_async_iterator(&state_for_values, prevent_cancel)
        }),
    );
    let state_for_async_iterator = Rc::clone(&state);
    stream.set_property(
        "__w3cos_symbol_asyncIterator",
        Value::function(move |_, _| {
            readable_stream_async_iterator(&state_for_async_iterator, false)
        }),
    );
    w3cos_core::class::set_prototype_of(
        &stream,
        &readable_stream_class().get_property("prototype"),
    );

    let start = source.get_property("start");
    if start.is_function() {
        start.call(source, vec![controller]);
    }
    stream
}

pub fn readable_stream_class() -> Value {
    READABLE_STREAM_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            stream_value(
                args.first()
                    .cloned()
                    .unwrap_or_else(|| Value::object(HashMap::new())),
                Value::Undefined,
            )
        });
        class.set_property("name", Value::string("ReadableStream"));
        class.set_property(
            "from",
            Value::function(|_, args| {
                let values = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .iter()
                    .collect::<Vec<_>>();
                let source = Value::object(HashMap::new());
                let values_for_start = values;
                source.set_property(
                    "start",
                    Value::function(move |_, args| {
                        let controller = args.first().cloned().unwrap_or(Value::Undefined);
                        for value in &values_for_start {
                            controller.call_method("enqueue", vec![value.clone()]);
                        }
                        controller.call_method("close", vec![]);
                        Value::Undefined
                    }),
                );
                stream_value(source, Value::Undefined)
            }),
        );
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in [
            "cancel",
            "getReader",
            "pipeThrough",
            "pipeTo",
            "tee",
            "values",
        ] {
            prototype.set_property(method, Value::function(|_, _| Value::Undefined));
        }
        prototype.set_property("locked", Value::Undefined);
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn readable_stream_default_reader_class() -> Value {
    DEFAULT_READER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            args.first()
                .cloned()
                .unwrap_or(Value::Undefined)
                .call_method("getReader", vec![])
        });
        class.set_property("name", Value::string("ReadableStreamDefaultReader"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in ["cancel", "read", "releaseLock"] {
            prototype.set_property(method, Value::function(|_, _| Value::Undefined));
        }
        prototype.set_property("closed", Value::Undefined);
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn readable_stream_default_controller_class() -> Value {
    DEFAULT_CONTROLLER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            type_error("ReadableStreamDefaultController cannot be constructed directly")
        });
        class.set_property("name", Value::string("ReadableStreamDefaultController"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in ["close", "enqueue", "error"] {
            prototype.set_property(method, Value::function(|_, _| Value::Undefined));
        }
        prototype.set_property("desiredSize", Value::Undefined);
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn readable_stream_byob_reader_class() -> Value {
    BYOB_READER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            let options = Value::object(HashMap::from([("mode".into(), Value::string("byob"))]));
            args.first()
                .cloned()
                .unwrap_or(Value::Undefined)
                .call_method("getReader", vec![options])
        });
        class.set_property("name", Value::string("ReadableStreamBYOBReader"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in ["cancel", "read", "releaseLock"] {
            prototype.set_property(method, Value::function(|_, _| Value::Undefined));
        }
        prototype.set_property("closed", Value::Undefined);
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn readable_stream_byob_request_class() -> Value {
    BYOB_REQUEST_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            type_error("ReadableStreamBYOBRequest cannot be constructed directly")
        });
        class.set_property("name", Value::string("ReadableStreamBYOBRequest"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("view", Value::Undefined);
        for method in ["respond", "respondWithNewView"] {
            prototype.set_property(method, Value::function(|_, _| Value::Undefined));
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn readable_byte_stream_controller_class() -> Value {
    BYTE_CONTROLLER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            type_error("ReadableByteStreamController cannot be constructed directly")
        });
        class.set_property("name", Value::string("ReadableByteStreamController"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in ["close", "enqueue", "error"] {
            prototype.set_property(method, Value::function(|_, _| Value::Undefined));
        }
        for property in ["byobRequest", "desiredSize"] {
            prototype.set_property(property, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn writable_controller_value(state: &Rc<RefCell<WritableState>>) -> Value {
    let state_for_error = Rc::clone(state);
    let controller = Value::object(HashMap::from([
        (
            "error".into(),
            Value::function(move |_, args| {
                let mut state = state_for_error.borrow_mut();
                if state.error.is_none() && !state.closed {
                    state.error = Some(args.first().cloned().unwrap_or(Value::Undefined));
                }
                Value::Undefined
            }),
        ),
        ("signal".into(), Value::Undefined),
    ]));
    w3cos_core::class::set_prototype_of(
        &controller,
        &writable_stream_default_controller_class().get_property("prototype"),
    );
    controller
}

fn writable_error(state: &Rc<RefCell<WritableState>>) -> Option<Value> {
    state.borrow().error.clone()
}

fn write_chunk(state: &Rc<RefCell<WritableState>>, chunk: Value) -> Value {
    if let Some(reason) = writable_error(state) {
        return w3cos_core::promise::reject(vec![reason]);
    }
    let (sink, controller, closed) = {
        let state = state.borrow();
        (state.sink.clone(), state.controller.clone(), state.closed)
    };
    if closed {
        return w3cos_core::promise::reject(vec![Value::object(HashMap::from([
            ("name".into(), Value::string("TypeError")),
            ("message".into(), Value::string("WritableStream is closed")),
        ]))]);
    }
    let write = sink.get_property("write");
    let result = if write.is_function() {
        write.call(sink, vec![chunk, controller])
    } else {
        Value::Undefined
    };
    w3cos_core::promise::resolve(vec![result])
}

fn close_writable(state: &Rc<RefCell<WritableState>>) -> Value {
    if let Some(reason) = writable_error(state) {
        return w3cos_core::promise::reject(vec![reason]);
    }
    let (sink, already_closed) = {
        let mut state = state.borrow_mut();
        let already_closed = state.closed;
        state.closed = true;
        (state.sink.clone(), already_closed)
    };
    if already_closed {
        return w3cos_core::promise::reject(vec![Value::object(HashMap::from([
            ("name".into(), Value::string("TypeError")),
            (
                "message".into(),
                Value::string("WritableStream is already closed"),
            ),
        ]))]);
    }
    let close = sink.get_property("close");
    let result = if close.is_function() {
        close.call(sink, vec![])
    } else {
        Value::Undefined
    };
    w3cos_core::promise::resolve(vec![result])
}

fn abort_writable(state: &Rc<RefCell<WritableState>>, reason: Value) -> Value {
    let sink = {
        let mut state = state.borrow_mut();
        if state.closed {
            return w3cos_core::promise::resolve(vec![Value::Undefined]);
        }
        state.closed = true;
        state.error = Some(reason.clone());
        state.sink.clone()
    };
    let abort = sink.get_property("abort");
    let result = if abort.is_function() {
        abort.call(sink, vec![reason])
    } else {
        Value::Undefined
    };
    w3cos_core::promise::resolve(vec![result])
}

fn writer_value(state: Rc<RefCell<WritableState>>) -> Value {
    let state_for_write = Rc::clone(&state);
    let state_for_close = Rc::clone(&state);
    let state_for_abort = Rc::clone(&state);
    let state_for_release = Rc::clone(&state);
    let state_for_size = Rc::clone(&state);
    let writer = Value::object(HashMap::from([
        (
            "write".into(),
            Value::function(move |_, args| {
                write_chunk(
                    &state_for_write,
                    args.first().cloned().unwrap_or(Value::Undefined),
                )
            }),
        ),
        (
            "close".into(),
            Value::function(move |_, _| close_writable(&state_for_close)),
        ),
        (
            "abort".into(),
            Value::function(move |_, args| {
                abort_writable(
                    &state_for_abort,
                    args.first().cloned().unwrap_or(Value::Undefined),
                )
            }),
        ),
        (
            "releaseLock".into(),
            Value::function(move |_, _| {
                state_for_release.borrow_mut().locked = false;
                Value::Undefined
            }),
        ),
        (
            "__w3cos_getter_desiredSize".into(),
            Value::function(move |_, _| {
                if state_for_size.borrow().error.is_some() {
                    Value::Null
                } else {
                    Value::Number(1.0)
                }
            }),
        ),
        (
            "ready".into(),
            w3cos_core::promise::resolve(vec![Value::Undefined]),
        ),
    ]));
    w3cos_core::class::set_prototype_of(
        &writer,
        &writable_stream_default_writer_class().get_property("prototype"),
    );
    writer
}

fn writable_value(sink: Value) -> Value {
    let state = Rc::new(RefCell::new(WritableState {
        sink: sink.clone(),
        controller: Value::Undefined,
        locked: false,
        closed: false,
        error: None,
    }));
    let controller = writable_controller_value(&state);
    state.borrow_mut().controller = controller.clone();
    let state_for_locked = Rc::clone(&state);
    let stream = Value::object(HashMap::from([(
        "__w3cos_getter_locked".into(),
        Value::function(move |_, _| Value::Bool(state_for_locked.borrow().locked)),
    )]));
    let state_for_writer = Rc::clone(&state);
    stream.set_property(
        "getWriter",
        Value::function(move |_, _| {
            {
                let mut state = state_for_writer.borrow_mut();
                if state.locked {
                    type_error("WritableStream is already locked");
                }
                state.locked = true;
            }
            writer_value(Rc::clone(&state_for_writer))
        }),
    );
    let state_for_abort = Rc::clone(&state);
    stream.set_property(
        "abort",
        Value::function(move |_, args| {
            if state_for_abort.borrow().locked {
                type_error("Cannot abort a locked WritableStream");
            }
            abort_writable(
                &state_for_abort,
                args.first().cloned().unwrap_or(Value::Undefined),
            )
        }),
    );
    let state_for_close = Rc::clone(&state);
    stream.set_property(
        "close",
        Value::function(move |_, _| {
            if state_for_close.borrow().locked {
                type_error("Cannot close a locked WritableStream");
            }
            close_writable(&state_for_close)
        }),
    );
    w3cos_core::class::set_prototype_of(
        &stream,
        &writable_stream_class().get_property("prototype"),
    );
    let start = sink.get_property("start");
    if start.is_function() {
        start.call(sink, vec![controller]);
    }
    stream
}

pub fn writable_stream_class() -> Value {
    WRITABLE_STREAM_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            writable_value(
                args.first()
                    .cloned()
                    .unwrap_or_else(|| Value::object(HashMap::new())),
            )
        });
        class.set_property("name", Value::string("WritableStream"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in ["abort", "close", "getWriter"] {
            prototype.set_property(method, Value::function(|_, _| Value::Undefined));
        }
        prototype.set_property("locked", Value::Undefined);
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn writable_stream_default_writer_class() -> Value {
    DEFAULT_WRITER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            args.first()
                .cloned()
                .unwrap_or(Value::Undefined)
                .call_method("getWriter", vec![])
        });
        class.set_property("name", Value::string("WritableStreamDefaultWriter"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in ["abort", "close", "releaseLock", "write"] {
            prototype.set_property(method, Value::function(|_, _| Value::Undefined));
        }
        for property in ["closed", "desiredSize", "ready"] {
            prototype.set_property(property, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn writable_stream_default_controller_class() -> Value {
    WRITABLE_CONTROLLER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            type_error("WritableStreamDefaultController cannot be constructed directly")
        });
        class.set_property("name", Value::string("WritableStreamDefaultController"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("error", Value::function(|_, _| Value::Undefined));
        prototype.set_property("signal", Value::Undefined);
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn transform_stream_default_controller_class() -> Value {
    TRANSFORM_CONTROLLER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            type_error("TransformStreamDefaultController cannot be constructed directly")
        });
        class.set_property("name", Value::string("TransformStreamDefaultController"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in ["enqueue", "error", "terminate"] {
            prototype.set_property(method, Value::function(|_, _| Value::Undefined));
        }
        prototype.set_property("desiredSize", Value::Undefined);
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn transform_stream_class() -> Value {
    TRANSFORM_STREAM_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            let transformer = args
                .first()
                .cloned()
                .unwrap_or_else(|| Value::object(HashMap::new()));
            let readable_controller = Rc::new(RefCell::new(Value::Undefined));
            let controller_for_start = Rc::clone(&readable_controller);
            let readable_source = Value::object(HashMap::from([(
                "start".into(),
                Value::function(move |_, args| {
                    *controller_for_start.borrow_mut() =
                        args.first().cloned().unwrap_or(Value::Undefined);
                    Value::Undefined
                }),
            )]));
            let readable = stream_value(readable_source, Value::Undefined);

            let controller_target = Rc::clone(&readable_controller);
            let transform_controller = Value::object(HashMap::new());
            transform_controller.set_property(
                "enqueue",
                Value::function(move |_, args| {
                    controller_target.borrow().call_method(
                        "enqueue",
                        vec![args.first().cloned().unwrap_or(Value::Undefined)],
                    )
                }),
            );
            let controller_target = Rc::clone(&readable_controller);
            transform_controller.set_property(
                "error",
                Value::function(move |_, args| {
                    controller_target.borrow().call_method(
                        "error",
                        vec![args.first().cloned().unwrap_or(Value::Undefined)],
                    )
                }),
            );
            let controller_target = Rc::clone(&readable_controller);
            transform_controller.set_property(
                "terminate",
                Value::function(move |_, _| {
                    controller_target.borrow().call_method("close", vec![])
                }),
            );
            w3cos_core::class::set_prototype_of(
                &transform_controller,
                &transform_stream_default_controller_class().get_property("prototype"),
            );

            let transformer_for_write = transformer.clone();
            let controller_for_write = transform_controller.clone();
            let sink = Value::object(HashMap::new());
            sink.set_property(
                "write",
                Value::function(move |_, args| {
                    let chunk = args.first().cloned().unwrap_or(Value::Undefined);
                    let transform = transformer_for_write.get_property("transform");
                    if transform.is_function() {
                        transform.call(
                            transformer_for_write.clone(),
                            vec![chunk, controller_for_write.clone()],
                        )
                    } else {
                        controller_for_write.call_method("enqueue", vec![chunk])
                    }
                }),
            );
            let transformer_for_close = transformer;
            let controller_for_close = transform_controller;
            sink.set_property(
                "close",
                Value::function(move |_, _| {
                    let flush = transformer_for_close.get_property("flush");
                    if flush.is_function() {
                        flush.call(
                            transformer_for_close.clone(),
                            vec![controller_for_close.clone()],
                        );
                    }
                    controller_for_close.call_method("terminate", vec![])
                }),
            );
            let writable = writable_value(sink);
            let value = Value::object(HashMap::from([
                ("readable".into(), readable),
                ("writable".into(), writable),
            ]));
            w3cos_core::class::set_prototype_of(
                &value,
                &transform_stream_class().get_property("prototype"),
            );
            value
        });
        class.set_property("name", Value::string("TransformStream"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("readable", Value::Undefined);
        prototype.set_property("writable", Value::Undefined);
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn queuing_strategy_class(byte_length: bool) -> Value {
    let slot = if byte_length {
        &BYTE_LENGTH_QUEUING_STRATEGY_CLASS
    } else {
        &COUNT_QUEUING_STRATEGY_CLASS
    };
    slot.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let name = if byte_length {
            "ByteLengthQueuingStrategy"
        } else {
            "CountQueuingStrategy"
        };
        let class = Value::function(move |_, args| {
            let init = args.first().cloned().unwrap_or(Value::Undefined);
            let high_water_mark = init.get_property("highWaterMark").to_number();
            if !high_water_mark.is_finite() || high_water_mark < 0.0 {
                type_error("QueuingStrategy highWaterMark must be a non-negative number");
            }
            let strategy = Value::object(HashMap::from([(
                "highWaterMark".into(),
                Value::Number(high_water_mark),
            )]));
            strategy.set_property(
                "size",
                Value::function(move |_, args| {
                    if byte_length {
                        Value::Number(
                            args.first()
                                .cloned()
                                .unwrap_or(Value::Undefined)
                                .get_property("byteLength")
                                .to_number(),
                        )
                    } else {
                        Value::Number(1.0)
                    }
                }),
            );
            w3cos_core::class::set_prototype_of(
                &strategy,
                &queuing_strategy_class(byte_length).get_property("prototype"),
            );
            strategy
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("highWaterMark", Value::Undefined);
        prototype.set_property("size", Value::function(|_, _| Value::Undefined));
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn count_queuing_strategy_class() -> Value {
    queuing_strategy_class(false)
}

pub fn byte_length_queuing_strategy_class() -> Value {
    queuing_strategy_class(true)
}

pub fn text_encoder_stream_class() -> Value {
    TEXT_ENCODER_STREAM_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            let encoder =
                w3cos_core::class::construct(&crate::text_encoding::text_encoder_class(), vec![]);
            let transformer = Value::object(HashMap::from([(
                "transform".into(),
                Value::function(move |_, args| {
                    let encoded = encoder.call_method(
                        "encode",
                        vec![Value::string(
                            &args
                                .first()
                                .cloned()
                                .unwrap_or(Value::Undefined)
                                .to_js_string(),
                        )],
                    );
                    args.get(1)
                        .cloned()
                        .unwrap_or(Value::Undefined)
                        .call_method("enqueue", vec![encoded])
                }),
            )]));
            let value = w3cos_core::class::construct(&transform_stream_class(), vec![transformer]);
            value.set_property("encoding", Value::string("utf-8"));
            w3cos_core::class::set_prototype_of(
                &value,
                &text_encoder_stream_class().get_property("prototype"),
            );
            value
        });
        class.set_property("name", Value::string("TextEncoderStream"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in ["encoding", "readable", "writable"] {
            prototype.set_property(property, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn text_decoder_stream_class() -> Value {
    TEXT_DECODER_STREAM_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            let label = args.first().cloned().unwrap_or(Value::Undefined);
            let label = if label.is_undefined() {
                Value::string("utf-8")
            } else {
                label
            };
            let options = args.get(1).cloned().unwrap_or(Value::Undefined);
            let decoder = w3cos_core::class::construct(
                &w3cos_core::web::text_decoder_class(),
                vec![label, options.clone()],
            );
            let encoding = decoder.get_property("encoding");
            let fatal = options.get_property("fatal").to_bool();
            let ignore_bom = options.get_property("ignoreBOM").to_bool();
            let buffered = Rc::new(RefCell::new(Vec::<u8>::new()));
            let buffered_for_transform = Rc::clone(&buffered);
            let decoder_for_flush = decoder.clone();
            let transformer = Value::object(HashMap::from([
                (
                    "transform".into(),
                    Value::function(move |_, args| {
                        let chunk = args.first().cloned().unwrap_or(Value::Undefined);
                        let Some(bytes) = w3cos_core::binary::bytes_of(&chunk) else {
                            args.get(1)
                                .cloned()
                                .unwrap_or(Value::Undefined)
                                .call_method(
                                    "error",
                                    vec![Value::object(HashMap::from([
                                        ("name".into(), Value::string("TypeError")),
                                        (
                                            "message".into(),
                                            Value::string(
                                                "TextDecoderStream chunks must be BufferSource values",
                                            ),
                                        ),
                                    ]))],
                                );
                            return Value::Undefined;
                        };
                        buffered_for_transform.borrow_mut().extend(bytes);
                        Value::Undefined
                    }),
                ),
                (
                    "flush".into(),
                    Value::function(move |_, args| {
                        let bytes = std::mem::take(&mut *buffered.borrow_mut());
                        let chunk = w3cos_core::binary::typed_array_value(
                            bytes
                                .into_iter()
                                .map(|byte| Value::Number(byte as f64))
                                .collect(),
                        );
                        let decoded =
                            decoder_for_flush.call_method("decode", vec![chunk]);
                        if !decoded.to_js_string().is_empty() {
                            args.first()
                                .cloned()
                                .unwrap_or(Value::Undefined)
                                .call_method("enqueue", vec![decoded]);
                        }
                        Value::Undefined
                    }),
                ),
            ]));
            let value =
                w3cos_core::class::construct(&transform_stream_class(), vec![transformer]);
            value.set_property("encoding", encoding);
            value.set_property("fatal", Value::Bool(fatal));
            value.set_property("ignoreBOM", Value::Bool(ignore_bom));
            w3cos_core::class::set_prototype_of(
                &value,
                &text_decoder_stream_class().get_property("prototype"),
            );
            value
        });
        class.set_property("name", Value::string("TextDecoderStream"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in ["encoding", "fatal", "ignoreBOM", "readable", "writable"] {
            prototype.set_property(property, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn codec_bytes(format: &str, bytes: &[u8], decompress: bool) -> Result<Vec<u8>, String> {
    if decompress {
        let mut output = Vec::new();
        match format {
            "gzip" => flate2::read::GzDecoder::new(bytes)
                .read_to_end(&mut output)
                .map_err(|error| error.to_string())?,
            "deflate" => flate2::read::ZlibDecoder::new(bytes)
                .read_to_end(&mut output)
                .map_err(|error| error.to_string())?,
            "deflate-raw" => flate2::read::DeflateDecoder::new(bytes)
                .read_to_end(&mut output)
                .map_err(|error| error.to_string())?,
            _ => unreachable!("format validated by the constructor"),
        };
        return Ok(output);
    }

    match format {
        "gzip" => {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder
                .write_all(bytes)
                .and_then(|_| encoder.finish())
                .map_err(|error| error.to_string())
        }
        "deflate" => {
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            encoder
                .write_all(bytes)
                .and_then(|_| encoder.finish())
                .map_err(|error| error.to_string())
        }
        "deflate-raw" => {
            let mut encoder =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            encoder
                .write_all(bytes)
                .and_then(|_| encoder.finish())
                .map_err(|error| error.to_string())
        }
        _ => unreachable!("format validated by the constructor"),
    }
}

fn compression_stream_class_inner(decompress: bool) -> Value {
    let slot = if decompress {
        &DECOMPRESSION_STREAM_CLASS
    } else {
        &COMPRESSION_STREAM_CLASS
    };
    slot.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class_name = if decompress {
            "DecompressionStream"
        } else {
            "CompressionStream"
        };
        let class = Value::function(move |_, args| {
            let format = args
                .first()
                .cloned()
                .unwrap_or(Value::Undefined)
                .to_js_string();
            if !matches!(format.as_str(), "gzip" | "deflate" | "deflate-raw") {
                type_error(&format!(
                    "{class_name} format must be \"gzip\", \"deflate\", or \"deflate-raw\""
                ));
            }
            COMPRESSION_BUFFERING_WARNING_EMITTED.with(|warned| {
                if !warned.replace(true) {
                    eprintln!(
                        "[w3cos] warning: CompressionStream and DecompressionStream currently \
                         buffer input until close; incremental output and exact backpressure \
                         remain pending"
                    );
                }
            });

            let buffered = Rc::new(RefCell::new(Vec::<u8>::new()));
            let buffered_for_transform = Rc::clone(&buffered);
            let format_for_flush = format.clone();
            let transformer = Value::object(HashMap::from([
                (
                    "transform".into(),
                    Value::function(move |_, args| {
                        let chunk = args.first().cloned().unwrap_or(Value::Undefined);
                        let Some(bytes) = w3cos_core::binary::bytes_of(&chunk) else {
                            args.get(1)
                                .cloned()
                                .unwrap_or(Value::Undefined)
                                .call_method(
                                    "error",
                                    vec![Value::object(HashMap::from([
                                        ("name".into(), Value::string("TypeError")),
                                        (
                                            "message".into(),
                                            Value::string(
                                                "Compression stream chunks must be BufferSource values",
                                            ),
                                        ),
                                    ]))],
                                );
                            return Value::Undefined;
                        };
                        buffered_for_transform.borrow_mut().extend(bytes);
                        Value::Undefined
                    }),
                ),
                (
                    "flush".into(),
                    Value::function(move |_, args| {
                        let bytes = std::mem::take(&mut *buffered.borrow_mut());
                        let controller = args.first().cloned().unwrap_or(Value::Undefined);
                        match codec_bytes(&format_for_flush, &bytes, decompress) {
                            Ok(output) if !output.is_empty() => {
                                controller.call_method(
                                    "enqueue",
                                    vec![w3cos_core::binary::typed_array_value(
                                        output
                                            .into_iter()
                                            .map(|byte| Value::Number(byte as f64))
                                            .collect(),
                                    )],
                                );
                            }
                            Ok(_) => {}
                            Err(message) => {
                                controller.call_method(
                                    "error",
                                    vec![Value::object(HashMap::from([
                                        ("name".into(), Value::string("TypeError")),
                                        ("message".into(), Value::string(&message)),
                                    ]))],
                                );
                            }
                        }
                        Value::Undefined
                    }),
                ),
            ]));
            let value =
                w3cos_core::class::construct(&transform_stream_class(), vec![transformer]);
            value.set_property("format", Value::string(&format));
            w3cos_core::class::set_prototype_of(
                &value,
                &compression_stream_class_inner(decompress).get_property("prototype"),
            );
            value
        });
        class.set_property("name", Value::string(class_name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("readable", Value::Undefined);
        prototype.set_property("writable", Value::Undefined);
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn compression_stream_class() -> Value {
    compression_stream_class_inner(false)
}

pub fn decompression_stream_class() -> Value {
    compression_stream_class_inner(true)
}

pub fn from_bytes(bytes: Vec<u8>, on_disturb: Value) -> Value {
    let chunk = w3cos_core::binary::typed_array_value(
        bytes
            .into_iter()
            .map(|byte| Value::Number(byte as f64))
            .collect(),
    );
    let source = Value::object(HashMap::new());
    source.set_property(
        "start",
        Value::function(move |_, args| {
            let controller = args.first().cloned().unwrap_or(Value::Undefined);
            if chunk.get_property("length").to_number() > 0.0 {
                controller.call_method("enqueue", vec![chunk.clone()]);
            }
            controller.call_method("close", vec![]);
            Value::Undefined
        }),
    );
    stream_value(source, on_disturb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn readable_stream_enqueues_reads_closes_locks_and_disturbs() {
        let disturbed = Rc::new(Cell::new(false));
        let disturbed_for_callback = Rc::clone(&disturbed);
        let stream = from_bytes(
            b"abc".to_vec(),
            Value::function(move |_, _| {
                disturbed_for_callback.set(true);
                Value::Undefined
            }),
        );
        assert!(w3cos_core::class::instance_of(
            &stream,
            &readable_stream_class()
        ));
        assert!(!stream.get_property("locked").to_bool());
        let reader = stream.call_method("getReader", vec![]);
        assert!(stream.get_property("locked").to_bool());
        assert!(w3cos_core::class::instance_of(
            &reader,
            &readable_stream_default_reader_class()
        ));
        let result = Rc::new(RefCell::new(Value::Undefined));
        let result_for_callback = Rc::clone(&result);
        reader.call_method("read", vec![]).call_method(
            "then",
            vec![Value::function(move |_, args| {
                *result_for_callback.borrow_mut() =
                    args.first().cloned().unwrap_or(Value::Undefined);
                Value::Undefined
            })],
        );
        w3cos_core::promise::drain_microtasks();
        assert!(disturbed.get());
        assert!(!result.borrow().get_property("done").to_bool());
        assert_eq!(
            result
                .borrow()
                .get_property("value")
                .get_property("0")
                .to_number(),
            97.0
        );
        let done = Rc::new(Cell::new(false));
        let done_for_callback = Rc::clone(&done);
        reader.call_method("read", vec![]).call_method(
            "then",
            vec![Value::function(move |_, args| {
                done_for_callback.set(args[0].get_property("done").to_bool());
                Value::Undefined
            })],
        );
        w3cos_core::promise::drain_microtasks();
        assert!(done.get());
        reader.call_method("releaseLock", vec![]);
        assert!(!stream.get_property("locked").to_bool());
    }

    #[test]
    fn writable_and_transform_streams_form_a_read_write_pipeline() {
        let transform = w3cos_core::class::construct(
            &transform_stream_class(),
            vec![Value::object(HashMap::from([(
                "transform".into(),
                Value::function(|_, args| {
                    args[1].call_method(
                        "enqueue",
                        vec![Value::string(&args[0].to_js_string().to_uppercase())],
                    )
                }),
            )]))],
        );
        let writer = transform
            .get_property("writable")
            .call_method("getWriter", vec![]);
        writer.call_method("write", vec![Value::string("hello")]);
        writer.call_method("close", vec![]);
        assert!(w3cos_core::class::instance_of(
            &transform,
            &transform_stream_class()
        ));
        assert!(w3cos_core::class::instance_of(
            &transform.get_property("writable"),
            &writable_stream_class()
        ));
        let reader = transform
            .get_property("readable")
            .call_method("getReader", vec![]);
        let result = Rc::new(RefCell::new(Value::Undefined));
        let result_for_callback = Rc::clone(&result);
        reader.call_method("read", vec![]).call_method(
            "then",
            vec![Value::function(move |_, args| {
                *result_for_callback.borrow_mut() =
                    args.first().cloned().unwrap_or(Value::Undefined);
                Value::Undefined
            })],
        );
        w3cos_core::promise::drain_microtasks();
        assert_eq!(
            result.borrow().get_property("value"),
            Value::string("HELLO")
        );
    }

    #[test]
    fn readable_stream_pipe_to_and_tee_move_all_chunks() {
        let source = readable_stream_class().call_method(
            "from",
            vec![Value::array(vec![Value::string("a"), Value::string("b")])],
        );
        let written = Rc::new(RefCell::new(Vec::new()));
        let written_for_sink = Rc::clone(&written);
        let destination = w3cos_core::class::construct(
            &writable_stream_class(),
            vec![Value::object(HashMap::from([(
                "write".into(),
                Value::function(move |_, args| {
                    written_for_sink.borrow_mut().push(args[0].to_js_string());
                    Value::Undefined
                }),
            )]))],
        );
        let completed = Rc::new(Cell::new(false));
        let completed_for_callback = Rc::clone(&completed);
        source.call_method("pipeTo", vec![destination]).call_method(
            "then",
            vec![Value::function(move |_, _| {
                completed_for_callback.set(true);
                Value::Undefined
            })],
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(written.borrow().as_slice(), &["a", "b"]);
        assert!(completed.get());

        let source = readable_stream_class()
            .call_method("from", vec![Value::array(vec![Value::string("shared")])]);
        let branches = source.call_method("tee", vec![]);
        crate::jsdom::drain_microtasks();
        for index in 0..2 {
            let result = Rc::new(RefCell::new(Value::Undefined));
            let result_for_callback = Rc::clone(&result);
            branches
                .get_property(&index.to_string())
                .call_method("getReader", vec![])
                .call_method("read", vec![])
                .call_method(
                    "then",
                    vec![Value::function(move |_, args| {
                        *result_for_callback.borrow_mut() = args[0].get_property("value");
                        Value::Undefined
                    })],
                );
            w3cos_core::promise::drain_microtasks();
            assert_eq!(*result.borrow(), Value::string("shared"));
        }
    }

    #[test]
    fn tee_waits_for_both_branch_cancellations_and_combines_reasons() {
        let original_cancel_reason = Rc::new(RefCell::new(String::new()));
        let reason_for_source = Rc::clone(&original_cancel_reason);
        let source = Value::object(HashMap::from([(
            "cancel".into(),
            Value::function(move |_, args| {
                *reason_for_source.borrow_mut() = args[0].to_js_string();
                Value::Undefined
            }),
        )]));
        let stream = w3cos_core::class::construct(&readable_stream_class(), vec![source]);
        let branches = stream.call_method("tee", vec![]);
        let first_done = Rc::new(Cell::new(false));
        let first_done_for_handler = Rc::clone(&first_done);
        branches
            .get_property("0")
            .call_method("cancel", vec![Value::string("left")])
            .call_method(
                "then",
                vec![Value::function(move |_, _| {
                    first_done_for_handler.set(true);
                    Value::Undefined
                })],
            );
        crate::jsdom::drain_microtasks();
        assert!(!first_done.get());
        assert!(original_cancel_reason.borrow().is_empty());

        let second_done = Rc::new(Cell::new(false));
        let second_done_for_handler = Rc::clone(&second_done);
        branches
            .get_property("1")
            .call_method("cancel", vec![Value::string("right")])
            .call_method(
                "then",
                vec![Value::function(move |_, _| {
                    second_done_for_handler.set(true);
                    Value::Undefined
                })],
            );
        crate::jsdom::drain_microtasks();
        assert!(first_done.get());
        assert!(second_done.get());
        assert_eq!(&*original_cancel_reason.borrow(), "left,right");
        assert!(!stream.get_property("locked").to_bool());
    }

    #[test]
    fn readable_stream_async_iterator_reads_returns_and_releases_locks() {
        let stream = readable_stream_class()
            .call_method("from", vec![Value::array(vec![Value::string("chunk")])]);
        let iterator = stream.call_method("__w3cos_symbol_asyncIterator", vec![]);
        assert_eq!(
            iterator.call_method("__w3cos_symbol_asyncIterator", vec![]),
            iterator
        );
        assert!(stream.get_property("locked").to_bool());

        let results = Rc::new(RefCell::new(Vec::new()));
        for _ in 0..2 {
            let results_for_handler = Rc::clone(&results);
            iterator.call_method("next", vec![]).call_method(
                "then",
                vec![Value::function(move |_, args| {
                    results_for_handler.borrow_mut().push(format!(
                        "{}:{}",
                        args[0].get_property("done").to_bool(),
                        args[0].get_property("value").to_js_string()
                    ));
                    Value::Undefined
                })],
            );
            crate::jsdom::drain_microtasks();
        }
        assert_eq!(&*results.borrow(), &["false:chunk", "true:undefined"]);
        assert!(!stream.get_property("locked").to_bool());

        let cancel_reason = Rc::new(RefCell::new(String::new()));
        let cancel_reason_for_source = Rc::clone(&cancel_reason);
        let source = Value::object(HashMap::from([(
            "cancel".into(),
            Value::function(move |_, args| {
                *cancel_reason_for_source.borrow_mut() = args[0].to_js_string();
                Value::Undefined
            }),
        )]));
        let stream = w3cos_core::class::construct(&readable_stream_class(), vec![source]);
        let iterator = stream.call_method("values", vec![]);
        iterator.call_method("return", vec![Value::string("stop")]);
        crate::jsdom::drain_microtasks();
        assert_eq!(&*cancel_reason.borrow(), "stop");
        assert!(!stream.get_property("locked").to_bool());

        let cancels = Rc::new(Cell::new(0));
        let cancels_for_source = Rc::clone(&cancels);
        let source = Value::object(HashMap::from([(
            "cancel".into(),
            Value::function(move |_, _| {
                cancels_for_source.set(cancels_for_source.get() + 1);
                Value::Undefined
            }),
        )]));
        let stream = w3cos_core::class::construct(&readable_stream_class(), vec![source]);
        let iterator = stream.call_method(
            "values",
            vec![Value::object(HashMap::from([(
                "preventCancel".into(),
                Value::Bool(true),
            )]))],
        );
        iterator.call_method("return", vec![Value::string("stop")]);
        crate::jsdom::drain_microtasks();
        assert_eq!(cancels.get(), 0);
        assert!(!stream.get_property("locked").to_bool());
    }

    #[test]
    fn pipe_to_applies_prevent_options_and_abort_signal() {
        let closes = Rc::new(Cell::new(0));
        let closes_for_sink = Rc::clone(&closes);
        let destination = w3cos_core::class::construct(
            &writable_stream_class(),
            vec![Value::object(HashMap::from([(
                "close".into(),
                Value::function(move |_, _| {
                    closes_for_sink.set(closes_for_sink.get() + 1);
                    Value::Undefined
                }),
            )]))],
        );
        readable_stream_class()
            .call_method("from", vec![Value::array(vec![Value::string("x")])])
            .call_method(
                "pipeTo",
                vec![
                    destination,
                    Value::object(HashMap::from([("preventClose".into(), Value::Bool(true))])),
                ],
            );
        crate::jsdom::drain_microtasks();
        assert_eq!(closes.get(), 0);

        let cancels = Rc::new(Cell::new(0));
        let cancels_for_source = Rc::clone(&cancels);
        let source = Value::object(HashMap::new());
        source.set_property(
            "start",
            Value::function(|_, args| {
                args[0].call_method("enqueue", vec![Value::string("x")]);
                Value::Undefined
            }),
        );
        source.set_property(
            "cancel",
            Value::function(move |_, _| {
                cancels_for_source.set(cancels_for_source.get() + 1);
                Value::Undefined
            }),
        );
        let destination = w3cos_core::class::construct(
            &writable_stream_class(),
            vec![Value::object(HashMap::from([(
                "write".into(),
                Value::function(|_, _| {
                    w3cos_core::promise::reject(vec![Value::string("destination-error")])
                }),
            )]))],
        );
        let rejection = Rc::new(RefCell::new(String::new()));
        let rejection_for_handler = Rc::clone(&rejection);
        w3cos_core::class::construct(&readable_stream_class(), vec![source])
            .call_method(
                "pipeTo",
                vec![
                    destination,
                    Value::object(HashMap::from([("preventCancel".into(), Value::Bool(true))])),
                ],
            )
            .call_method(
                "catch",
                vec![Value::function(move |_, args| {
                    *rejection_for_handler.borrow_mut() = args[0].to_js_string();
                    Value::Undefined
                })],
            );
        crate::jsdom::drain_microtasks();
        assert_eq!(cancels.get(), 0);
        assert_eq!(&*rejection.borrow(), "destination-error");

        let aborts = Rc::new(Cell::new(0));
        let source = Value::object(HashMap::from([(
            "start".into(),
            Value::function(|_, args| {
                args[0].call_method("error", vec![Value::string("source-error")]);
                Value::Undefined
            }),
        )]));
        let aborts_for_sink = Rc::clone(&aborts);
        let destination = w3cos_core::class::construct(
            &writable_stream_class(),
            vec![Value::object(HashMap::from([(
                "abort".into(),
                Value::function(move |_, _| {
                    aborts_for_sink.set(aborts_for_sink.get() + 1);
                    Value::Undefined
                }),
            )]))],
        );
        w3cos_core::class::construct(&readable_stream_class(), vec![source]).call_method(
            "pipeTo",
            vec![
                destination,
                Value::object(HashMap::from([("preventAbort".into(), Value::Bool(true))])),
            ],
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(aborts.get(), 0);

        let aborts = Rc::new(Cell::new(0));
        let cancels = Rc::new(Cell::new(0));
        let cancels_for_source = Rc::clone(&cancels);
        let source = Value::object(HashMap::from([(
            "cancel".into(),
            Value::function(move |_, _| {
                cancels_for_source.set(cancels_for_source.get() + 1);
                Value::Undefined
            }),
        )]));
        let aborts_for_sink = Rc::clone(&aborts);
        let destination = w3cos_core::class::construct(
            &writable_stream_class(),
            vec![Value::object(HashMap::from([(
                "abort".into(),
                Value::function(move |_, _| {
                    aborts_for_sink.set(aborts_for_sink.get() + 1);
                    Value::Undefined
                }),
            )]))],
        );
        let controller =
            w3cos_core::class::construct(&crate::fetch::abort_controller_class(), vec![]);
        controller.call_method("abort", vec![Value::string("stopped")]);
        w3cos_core::class::construct(&readable_stream_class(), vec![source]).call_method(
            "pipeTo",
            vec![
                destination,
                Value::object(HashMap::from([(
                    "signal".into(),
                    controller.get_property("signal"),
                )])),
            ],
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(aborts.get(), 1);
        assert_eq!(cancels.get(), 1);
    }

    #[test]
    fn queuing_strategies_report_count_and_byte_length_sizes() {
        let count = w3cos_core::class::construct(
            &count_queuing_strategy_class(),
            vec![Value::object(HashMap::from([(
                "highWaterMark".into(),
                Value::Number(3.0),
            )]))],
        );
        let bytes = w3cos_core::class::construct(
            &byte_length_queuing_strategy_class(),
            vec![Value::object(HashMap::from([(
                "highWaterMark".into(),
                Value::Number(16.0),
            )]))],
        );
        let chunk =
            w3cos_core::binary::typed_array_value(vec![Value::Number(1.0), Value::Number(2.0)]);
        assert_eq!(
            count.call_method("size", vec![chunk.clone()]),
            Value::Number(1.0)
        );
        assert_eq!(bytes.call_method("size", vec![chunk]), Value::Number(2.0));
        assert_eq!(count.get_property("highWaterMark"), Value::Number(3.0));
        assert!(w3cos_core::class::instance_of(
            &bytes,
            &byte_length_queuing_strategy_class()
        ));
    }

    #[test]
    fn text_encoder_and_decoder_streams_roundtrip_split_utf8() {
        let encoder = w3cos_core::class::construct(&text_encoder_stream_class(), vec![]);
        let encoder_writer = encoder
            .get_property("writable")
            .call_method("getWriter", vec![]);
        encoder_writer.call_method("write", vec![Value::string("A✓")]);
        encoder_writer.call_method("close", vec![]);
        let encoded = Rc::new(RefCell::new(Value::Undefined));
        let encoded_for_callback = Rc::clone(&encoded);
        encoder
            .get_property("readable")
            .call_method("getReader", vec![])
            .call_method("read", vec![])
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *encoded_for_callback.borrow_mut() = args[0].get_property("value");
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        assert_eq!(encoded.borrow().get_property("length"), Value::Number(4.0));

        let decoder = w3cos_core::class::construct(&text_decoder_stream_class(), vec![]);
        let writer = decoder
            .get_property("writable")
            .call_method("getWriter", vec![]);
        writer.call_method(
            "write",
            vec![w3cos_core::binary::typed_array_value(vec![
                Value::Number(0xe2 as f64),
                Value::Number(0x9c as f64),
            ])],
        );
        writer.call_method(
            "write",
            vec![w3cos_core::binary::typed_array_value(vec![Value::Number(
                0x93 as f64,
            )])],
        );
        writer.call_method("close", vec![]);
        let decoded = Rc::new(RefCell::new(Value::Undefined));
        let decoded_for_callback = Rc::clone(&decoded);
        decoder
            .get_property("readable")
            .call_method("getReader", vec![])
            .call_method("read", vec![])
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *decoded_for_callback.borrow_mut() = args[0].get_property("value");
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        assert_eq!(*decoded.borrow(), Value::string("✓"));
    }

    #[test]
    fn compression_and_decompression_streams_roundtrip_gzip() {
        let compressor =
            w3cos_core::class::construct(&compression_stream_class(), vec![Value::string("gzip")]);
        let writer = compressor
            .get_property("writable")
            .call_method("getWriter", vec![]);
        writer.call_method(
            "write",
            vec![w3cos_core::binary::typed_array_value(
                b"hello "
                    .iter()
                    .map(|byte| Value::Number(*byte as f64))
                    .collect(),
            )],
        );
        writer.call_method(
            "write",
            vec![w3cos_core::binary::typed_array_value(
                b"streams"
                    .iter()
                    .map(|byte| Value::Number(*byte as f64))
                    .collect(),
            )],
        );
        writer.call_method("close", vec![]);
        let compressed = Rc::new(RefCell::new(Value::Undefined));
        let compressed_for_callback = Rc::clone(&compressed);
        compressor
            .get_property("readable")
            .call_method("getReader", vec![])
            .call_method("read", vec![])
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *compressed_for_callback.borrow_mut() = args[0].get_property("value");
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        assert!(
            compressed.borrow().get_property("length").to_number() > 0.0,
            "gzip output should contain a header and compressed payload"
        );

        let decompressor = w3cos_core::class::construct(
            &decompression_stream_class(),
            vec![Value::string("gzip")],
        );
        let decompressor_writer = decompressor
            .get_property("writable")
            .call_method("getWriter", vec![]);
        decompressor_writer.call_method("write", vec![compressed.borrow().clone()]);
        decompressor_writer.call_method("close", vec![]);
        let decoded = Rc::new(RefCell::new(Value::Undefined));
        let decoded_for_callback = Rc::clone(&decoded);
        decompressor
            .get_property("readable")
            .call_method("getReader", vec![])
            .call_method("read", vec![])
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *decoded_for_callback.borrow_mut() = args[0].get_property("value");
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        let decoded_bytes = w3cos_core::binary::bytes_of(&decoded.borrow())
            .expect("decompression should emit a Uint8Array");
        assert_eq!(decoded_bytes, b"hello streams");
    }

    #[test]
    fn byte_streams_expose_a_compatible_byob_reader() {
        let source = Value::object(HashMap::from([("type".into(), Value::string("bytes"))]));
        source.set_property(
            "start",
            Value::function(|_, args| {
                args[0].call_method(
                    "enqueue",
                    vec![w3cos_core::binary::typed_array_value(vec![
                        Value::Number(1.0),
                        Value::Number(2.0),
                    ])],
                );
                args[0].call_method("close", Vec::new());
                Value::Undefined
            }),
        );
        let stream = w3cos_core::class::construct(&readable_stream_class(), vec![source]);
        let reader = stream.call_method(
            "getReader",
            vec![Value::object(HashMap::from([(
                "mode".into(),
                Value::string("byob"),
            )]))],
        );
        assert!(w3cos_core::class::instance_of(
            &reader,
            &readable_stream_byob_reader_class()
        ));
        let result = Rc::new(RefCell::new(Value::Undefined));
        let result_for_callback = Rc::clone(&result);
        reader
            .call_method(
                "read",
                vec![w3cos_core::binary::typed_array_value(vec![
                    Value::Number(0.0),
                    Value::Number(0.0),
                ])],
            )
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *result_for_callback.borrow_mut() = args[0].get_property("value");
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        assert_eq!(
            w3cos_core::binary::bytes_of(&result.borrow()).unwrap(),
            vec![1, 2]
        );
    }
}
