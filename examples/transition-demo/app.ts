import { Column, Row, Text, Button } from "@w3cos/std"

export default Column({
  style: {
    gap: 24,
    padding: 48,
    alignItems: "center",
    justifyContent: "center",
    background: "#0f0f1a",
    width: 600,
  },
  children: [
    Text("CSS Transition Demo", {
      style: {
        fontSize: 32,
        color: "#ffffff",
        fontWeight: 700,
        marginBottom: 8,
        transition: {
          property: "All",
          duration_ms: 500,
          easing: "EaseInOut",
          delay_ms: 0,
        },
      },
    }),
    Text("Hover over buttons to see smooth transitions", {
      style: { fontSize: 16, color: "#888899" },
    }),
    Row({
      style: { gap: 24, marginTop: 32 },
      children: [
        Button("Fade Effect", {
          style: {
            padding: "16px 32px",
            background: "#e94560",
            color: "#ffffff",
            borderRadius: 12,
            fontSize: 18,
            transition: {
              property: "All",
              duration_ms: 300,
              easing: "EaseOut",
              delay_ms: 0,
            },
          },
        }),
        Button("Smooth Move", {
          style: {
            padding: "16px 32px",
            background: "#0f3460",
            color: "#ffffff",
            borderRadius: 12,
            fontSize: 18,
            transition: {
              property: "All",
              duration_ms: 600,
              easing: "EaseInOut",
              delay_ms: 0,
            },
          },
        }),
        Button("Quick Snap", {
          style: {
            padding: "16px 32px",
            background: "#16213e",
            color: "#e94560",
            borderRadius: 12,
            fontSize: 18,
            borderWidth: 1,
            borderColor: "#e94560",
            transition: {
              property: "All",
              duration_ms: 100,
              easing: "Linear",
              delay_ms: 0,
            },
          },
        }),
      ],
    }),
  ],
})
