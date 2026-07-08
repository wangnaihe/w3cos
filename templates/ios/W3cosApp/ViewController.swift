import UIKit

@_silgen_name("w3cos_set_safe_area_insets")
func w3cos_set_safe_area_insets(_ top: Float, _ right: Float, _ bottom: Float, _ left: Float)

@_silgen_name("w3cos_app_run")
func w3cos_app_run() -> Int32

class ViewController: UIViewController {
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
}
