import { Column, Row, Text } from "@w3cos/std"

export default Column({
  style: { gap: 12, padding: 32, background: "#0f0f1a", width: 380, borderRadius: 16 },
  children: [
    Text("Weather", { style: { fontSize: 28, color: "#ffffff", fontWeight: 700, marginBottom: 8 } }),
    Text("San Francisco", { style: { fontSize: 18, color: "#a0a0b0" } }),

    Column({
      style: { gap: 8, padding: 24, background: "#1a1a2e", borderRadius: 16, alignItems: "center", marginTop: 16 },
      children: [
        Text("☀️", { style: { fontSize: 64 } }),
        Text("72°F", { style: { fontSize: 48, color: "#ffffff", fontWeight: 700 } }),
        Text("Sunny", { style: { fontSize: 18, color: "#e94560" } }),
      ]
    }),

    Row({
      style: { justifyContent: "space-around", marginTop: 16, padding: 16, background: "#1a1a2e", borderRadius: 12 },
      children: [
        Column({ style: { gap: 4, alignItems: "center" }, children: [
          Text("💧", { style: { fontSize: 20 } }),
          Text("62%", { style: { fontSize: 14, color: "#ffffff" } }),
          Text("Humidity", { style: { fontSize: 11, color: "#888899" } }),
        ]}),
        Column({ style: { gap: 4, alignItems: "center" }, children: [
          Text("🌬", { style: { fontSize: 20 } }),
          Text("12 mph", { style: { fontSize: 14, color: "#ffffff" } }),
          Text("Wind", { style: { fontSize: 11, color: "#888899" } }),
        ]}),
        Column({ style: { gap: 4, alignItems: "center" }, children: [
          Text("👁", { style: { fontSize: 20 } }),
          Text("10 mi", { style: { fontSize: 14, color: "#ffffff" } }),
          Text("Visibility", { style: { fontSize: 11, color: "#888899" } }),
        ]}),
      ]
    }),

    Text("5-Day Forecast", { style: { fontSize: 16, color: "#e94560", fontWeight: 600, marginTop: 16, marginBottom: 4 } }),
    Row({
      style: { justifyContent: "space-around", padding: 12, background: "#1a1a2e", borderRadius: 12 },
      children: [
        Column({ style: { gap: 4, alignItems: "center" }, children: [
          Text("Mon", { style: { fontSize: 12, color: "#888899" } }),
          Text("☀️", { style: { fontSize: 24 } }),
          Text("75°", { style: { fontSize: 14, color: "#ffffff" } }),
        ]}),
        Column({ style: { gap: 4, alignItems: "center" }, children: [
          Text("Tue", { style: { fontSize: 12, color: "#888899" } }),
          Text("⛅", { style: { fontSize: 24 } }),
          Text("68°", { style: { fontSize: 14, color: "#ffffff" } }),
        ]}),
        Column({ style: { gap: 4, alignItems: "center" }, children: [
          Text("Wed", { style: { fontSize: 12, color: "#888899" } }),
          Text("🌧", { style: { fontSize: 24 } }),
          Text("61°", { style: { fontSize: 14, color: "#ffffff" } }),
        ]}),
        Column({ style: { gap: 4, alignItems: "center" }, children: [
          Text("Thu", { style: { fontSize: 12, color: "#888899" } }),
          Text("🌧", { style: { fontSize: 24 } }),
          Text("59°", { style: { fontSize: 14, color: "#ffffff" } }),
        ]}),
        Column({ style: { gap: 4, alignItems: "center" }, children: [
          Text("Fri", { style: { fontSize: 12, color: "#888899" } }),
          Text("☀️", { style: { fontSize: 24 } }),
          Text("73°", { style: { fontSize: 14, color: "#ffffff" } }),
        ]}),
      ]
    }),
  ]
})
