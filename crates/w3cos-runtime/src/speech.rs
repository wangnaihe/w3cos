//! Web Speech API-compatible speech recognition bridge.
//!
//! The public surface follows `SpeechRecognition` semantics (`lang`,
//! `continuous`, `interimResults`, `processLocally`, transcript, confidence,
//! and finality). Platform engines remain implementation details.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeechRecognitionOptions {
    pub lang: String,
    pub process_locally: bool,
    pub continuous: bool,
    pub interim_results: bool,
}

#[derive(Clone, Debug)]
pub struct SpeechRecognitionBinding {
    pub transcript_signal: usize,
    pub final_signal: usize,
    pub confidence_signal: usize,
    pub status_signal: usize,
    pub options: SpeechRecognitionOptions,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpeechRecognitionEvent {
    Start,
    Result {
        transcript: String,
        is_final: bool,
        confidence_percent: i64,
    },
    End,
    Error {
        code: &'static str,
        message: String,
    },
}

static BINDING: OnceLock<Mutex<Option<SpeechRecognitionBinding>>> = OnceLock::new();
static EVENTS: OnceLock<Mutex<VecDeque<SpeechRecognitionEvent>>> = OnceLock::new();

fn binding() -> &'static Mutex<Option<SpeechRecognitionBinding>> {
    BINDING.get_or_init(|| Mutex::new(None))
}

fn events() -> &'static Mutex<VecDeque<SpeechRecognitionEvent>> {
    EVENTS.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn push_event(event: SpeechRecognitionEvent) {
    if let Ok(mut pending) = events().lock() {
        pending.push_back(event);
    }
}

pub fn start(next: SpeechRecognitionBinding) {
    platform::stop();
    *binding().lock().expect("speech binding mutex poisoned") = None;
    events()
        .lock()
        .expect("speech event mutex poisoned")
        .clear();
    crate::state::set_text_signal(next.transcript_signal, "");
    crate::state::set_signal(next.final_signal, 0);
    crate::state::set_signal(next.confidence_signal, 0);
    crate::state::set_signal(next.status_signal, 1); // requesting permission
    let options = next.options.clone();
    *binding().lock().expect("speech binding mutex poisoned") = Some(next);
    platform::request_start(options);
}

pub fn stop() {
    platform::stop();
    if binding().lock().is_ok_and(|active| active.is_some()) {
        push_event(SpeechRecognitionEvent::End);
    }
}

pub fn is_active() -> bool {
    binding().lock().is_ok_and(|active| active.is_some())
}

pub fn next_deadline() -> Option<Instant> {
    is_active().then(|| Instant::now() + Duration::from_millis(50))
}

/// Poll platform authorization/results on the runtime's main event-loop thread.
/// Returns true when reactive state changed.
pub fn poll() -> bool {
    platform::poll_start();
    let Some(active) = binding().lock().ok().and_then(|active| active.clone()) else {
        return false;
    };
    let mut changed = false;
    let mut terminal = false;
    let mut pending = events().lock().expect("speech event mutex poisoned");
    while let Some(event) = pending.pop_front() {
        changed = true;
        match event {
            SpeechRecognitionEvent::Start => {
                crate::state::set_signal(active.status_signal, 2); // listening
            }
            SpeechRecognitionEvent::Result {
                transcript,
                is_final,
                confidence_percent,
            } => {
                crate::state::set_text_signal(active.transcript_signal, transcript);
                crate::state::set_signal(active.final_signal, i64::from(is_final));
                crate::state::set_signal(
                    active.confidence_signal,
                    confidence_percent.clamp(0, 100),
                );
            }
            SpeechRecognitionEvent::End => {
                crate::state::set_signal(active.status_signal, 3);
                terminal = true;
            }
            SpeechRecognitionEvent::Error { code, message } => {
                crate::state::set_text_signal(
                    active.transcript_signal,
                    if message.is_empty() {
                        code.to_string()
                    } else {
                        message
                    },
                );
                crate::state::set_signal(active.status_signal, error_status(code));
                terminal = true;
            }
        }
    }
    drop(pending);
    if terminal {
        platform::stop();
        *binding().lock().expect("speech binding mutex poisoned") = None;
    }
    changed
}

fn error_status(code: &str) -> i64 {
    match code {
        "not-allowed" => -2,
        "language-not-supported" => -3,
        "service-not-allowed" => -4,
        "audio-capture" => -5,
        "no-speech" => -6,
        _ => -1,
    }
}

#[cfg(target_os = "ios")]
mod platform {
    use super::{SpeechRecognitionEvent, SpeechRecognitionOptions, push_event};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use std::cell::RefCell;
    use std::ffi::{CStr, CString};
    use std::sync::atomic::{AtomicI64, Ordering};

    #[link(name = "Speech", kind = "framework")]
    unsafe extern "C" {}
    #[link(name = "AVFoundation", kind = "framework")]
    unsafe extern "C" {}

    // SFSpeechRecognizerAuthorizationStatus values.
    const AUTH_NOT_DETERMINED: i64 = 0;
    const AUTH_AUTHORIZED: i64 = 3;
    const AUTH_PENDING: i64 = -1;
    const MIC_PENDING: i64 = 0;
    const MIC_GRANTED: i64 = 1;
    const MIC_DENIED: i64 = 2;
    const AV_RECORD_PERMISSION_DENIED: u32 = 1_684_369_017; // 'deny'
    const AV_RECORD_PERMISSION_GRANTED: u32 = 1_735_552_628; // 'grnt'

    static SPEECH_AUTH: AtomicI64 = AtomicI64::new(AUTH_PENDING);
    static MIC_AUTH: AtomicI64 = AtomicI64::new(MIC_PENDING);

    thread_local! {
        static PENDING: RefCell<Option<SpeechRecognitionOptions>> = const { RefCell::new(None) };
        static SESSION: RefCell<Option<IosSpeechSession>> = const { RefCell::new(None) };
    }

    struct IosSpeechSession {
        _recognizer: Retained<AnyObject>,
        engine: Retained<AnyObject>,
        input_node: Retained<AnyObject>,
        request: Retained<AnyObject>,
        task: Retained<AnyObject>,
    }

    fn ns_string(value: &str) -> Option<*mut AnyObject> {
        let value = CString::new(value).ok()?;
        let class = AnyClass::get("NSString")?;
        let string: *mut AnyObject =
            unsafe { objc2::msg_send![class, stringWithUTF8String: value.as_ptr()] };
        (!string.is_null()).then_some(string)
    }

    fn rust_string(value: *mut AnyObject) -> String {
        if value.is_null() {
            return String::new();
        }
        let bytes: *const std::ffi::c_char = unsafe { objc2::msg_send![&*value, UTF8String] };
        if bytes.is_null() {
            return String::new();
        }
        unsafe { CStr::from_ptr(bytes) }
            .to_string_lossy()
            .into_owned()
    }

    fn error_message(error: *mut AnyObject) -> String {
        if error.is_null() {
            return String::new();
        }
        let description: *mut AnyObject =
            unsafe { objc2::msg_send![&*error, localizedDescription] };
        rust_string(description)
    }

    pub(super) fn request_start(options: SpeechRecognitionOptions) {
        PENDING.with(|pending| *pending.borrow_mut() = Some(options));
        SPEECH_AUTH.store(AUTH_PENDING, Ordering::SeqCst);
        MIC_AUTH.store(MIC_PENDING, Ordering::SeqCst);

        let Some(speech_class) = AnyClass::get("SFSpeechRecognizer") else {
            push_event(SpeechRecognitionEvent::Error {
                code: "service-not-allowed",
                message: "iOS Speech framework unavailable".into(),
            });
            return;
        };
        let current: isize = unsafe { objc2::msg_send![speech_class, authorizationStatus] };
        if current as i64 == AUTH_AUTHORIZED {
            SPEECH_AUTH.store(AUTH_AUTHORIZED, Ordering::SeqCst);
        } else if current as i64 == AUTH_NOT_DETERMINED {
            let block = block2_05::RcBlock::new(|status: isize| {
                SPEECH_AUTH.store(status as i64, Ordering::SeqCst);
            });
            let _: () = unsafe { objc2::msg_send![speech_class, requestAuthorization: &*block] };
        } else {
            SPEECH_AUTH.store(current as i64, Ordering::SeqCst);
        }

        let Some(audio_class) = AnyClass::get("AVAudioSession") else {
            MIC_AUTH.store(MIC_DENIED, Ordering::SeqCst);
            return;
        };
        let audio_session: *mut AnyObject =
            unsafe { objc2::msg_send![audio_class, sharedInstance] };
        if audio_session.is_null() {
            MIC_AUTH.store(MIC_DENIED, Ordering::SeqCst);
            return;
        }
        let block = block2_05::RcBlock::new(|granted: i8| {
            MIC_AUTH.store(
                if granted != 0 {
                    MIC_GRANTED
                } else {
                    MIC_DENIED
                },
                Ordering::SeqCst,
            );
        });
        let _: () = unsafe { objc2::msg_send![&*audio_session, requestRecordPermission: &*block] };
    }

    pub(super) fn poll_start() {
        let Some(options) = PENDING.with(|pending| pending.borrow().clone()) else {
            return;
        };
        refresh_authorization_status();
        let speech_auth = SPEECH_AUTH.load(Ordering::SeqCst);
        let mic_auth = MIC_AUTH.load(Ordering::SeqCst);
        if speech_auth != AUTH_AUTHORIZED {
            if speech_auth != AUTH_PENDING && speech_auth != AUTH_NOT_DETERMINED {
                PENDING.with(|pending| pending.borrow_mut().take());
                push_event(SpeechRecognitionEvent::Error {
                    code: "not-allowed",
                    message: "语音识别权限未授权".into(),
                });
            }
            return;
        }
        if mic_auth != MIC_GRANTED {
            if mic_auth == MIC_DENIED {
                PENDING.with(|pending| pending.borrow_mut().take());
                push_event(SpeechRecognitionEvent::Error {
                    code: "not-allowed",
                    message: "麦克风权限未授权".into(),
                });
            }
            return;
        }
        PENDING.with(|pending| pending.borrow_mut().take());
        if let Err((code, message)) = start_engine(&options) {
            push_event(SpeechRecognitionEvent::Error { code, message });
        }
    }

    fn refresh_authorization_status() {
        if let Some(speech_class) = AnyClass::get("SFSpeechRecognizer") {
            let current: isize = unsafe { objc2::msg_send![speech_class, authorizationStatus] };
            if current as i64 != AUTH_NOT_DETERMINED {
                SPEECH_AUTH.store(current as i64, Ordering::SeqCst);
            }
        }
        if let Some(audio_class) = AnyClass::get("AVAudioSession") {
            let session: *mut AnyObject = unsafe { objc2::msg_send![audio_class, sharedInstance] };
            if !session.is_null() {
                let permission: u32 = unsafe { objc2::msg_send![&*session, recordPermission] };
                match permission {
                    AV_RECORD_PERMISSION_GRANTED => MIC_AUTH.store(MIC_GRANTED, Ordering::SeqCst),
                    AV_RECORD_PERMISSION_DENIED => MIC_AUTH.store(MIC_DENIED, Ordering::SeqCst),
                    _ => {}
                }
            }
        }
    }

    fn start_engine(options: &SpeechRecognitionOptions) -> Result<(), (&'static str, String)> {
        configure_audio_session()?;
        let locale_class = AnyClass::get("NSLocale")
            .ok_or(("language-not-supported", "NSLocale unavailable".into()))?;
        let lang = ns_string(&options.lang)
            .ok_or(("language-not-supported", "invalid language tag".into()))?;
        let locale: *mut AnyObject =
            unsafe { objc2::msg_send![locale_class, localeWithLocaleIdentifier: &*lang] };
        let recognizer_class = AnyClass::get("SFSpeechRecognizer").ok_or((
            "service-not-allowed",
            "SFSpeechRecognizer unavailable".into(),
        ))?;
        let recognizer: *mut AnyObject = unsafe { objc2::msg_send![recognizer_class, alloc] };
        let recognizer: *mut AnyObject =
            unsafe { objc2::msg_send![&*recognizer, initWithLocale: &*locale] };
        let recognizer = unsafe { Retained::from_raw(recognizer) }.ok_or_else(|| {
            (
                "language-not-supported",
                format!("{} is not supported", options.lang),
            )
        })?;
        let available: bool = unsafe { objc2::msg_send![&*recognizer, isAvailable] };
        if !available {
            return Err((
                "service-not-allowed",
                "speech recognizer unavailable".into(),
            ));
        }
        let supports_local: bool =
            unsafe { objc2::msg_send![&*recognizer, supportsOnDeviceRecognition] };
        if options.process_locally && !supports_local {
            return Err((
                "service-not-allowed",
                format!("{} 端侧语音模型不可用", options.lang),
            ));
        }

        let request_class = AnyClass::get("SFSpeechAudioBufferRecognitionRequest")
            .ok_or(("service-not-allowed", "speech request unavailable".into()))?;
        let request: *mut AnyObject = unsafe { objc2::msg_send![request_class, new] };
        let request = unsafe { Retained::from_raw(request) }
            .ok_or(("service-not-allowed", "speech request unavailable".into()))?;
        let _: () = unsafe {
            objc2::msg_send![&*request, setShouldReportPartialResults: options.interim_results]
        };
        if options.process_locally {
            let _: () =
                unsafe { objc2::msg_send![&*request, setRequiresOnDeviceRecognition: true] };
        }

        let result_block =
            block2_05::RcBlock::new(|result: *mut AnyObject, error: *mut AnyObject| {
                if !result.is_null() {
                    let transcription: *mut AnyObject =
                        unsafe { objc2::msg_send![&*result, bestTranscription] };
                    let formatted: *mut AnyObject = if transcription.is_null() {
                        std::ptr::null_mut()
                    } else {
                        unsafe { objc2::msg_send![&*transcription, formattedString] }
                    };
                    let is_final: bool = unsafe { objc2::msg_send![&*result, isFinal] };
                    let confidence = if transcription.is_null() {
                        0
                    } else {
                        let segments: *mut AnyObject =
                            unsafe { objc2::msg_send![&*transcription, segments] };
                        let last: *mut AnyObject = if segments.is_null() {
                            std::ptr::null_mut()
                        } else {
                            unsafe { objc2::msg_send![&*segments, lastObject] }
                        };
                        if last.is_null() {
                            0
                        } else {
                            let value: f32 = unsafe { objc2::msg_send![&*last, confidence] };
                            (value.clamp(0.0, 1.0) * 100.0).round() as i64
                        }
                    };
                    push_event(SpeechRecognitionEvent::Result {
                        transcript: rust_string(formatted),
                        is_final,
                        confidence_percent: confidence,
                    });
                    if is_final {
                        push_event(SpeechRecognitionEvent::End);
                    }
                }
                if !error.is_null() {
                    push_event(SpeechRecognitionEvent::Error {
                        code: "audio-capture",
                        message: error_message(error),
                    });
                }
            });
        let task: *mut AnyObject = unsafe {
            objc2::msg_send![
                &*recognizer,
                recognitionTaskWithRequest: &*request,
                resultHandler: &*result_block
            ]
        };
        // `recognitionTaskWithRequest:` returns an autoreleased object. Keep a
        // strong reference because terminal callbacks may release the task
        // before the runtime event loop performs its cleanup pass.
        let task = unsafe { Retained::retain_autoreleased(task) }
            .ok_or(("service-not-allowed", "speech task unavailable".into()))?;

        let engine_class = AnyClass::get("AVAudioEngine")
            .ok_or(("audio-capture", "AVAudioEngine unavailable".into()))?;
        let engine: *mut AnyObject = unsafe { objc2::msg_send![engine_class, new] };
        let engine = unsafe { Retained::from_raw(engine) }
            .ok_or(("audio-capture", "AVAudioEngine unavailable".into()))?;
        let input_node: *mut AnyObject = unsafe { objc2::msg_send![&*engine, inputNode] };
        let input_node = unsafe { Retained::retain(input_node) }
            .ok_or(("audio-capture", "microphone input unavailable".into()))?;
        let format: *mut AnyObject =
            unsafe { objc2::msg_send![&*input_node, inputFormatForBus: 0usize] };
        let sample_rate: f64 = unsafe { objc2::msg_send![&*format, sampleRate] };
        let channel_count: u32 = unsafe { objc2::msg_send![&*format, channelCount] };
        if sample_rate <= 0.0 || channel_count == 0 {
            return Err((
                "audio-capture",
                "microphone input format is unavailable".into(),
            ));
        }
        let request_ptr = (&*request as *const AnyObject) as usize;
        let audio_block =
            block2_05::RcBlock::new(move |buffer: *mut AnyObject, _when: *mut AnyObject| {
                if buffer.is_null() {
                    return;
                }
                let request = request_ptr as *mut AnyObject;
                let _: () = unsafe { objc2::msg_send![&*request, appendAudioPCMBuffer: &*buffer] };
            });
        let _: () = unsafe {
            objc2::msg_send![
                &*input_node,
                installTapOnBus: 0usize,
                bufferSize: 1024u32,
                format: &*format,
                block: &*audio_block
            ]
        };
        let _: () = unsafe { objc2::msg_send![&*engine, prepare] };
        let mut error: *mut AnyObject = std::ptr::null_mut();
        let started: bool = unsafe { objc2::msg_send![&*engine, startAndReturnError: &mut error] };
        if !started {
            return Err(("audio-capture", error_message(error)));
        }
        SESSION.with(|session| {
            *session.borrow_mut() = Some(IosSpeechSession {
                _recognizer: recognizer,
                engine,
                input_node,
                request,
                task,
            });
        });
        push_event(SpeechRecognitionEvent::Start);
        Ok(())
    }

    fn configure_audio_session() -> Result<(), (&'static str, String)> {
        let audio_class = AnyClass::get("AVAudioSession")
            .ok_or(("audio-capture", "AVAudioSession unavailable".into()))?;
        let session: *mut AnyObject = unsafe { objc2::msg_send![audio_class, sharedInstance] };
        let category = ns_string("AVAudioSessionCategoryRecord")
            .ok_or(("audio-capture", "invalid audio category".into()))?;
        let mode = ns_string("AVAudioSessionModeMeasurement")
            .ok_or(("audio-capture", "invalid audio mode".into()))?;
        let mut error: *mut AnyObject = std::ptr::null_mut();
        let configured: bool = unsafe {
            objc2::msg_send![
                &*session,
                setCategory: &*category,
                mode: &*mode,
                options: 0usize,
                error: &mut error
            ]
        };
        if !configured {
            return Err(("audio-capture", error_message(error)));
        }
        error = std::ptr::null_mut();
        let active: bool = unsafe {
            objc2::msg_send![
                &*session,
                setActive: true,
                withOptions: 0usize,
                error: &mut error
            ]
        };
        if !active {
            return Err(("audio-capture", error_message(error)));
        }
        Ok(())
    }

    pub(super) fn stop() {
        PENDING.with(|pending| pending.borrow_mut().take());
        SESSION.with(|session| {
            let Some(active) = session.borrow_mut().take() else {
                return;
            };
            let _: () = unsafe { objc2::msg_send![&*active.input_node, removeTapOnBus: 0usize] };
            let _: () = unsafe { objc2::msg_send![&*active.engine, stop] };
            let _: () = unsafe { objc2::msg_send![&*active.request, endAudio] };
            let _: () = unsafe { objc2::msg_send![&*active.task, finish] };
            if let Some(audio_class) = AnyClass::get("AVAudioSession") {
                let audio_session: *mut AnyObject =
                    unsafe { objc2::msg_send![audio_class, sharedInstance] };
                let mut error: *mut AnyObject = std::ptr::null_mut();
                let _: bool = unsafe {
                    objc2::msg_send![
                        &*audio_session,
                        setActive: false,
                        withOptions: 0usize,
                        error: &mut error
                    ]
                };
            }
        });
    }
}

#[cfg(not(target_os = "ios"))]
mod platform {
    use super::{SpeechRecognitionEvent, SpeechRecognitionOptions, push_event};

    pub(super) fn request_start(_options: SpeechRecognitionOptions) {
        push_event(SpeechRecognitionEvent::Error {
            code: "service-not-allowed",
            message: "native SpeechRecognition is not available on this platform".into(),
        });
    }

    pub(super) fn poll_start() {}
    pub(super) fn stop() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_speech_error_codes_have_stable_signal_values() {
        assert_eq!(error_status("not-allowed"), -2);
        assert_eq!(error_status("service-not-allowed"), -4);
        assert_eq!(error_status("audio-capture"), -5);
    }
}
