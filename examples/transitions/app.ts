import { Column, Row, Text, Button } from "@w3cos/std"

export default Column({
  style: {
    gap: 24,
    padding: 48,
    alignItems: "center",
    justifyContent: "center",
    background: "#0f0f1a",
  },
  children: [
    Text("CSS Transitions", { style: { fontSize: 28, color: "#ffffff", fontWeight: 700 } }),
    Text("Hover the boxes to see animated transitions", { style: { fontSize: 14, color: "#888899" } }),

    // Opacity transition
    Text("Opacity Transition", { style: { fontSize: 16, color: "#e94560", fontWeight: 600, marginTop: 16 } }),
    Row({
      style: { gap: 16, padding: 16, background: "#1a1a2e", borderRadius: 12 },
      children: [
        Text("Hover me", { style: { padding: "12px 24px", background: "#e94560", color: "#fff", borderRadius: 8, transition: { property: "Opacity", duration_ms: 300, easing: "EaseOut", delay_ms: 0 } } }),
      ]
    }),

    // Background color transition
    Text("Background Transition", { style: { fontSize: 16, color: "#e94560", fontWeight: 600, marginTop: 16 } }),
    Row({
      style: { gap: 16, padding: 16, background: "#1a1a2e", borderRadius: 12 },
      children: [
        Text("Color Shift", { style: { padding: "12px 24px", background: "#e94560", color: "#fff", borderRadius: 8, transition: { property: "All", duration_ms: 500, easing: "EaseInOut", delay_ms: 0 } } }),
      ]
    }),

    // Border radius transition
    Text("Border Radius Transition", { style: { fontSize: 16, color: "#e94560", fontWeight: 600, marginTop: 16 } }),
    Row({
      style: { gap: 16, padding: 16, background: "#1a1a2e", borderRadius: 12 },
      children: [
        Text("Rounded", { style: { padding: "12px 24px", background: "#0f3460", color: "#fff", border_radius: 8, transition: { property: "All", duration_ms: 400, easing: "Ease", delay_ms: 0 } } }),
      ]
    }),
  ]
})
