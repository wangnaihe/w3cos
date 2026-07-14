//! Minimal UIKit diagnostics for the winit input client.

use objc2::encode::{Encode, Encoding};
use objc2::runtime::{AnyClass, AnyObject};
use std::ffi::{CStr, CString};
use std::sync::Once;
use std::sync::atomic::{AtomicI64, Ordering};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

fn view(window: &Window) -> Option<&AnyObject> {
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::UiKit(handle) = handle.as_raw() else {
        return None;
    };
    Some(unsafe { &*handle.ui_view.as_ptr().cast() })
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
        origin: CGPoint { x: -2.0, y: -2.0 },
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
    let _: () = unsafe { objc2::msg_send![&*field, setAccessibilityElementsHidden: true] };
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
pub fn ensure_text_input_first_responder(window: &Window, initial: &str) -> Option<bool> {
    let field = text_field(window, true)?;
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
