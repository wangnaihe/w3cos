import { Column, Row, Text, Button } from "@w3cos/std"

export default Column({
  style: { gap: 24, padding: 64, alignItems: "center", justifyContent: "center", background: "#1e1e2e" },
  children: [
    Text("Counter", { style: { fontSize: 32, color: "#cdd6f4", fontWeight: 700 } }),
    Text("0", { style: { fontSize: 72, color: "#f38ba8" } }),
    Row({
      style: { gap: 16 },
      children: [
        Button("-", { style: { fontSize: 24, background: "#45475a", color: "#cdd6f4", borderRadius: 12 } }),
        Button("+", { style: { fontSize: 24, background: "#a6e3a1", color: "#1e1e2e", borderRadius: 12 } }),
      ]
    }),
  ]
})
