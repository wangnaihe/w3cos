import { Column, Row, Text } from "@w3cos/std"

function SettingItem(label: string, value: string) {
  return Row({
    style: { justifyContent: "space-between", padding: 16, borderBottom: "1px solid #1a1a2e", width: "100%" },
    children: [
      Text(label, { style: { fontSize: 16, color: "#e0e0e0" } }),
      Text(value, { style: { fontSize: 16, color: "#888899" } }),
    ]
  })
}

export default Column({
  style: { gap: 0, padding: 32, background: "#0f0f1a", width: 400, borderRadius: 16 },
  children: [
    Text("Settings", { style: { fontSize: 28, color: "#ffffff", fontWeight: 700, marginBottom: 16 } }),

    Text("Appearance", { style: { fontSize: 13, color: "#e94560", fontWeight: 600, marginTop: 16, marginBottom: 8 } }),
    SettingItem("Theme", "Dark"),
    SettingItem("Font Size", "16px"),
    SettingItem("Language", "English"),

    Text("Notifications", { style: { fontSize: 13, color: "#e94560", fontWeight: 600, marginTop: 16, marginBottom: 8 } }),
    SettingItem("Push Notifications", "Enabled"),
    SettingItem("Email Digest", "Weekly"),
    SettingItem("Sound", "On"),

    Text("Account", { style: { fontSize: 13, color: "#e94560", fontWeight: 600, marginTop: 16, marginBottom: 8 } }),
    SettingItem("Username", "w3cos_user"),
    SettingItem("Email", "user@w3cos.dev"),
    SettingItem("Plan", "Free"),

    Text("About", { style: { fontSize: 13, color: "#e94560", fontWeight: 600, marginTop: 16, marginBottom: 8 } }),
    SettingItem("Version", "1.0.0"),
    SettingItem("License", "MIT"),
  ]
})
