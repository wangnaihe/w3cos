import UIKit
import UniformTypeIdentifiers

@_silgen_name("w3cos_set_safe_area_insets")
func w3cos_set_safe_area_insets(_ top: Float, _ right: Float, _ bottom: Float, _ left: Float)

@_silgen_name("w3cos_app_run")
func w3cos_app_run() -> Int32

@_silgen_name("w3cos_complete_file_picker")
func w3cos_complete_file_picker(_ pathsJSON: UnsafePointer<CChar>)

@_silgen_name("w3cos_set_file_picker_callback")
func w3cos_set_file_picker_callback(_ callback: @convention(c) (UInt8) -> Void)

private weak var w3cosFilePickerHost: ViewController?

private func requestW3cosFilePicker(_ allowsMultiple: UInt8) {
    DispatchQueue.main.async {
        w3cosFilePickerHost?.presentFilePicker(allowsMultiple: allowsMultiple != 0)
    }
}

class ViewController: UIViewController, UIDocumentPickerDelegate {
    private func reportSafeArea() {
        let insets = view.safeAreaInsets
        w3cos_set_safe_area_insets(
            Float(insets.top),
            Float(insets.right),
            Float(insets.bottom),
            Float(insets.left)
        )
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        w3cosFilePickerHost = self
        w3cos_set_file_picker_callback(requestW3cosFilePicker)
        view.backgroundColor = UIColor(red: 0.06, green: 0.08, blue: 0.10, alpha: 1)

        let label = UILabel()
        label.text = "W3C OS"
        label.textColor = .white
        label.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(label)
        NSLayoutConstraint.activate([
            label.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            label.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 24),
        ])

        DispatchQueue.main.async {
            self.reportSafeArea()
            let code = w3cos_app_run()
            if code != 0 {
                label.text = "w3cos_app_run failed: \(code)"
            }
        }
    }

    override func viewSafeAreaInsetsDidChange() {
        super.viewSafeAreaInsetsDidChange()
        reportSafeArea()
    }

    fileprivate func presentFilePicker(allowsMultiple: Bool) {
        guard presentedViewController == nil else { return }
        let picker = UIDocumentPickerViewController(forOpeningContentTypes: [.item], asCopy: true)
        picker.delegate = self
        picker.allowsMultipleSelection = allowsMultiple
        present(picker, animated: true)
    }

    func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) {
        let paths = urls.map(\.path)
        guard let data = try? JSONSerialization.data(withJSONObject: paths),
              let json = String(data: data, encoding: .utf8) else { return }
        json.withCString { w3cos_complete_file_picker($0) }
    }

    func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
        "[]".withCString { w3cos_complete_file_picker($0) }
    }
}
