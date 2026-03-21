pub fn show(title: &str, body: &str) -> bool {
    notify_rust::Notification::new()
        .summary(title)
        .body(body)
        .show()
        .is_ok()
}

pub fn show_with_icon(title: &str, body: &str, icon: &str) -> bool {
    notify_rust::Notification::new()
        .summary(title)
        .body(body)
        .icon(icon)
        .show()
        .is_ok()
}
