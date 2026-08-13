//! Minimal UIKit diagnostics for the winit input client.

use objc2::encode::{Encode, Encoding};
use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Sel};
use objc2::{msg_send, sel};
use std::ffi::{CStr, CString};
use std::sync::Once;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::{AtomicI64, Ordering};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

static DOCUMENT_PICKER_DELEGATE: AtomicPtr<AnyObject> = AtomicPtr::new(std::ptr::null_mut());

extern "C" fn document_picker_did_pick(
    _this: &AnyObject,
    _cmd: Sel,
    _controller: &AnyObject,
    urls: &AnyObject,
) {
    let count: usize = unsafe { msg_send![urls, count] };
    let mut paths = Vec::with_capacity(count);
    let mut accessed_urls = Vec::with_capacity(count);
    for index in 0..count {
        let url: *mut AnyObject = unsafe { msg_send![urls, objectAtIndex: index] };
        if url.is_null() {
            continue;
        }
        let accessed: bool = unsafe { msg_send![&*url, startAccessingSecurityScopedResource] };
        if accessed {
            accessed_urls.push(url);
        }
        let path: *mut AnyObject = unsafe { msg_send![&*url, path] };
        if let Some(path) = rust_string(path) {
            paths.push(path);
        }
    }
    complete_document_picker(paths);
    for url in accessed_urls {
        let _: () = unsafe { msg_send![&*url, stopAccessingSecurityScopedResource] };
    }
}

extern "C" fn document_picker_cancelled(_this: &AnyObject, _cmd: Sel, _controller: &AnyObject) {
    complete_document_picker(Vec::new());
}

fn complete_document_picker(paths: Vec<String>) {
    let Ok(json) = serde_json::to_string(&paths) else {
        return;
    };
    let Ok(json) = CString::new(json) else {
        return;
    };
    crate::jsdom::w3cos_complete_file_picker(json.as_ptr());
}

fn document_picker_delegate() -> Option<&'static AnyObject> {
    let existing = DOCUMENT_PICKER_DELEGATE.load(Ordering::Acquire);
    if !existing.is_null() {
        return Some(unsafe { &*existing });
    }
    let superclass = AnyClass::get("NSObject")?;
    let class = if let Some(class) = AnyClass::get("W3cosDocumentPickerDelegate") {
        class
    } else {
        let mut builder = ClassBuilder::new("W3cosDocumentPickerDelegate", superclass)?;
        unsafe {
            builder.add_method(
                sel!(documentPicker:didPickDocumentsAtURLs:),
                document_picker_did_pick as extern "C" fn(_, _, _, _),
            );
            builder.add_method(
                sel!(documentPickerWasCancelled:),
                document_picker_cancelled as extern "C" fn(_, _, _),
            );
        }
        builder.register()
    };
    let delegate: *mut AnyObject = unsafe { msg_send![class, new] };
    if delegate.is_null() {
        return None;
    }
    match DOCUMENT_PICKER_DELEGATE.compare_exchange(
        std::ptr::null_mut(),
        delegate,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => Some(unsafe { &*delegate }),
        Err(existing) => {
            let _: () = unsafe { msg_send![&*delegate, release] };
            Some(unsafe { &*existing })
        }
    }
}

/// Present UIKit's document picker from the winit-owned application window.
///
/// Cargo produces the iOS executable directly, so this bridge must live in
/// the runtime instead of the optional Swift/Xcode template shell.
pub fn present_document_picker(allows_multiple: bool) -> bool {
    let Some(application_class) = AnyClass::get("UIApplication") else {
        return false;
    };
    let application: *mut AnyObject =
        unsafe { objc2::msg_send![application_class, sharedApplication] };
    if application.is_null() {
        return false;
    }
    let window: *mut AnyObject = unsafe { objc2::msg_send![&*application, keyWindow] };
    if window.is_null() {
        return false;
    }
    let root: *mut AnyObject = unsafe { objc2::msg_send![&*window, rootViewController] };
    if root.is_null() {
        return false;
    }
    let Some(document_picker_class) = AnyClass::get("UIDocumentPickerViewController") else {
        return false;
    };
    let Some(array_class) = AnyClass::get("NSArray") else {
        return false;
    };
    let Some(public_data) = ns_string("public.data") else {
        return false;
    };
    let document_types: *mut AnyObject =
        unsafe { objc2::msg_send![array_class, arrayWithObject: &*public_data] };
    let picker: *mut AnyObject = unsafe { objc2::msg_send![document_picker_class, alloc] };
    if picker.is_null() || document_types.is_null() {
        return false;
    }
    let picker: *mut AnyObject = unsafe {
        objc2::msg_send![&*picker, initWithDocumentTypes: &*document_types, inMode: 0usize]
    };
    if picker.is_null() {
        return false;
    }
    let _: () = unsafe { objc2::msg_send![&*picker, setAllowsMultipleSelection: allows_multiple] };
    let Some(delegate) = document_picker_delegate() else {
        return false;
    };
    let _: () = unsafe { objc2::msg_send![&*picker, setDelegate: delegate] };
    let _: () = unsafe {
        objc2::msg_send![
            &*root,
            presentViewController: &*picker,
            animated: true,
            completion: std::ptr::null::<AnyObject>()
        ]
    };
    true
}

fn view(window: &Window) -> Option<&AnyObject> {
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::UiKit(handle) = handle.as_raw() else {
        return None;
    };
    Some(unsafe { &*handle.ui_view.as_ptr().cast() })
}

/// Current UIKit safe-area insets in logical/CSS pixels.
///
/// Querying the UIView is more reliable than deriving these values from
/// winit's inner/outer geometry: recent winit versions expose a full-screen
/// content view on iOS, so both rectangles can have the same origin.
pub fn safe_area_insets(window: &Window) -> Option<w3cos_std::safe_area::SafeAreaInsets> {
    let root = view(window)?;
    let _: () = unsafe { objc2::msg_send![root, layoutIfNeeded] };
    let insets: UIEdgeInsets = unsafe { objc2::msg_send![root, safeAreaInsets] };
    Some(w3cos_std::safe_area::SafeAreaInsets {
        top: insets.top as f32,
        right: insets.right as f32,
        bottom: insets.bottom as f32,
        left: insets.left as f32,
    })
}

type CGFloat = f64;
const IME_TEXT_FIELD_TAG: isize = 0x5733_494d;
static KEYBOARD_OBSERVER_ONCE: Once = Once::new();
static KEYBOARD_INSET_MILLI: AtomicI64 = AtomicI64::new(-1);

#[repr(C)]
struct CGPoint {
    x: CGFloat,
    y: CGFloat,
}

unsafe impl Encode for CGPoint {
    const ENCODING: Encoding = Encoding::Struct("CGPoint", &[CGFloat::ENCODING, CGFloat::ENCODING]);
}

#[repr(C)]
struct CGSize {
    width: CGFloat,
    height: CGFloat,
}

unsafe impl Encode for CGSize {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[CGFloat::ENCODING, CGFloat::ENCODING]);
}

#[repr(C)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

unsafe impl Encode for CGRect {
    const ENCODING: Encoding = Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
}

#[repr(C)]
struct UIEdgeInsets {
    top: CGFloat,
    left: CGFloat,
    bottom: CGFloat,
    right: CGFloat,
}

unsafe impl Encode for UIEdgeInsets {
    const ENCODING: Encoding = Encoding::Struct(
        "UIEdgeInsets",
        &[
            CGFloat::ENCODING,
            CGFloat::ENCODING,
            CGFloat::ENCODING,
            CGFloat::ENCODING,
        ],
    );
}

fn ns_string(value: &str) -> Option<*mut AnyObject> {
    let value = CString::new(value).ok()?;
    let class = AnyClass::get("NSString")?;
    let string: *mut AnyObject =
        unsafe { objc2::msg_send![class, stringWithUTF8String: value.as_ptr()] };
    (!string.is_null()).then_some(string)
}

fn rust_string(value: *mut AnyObject) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let bytes: *const std::ffi::c_char = unsafe { objc2::msg_send![&*value, UTF8String] };
    if bytes.is_null() {
        return None;
    }
    Some(
        unsafe { CStr::from_ptr(bytes) }
            .to_string_lossy()
            .into_owned(),
    )
}

fn install_keyboard_frame_observer() {
    KEYBOARD_OBSERVER_ONCE.call_once(|| {
        KEYBOARD_INSET_MILLI.store(0, Ordering::SeqCst);
        let Some(center_class) = AnyClass::get("NSNotificationCenter") else {
            return;
        };
        let center: *mut AnyObject = unsafe { objc2::msg_send![center_class, defaultCenter] };
        for notification_name in [
            "UIKeyboardWillShowNotification",
            "UIKeyboardDidShowNotification",
            "UIKeyboardWillChangeFrameNotification",
            "UIKeyboardDidChangeFrameNotification",
            "UIKeyboardWillHideNotification",
            "UIKeyboardDidHideNotification",
        ] {
            let Some(name) = ns_string(notification_name) else {
                continue;
            };
            let hides_keyboard = notification_name.contains("Hide");
            let block = block2_05::RcBlock::new(move |notification: *mut AnyObject| {
                if notification.is_null() {
                    return;
                }
                let user_info: *mut AnyObject =
                    unsafe { objc2::msg_send![&*notification, userInfo] };
                if user_info.is_null() {
                    return;
                }
                let Some(frame_key) = ns_string("UIKeyboardFrameEndUserInfoKey") else {
                    return;
                };
                let value: *mut AnyObject =
                    unsafe { objc2::msg_send![&*user_info, objectForKey: &*frame_key] };
                if value.is_null() {
                    return;
                }
                let frame: CGRect = unsafe { objc2::msg_send![&*value, CGRectValue] };
                let Some(screen_class) = AnyClass::get("UIScreen") else {
                    return;
                };
                let screen: *mut AnyObject = unsafe { objc2::msg_send![screen_class, mainScreen] };
                if screen.is_null() {
                    return;
                }
                let bounds: CGRect = unsafe { objc2::msg_send![&*screen, bounds] };
                let covered = if hides_keyboard {
                    0.0
                } else {
                    (bounds.size.height - frame.origin.y)
                        .max(frame.size.height)
                        .clamp(0.0, bounds.size.height)
                };
                KEYBOARD_INSET_MILLI.store((covered * 1000.0) as i64, Ordering::SeqCst);
            });
            let _: *mut AnyObject = unsafe {
                objc2::msg_send![
                    &*center,
                    addObserverForName: &*name,
                    object: std::ptr::null::<AnyObject>(),
                    queue: std::ptr::null::<AnyObject>(),
                    usingBlock: &*block
                ]
            };
        }
    });
}

fn text_field(window: &Window, create: bool) -> Option<&AnyObject> {
    let root = view(window)?;
    let existing: *mut AnyObject =
        unsafe { objc2::msg_send![root, viewWithTag: IME_TEXT_FIELD_TAG] };
    if !existing.is_null() {
        return Some(unsafe { &*existing });
    }
    if !create {
        return None;
    }
    install_keyboard_frame_observer();

    let class = AnyClass::get("UITextField")?;
    let field: *mut AnyObject = unsafe { objc2::msg_send![class, alloc] };
    if field.is_null() {
        return None;
    }
    // Keep UIKit's full UITextInput implementation for marked text/candidate
    // handling, while w3cos remains responsible for drawing the visible field.
    let frame = CGRect {
        // Keep the transparent IME client inside the window. Newer UIKit
        // versions can show a keyboard for an offscreen first responder while
        // still withholding the effective text-input focus.
        origin: CGPoint { x: 1.0, y: 1.0 },
        size: CGSize {
            width: 1.0,
            height: 1.0,
        },
    };
    let field: *mut AnyObject = unsafe { objc2::msg_send![&*field, initWithFrame: frame] };
    if field.is_null() {
        return None;
    }
    let _: () = unsafe { objc2::msg_send![&*field, setTag: IME_TEXT_FIELD_TAG] };
    let _: () = unsafe { objc2::msg_send![&*field, setIsAccessibilityElement: true] };
    if let Some(identifier) = ns_string("w3cos-native-text-input") {
        let _: () = unsafe { objc2::msg_send![&*field, setAccessibilityIdentifier: &*identifier] };
    }
    let color_class = AnyClass::get("UIColor")?;
    let clear: *mut AnyObject = unsafe { objc2::msg_send![color_class, clearColor] };
    let _: () = unsafe { objc2::msg_send![&*field, setTextColor: clear] };
    let _: () = unsafe { objc2::msg_send![&*field, setTintColor: clear] };
    let _: () = unsafe { objc2::msg_send![root, addSubview: &*field] };
    Some(unsafe { &*field })
}

pub struct NativeTextInputState {
    pub text: String,
    pub is_composing: bool,
}

/// Use UIKit's UITextField as the native IME client. winit's iOS WinitView
/// implements UIKeyInput only, which cannot provide Pinyin marked text and
/// candidate selection.
pub fn ensure_text_input_first_responder(
    window: &Window,
    initial: &str,
    secure: bool,
) -> Option<bool> {
    let field = text_field(window, true)?;
    let _: () = unsafe { objc2::msg_send![field, setSecureTextEntry: secure] };
    let already_first: bool = unsafe { objc2::msg_send![field, isFirstResponder] };
    if !already_first {
        let value = ns_string(initial)?;
        let _: () = unsafe { objc2::msg_send![field, setText: &*value] };
    }
    let accepted = if already_first {
        true
    } else {
        unsafe { objc2::msg_send![field, becomeFirstResponder] }
    };
    let is_first: bool = unsafe { objc2::msg_send![field, isFirstResponder] };
    if std::env::var_os("W3COS_INPUT_TRACE").is_some() {
        eprintln!(
            "[W3C OS][IME] textField accepted={accepted} isFirst={is_first} inset={:?}",
            keyboard_inset_bottom(window)
        );
    }
    Some(accepted || is_first)
}

pub fn text_input_state(window: &Window) -> Option<NativeTextInputState> {
    let field = text_field(window, false)?;
    let text: *mut AnyObject = unsafe { objc2::msg_send![field, text] };
    let marked: *mut AnyObject = unsafe { objc2::msg_send![field, markedTextRange] };
    Some(NativeTextInputState {
        text: rust_string(text)?,
        is_composing: !marked.is_null(),
    })
}

pub fn set_text_input_value(window: &Window, value: &str) {
    let Some(field) = text_field(window, false) else {
        return;
    };
    let Some(value) = ns_string(value) else {
        return;
    };
    let _: () = unsafe { objc2::msg_send![field, setText: &*value] };
}

pub fn resign_text_input(window: &Window) {
    if let Some(field) = text_field(window, false) {
        let _: bool = unsafe { objc2::msg_send![field, resignFirstResponder] };
    }
}

/// Visible bottom inset reported by UIKit's keyboard layout guide (iOS 15+).
/// Values are UIKit points, which match the runtime's logical/CSS pixels.
pub fn keyboard_inset_bottom(window: &Window) -> Option<f32> {
    let notified = KEYBOARD_INSET_MILLI.load(Ordering::SeqCst);
    if notified >= 0 {
        return Some(notified as f32 / 1000.0);
    }
    fn covered_by_keyboard(view: &AnyObject) -> Option<f32> {
        let _: () = unsafe { objc2::msg_send![view, layoutIfNeeded] };
        let guide: *mut AnyObject = unsafe { objc2::msg_send![view, keyboardLayoutGuide] };
        if guide.is_null() {
            return None;
        }
        let bounds: CGRect = unsafe { objc2::msg_send![view, bounds] };
        let frame: CGRect = unsafe { objc2::msg_send![&*guide, layoutFrame] };
        if frame.size.width <= 0.0 && frame.size.height <= 0.0 {
            return None;
        }
        let safe_area: UIEdgeInsets = unsafe { objc2::msg_send![view, safeAreaInsets] };
        let covered = (bounds.size.height - frame.origin.y).clamp(0.0, bounds.size.height);
        Some(if covered <= safe_area.bottom + 8.0 {
            0.0
        } else {
            covered as f32
        })
    }

    let root = view(window)?;
    let root_covered = covered_by_keyboard(root).unwrap_or(0.0);
    let ui_window: *mut AnyObject = unsafe { objc2::msg_send![root, window] };
    if ui_window.is_null() {
        return Some(root_covered);
    }
    let window_covered = covered_by_keyboard(unsafe { &*ui_window }).unwrap_or(0.0);
    Some(root_covered.max(window_covered))
}

pub fn ensure_key_window(window: &Window) -> Option<bool> {
    let view = view(window)?;
    let ui_window: *mut AnyObject = unsafe { objc2::msg_send![view, window] };
    if ui_window.is_null() {
        return None;
    }
    let ui_window = unsafe { &*ui_window };
    let mut is_key: bool = unsafe { objc2::msg_send![ui_window, isKeyWindow] };
    if !is_key {
        let _: () = unsafe { objc2::msg_send![ui_window, makeKeyAndVisible] };
        is_key = unsafe { objc2::msg_send![ui_window, isKeyWindow] };
    }
    Some(is_key)
}
