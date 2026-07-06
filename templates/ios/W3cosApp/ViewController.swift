import UIKit

@_silgen_name("w3cos_app_run")
func w3cos_app_run() -> Int32

class ViewController: UIViewController {
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

        DispatchQueue.global(qos: .userInteractive).async {
            let code = w3cos_app_run()
            if code != 0 {
                DispatchQueue.main.async {
                    label.text = "w3cos_app_run failed: \(code)"
                }
            }
        }
    }
}
