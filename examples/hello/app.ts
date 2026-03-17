import { Column, Row, Text, Button } from "@w3cos/std"

export default Column({
  style: { gap: 20, padding: 48, alignItems: "center", justifyContent: "center", background: "#0f0f1a" },
  children: [
    Text("W3C OS", { style: { fontSize: 42, color: "#e94560", fontWeight: 700 } }),
    Text("TypeScript compiled to native binary.", { style: { fontSize: 20, color: "#a0a0b0" } }),
    Text("No browser. No V8. No Electron. Pure native.", { style: { fontSize: 16, color: "#606070" } }),
    Row({
      style: { gap: 16, padding: 24 },
      children: [
        Button("Get Started", { style: { background: "#e94560", color: "#ffffff", borderRadius: 8, fontSize: 16 } }),
        Button("View Source", { style: { background: "#1a1a2e", color: "#e94560", borderRadius: 8, borderWidth: 1, borderColor: "#e94560", fontSize: 16 } }),
      ]
    }),
    Text("2.4 MB native binary  |  < 100ms startup  |  10 MB RAM", { style: { fontSize: 14, color: "#404050" } }),
  ]
})
