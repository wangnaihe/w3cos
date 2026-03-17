import { Column, Row, Text, Button } from "@w3cos/std"

export default Column({
  style: { gap: 0, background: "#0f172a" },
  children: [
    Row({
      style: { padding: 16, background: "#1e293b", justifyContent: "spaceBetween", alignItems: "center" },
      children: [
        Text("Dashboard", { style: { fontSize: 20, color: "#f8fafc", fontWeight: 700 } }),
        Text("v0.1.0", { style: { fontSize: 14, color: "#64748b" } }),
      ]
    }),
    Row({
      style: { gap: 16, padding: 24 },
      children: [
        Column({
          style: { padding: 20, background: "#1e293b", borderRadius: 12, flexGrow: 1 },
          children: [
            Text("Users", { style: { fontSize: 14, color: "#94a3b8" } }),
            Text("12,847", { style: { fontSize: 28, color: "#f8fafc", fontWeight: 700 } }),
            Text("+14.2%", { style: { fontSize: 14, color: "#4ade80" } }),
          ]
        }),
        Column({
          style: { padding: 20, background: "#1e293b", borderRadius: 12, flexGrow: 1 },
          children: [
            Text("Revenue", { style: { fontSize: 14, color: "#94a3b8" } }),
            Text("$84,230", { style: { fontSize: 28, color: "#f8fafc", fontWeight: 700 } }),
            Text("+8.7%", { style: { fontSize: 14, color: "#4ade80" } }),
          ]
        }),
        Column({
          style: { padding: 20, background: "#1e293b", borderRadius: 12, flexGrow: 1 },
          children: [
            Text("Active", { style: { fontSize: 14, color: "#94a3b8" } }),
            Text("3,291", { style: { fontSize: 28, color: "#f8fafc", fontWeight: 700 } }),
            Text("-2.1%", { style: { fontSize: 14, color: "#f87171" } }),
          ]
        }),
      ]
    }),
    Column({
      style: { padding: 24, gap: 12 },
      children: [
        Text("Recent Activity", { style: { fontSize: 18, color: "#f8fafc", fontWeight: 700 } }),
        Row({
          style: { padding: 12, background: "#1e293b", borderRadius: 8, justifyContent: "spaceBetween" },
          children: [
            Text("New user registered", { style: { fontSize: 14, color: "#cbd5e1" } }),
            Text("2 min ago", { style: { fontSize: 12, color: "#64748b" } }),
          ]
        }),
        Row({
          style: { padding: 12, background: "#1e293b", borderRadius: 8, justifyContent: "spaceBetween" },
          children: [
            Text("Payment received", { style: { fontSize: 14, color: "#cbd5e1" } }),
            Text("5 min ago", { style: { fontSize: 12, color: "#64748b" } }),
          ]
        }),
        Row({
          style: { padding: 12, background: "#1e293b", borderRadius: 8, justifyContent: "spaceBetween" },
          children: [
            Text("Server scaled up", { style: { fontSize: 14, color: "#cbd5e1" } }),
            Text("12 min ago", { style: { fontSize: 12, color: "#64748b" } }),
          ]
        }),
      ]
    }),
  ]
})
