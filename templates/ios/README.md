# iOS shell template (M2)

Xcode project linking `libs/libw3cos_mobile_app.a` built from TSX via `w3cos mobile build`.

## Prerequisites

- **Xcode** (not Command Line Tools only): `sudo xcode-select -s /Applications/Xcode.app/Contents/Developer`
- Rust: `rustup target add aarch64-apple-ios-sim`

## Build

From mobile project root:

```bash
w3cos mobile build --platform ios
```

Then open `ios/W3cosApp.xcodeproj` in Xcode → Run on iPhone simulator.

## Layout

```
ios/
├── W3cosApp.xcodeproj
├── W3cosApp/
│   ├── AppDelegate.swift
│   ├── ViewController.swift   # calls w3cos_app_run()
│   └── Info.plist
└── libs/
    └── libw3cos_mobile_app.a  # produced by w3cos mobile build
```

## Customize

Set `PRODUCT_BUNDLE_IDENTIFIER` in Xcode ↔ `bundle_id` in `w3cos.app.json`.
