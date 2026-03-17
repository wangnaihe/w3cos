import { Column, Row, Text, Button } from "@w3cos/std"

function Calculator() {
  return Column({
    style: { gap: 16, padding: 32, alignItems: "center", background: "#1a1a2e", borderRadius: 16, width: 320 },
    children: [
      Text("Calculator", { style: { fontSize: 24, color: "#e94560", fontWeight: 700, marginBottom: 8 } }),
      Text("42", { style: { fontSize: 48, color: "#ffffff", fontWeight: 700, padding: 16, background: "#16213e", borderRadius: 12, width: "100%", textAlign: "right", marginBottom: 8 } }),
      Row({
        style: { gap: 8, width: "100%" },
        children: [
          Button("7", { style: { flex: 1, padding: 16, background: "#0f3460", color: "#fff", borderRadius: 8, fontSize: 20 } }),
          Button("8", { style: { flex: 1, padding: 16, background: "#0f3460", color: "#fff", borderRadius: 8, fontSize: 20 } }),
          Button("9", { style: { flex: 1, padding: 16, background: "#0f3460", color: "#fff", borderRadius: 8, fontSize: 20 } }),
          Button("/", { style: { flex: 1, padding: 16, background: "#e94560", color: "#fff", borderRadius: 8, fontSize: 20 } }),
        ]
      }),
      Row({
        style: { gap: 8, width: "100%" },
        children: [
          Button("4", { style: { flex: 1, padding: 16, background: "#0f3460", color: "#fff", borderRadius: 8, fontSize: 20 } }),
          Button("5", { style: { flex: 1, padding: 16, background: "#0f3460", color: "#fff", borderRadius: 8, fontSize: 20 } }),
          Button("6", { style: { flex: 1, padding: 16, background: "#0f3460", color: "#fff", borderRadius: 8, fontSize: 20 } }),
          Button("*", { style: { flex: 1, padding: 16, background: "#e94560", color: "#fff", borderRadius: 8, fontSize: 20 } }),
        ]
      }),
      Row({
        style: { gap: 8, width: "100%" },
        children: [
          Button("1", { style: { flex: 1, padding: 16, background: "#0f3460", color: "#fff", borderRadius: 8, fontSize: 20 } }),
          Button("2", { style: { flex: 1, padding: 16, background: "#0f3460", color: "#fff", borderRadius: 8, fontSize: 20 } }),
          Button("3", { style: { flex: 1, padding: 16, background: "#0f3460", color: "#fff", borderRadius: 8, fontSize: 20 } }),
          Button("-", { style: { flex: 1, padding: 16, background: "#e94560", color: "#fff", borderRadius: 8, fontSize: 20 } }),
        ]
      }),
      Row({
        style: { gap: 8, width: "100%" },
        children: [
          Button("0", { style: { flex: 2, padding: 16, background: "#0f3460", color: "#fff", borderRadius: 8, fontSize: 20 } }),
          Button(".", { style: { flex: 1, padding: 16, background: "#0f3460", color: "#fff", borderRadius: 8, fontSize: 20 } }),
          Button("=", { style: { flex: 1, padding: 16, background: "#e94560", color: "#fff", borderRadius: 8, fontSize: 20 } }),
        ]
      }),
    ]
  })
}

export default Calculator()
