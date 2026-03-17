import { Column, Row, Text, Button } from "@w3cos/std"

function MessageBubble(text: string, isMine: boolean) {
  return Row({
    style: { justifyContent: isMine ? "flex-end" : "flex-start", padding: "4px 0", width: "100%" },
    children: [
      Text(text, { style: {
        padding: "10px 16px",
        borderRadius: 16,
        background: isMine ? "#e94560" : "#1a1a2e",
        color: isMine ? "#ffffff" : "#e0e0e0",
        fontSize: 15,
        maxWidth: "75%",
      }}),
    ]
  })
}

export default Column({
  style: { gap: 0, padding: 0, background: "#0f0f1a", width: 400, height: 600, borderRadius: 16, overflow: "hidden" },
  children: [
    // Header
    Column({
      style: { padding: "16px 20px", background: "#1a1a2e", borderBottom: "1px solid #2a2a3e", width: "100%" },
      children: [
        Row({
          style: { justifyContent: "space-between", alignItems: "center" },
          children: [
            Text("←", { style: { fontSize: 20, color: "#e94560" } }),
            Column({ style: { gap: 2 }, children: [
              Text("Alice", { style: { fontSize: 16, color: "#ffffff", fontWeight: 600 } }),
              Text("Online", { style: { fontSize: 12, color: "#4caf50" } }),
            ]}),
            Text("⋮", { style: { fontSize: 20, color: "#888899" } }),
          ]
        }),
      ]
    }),

    // Messages
    Column({
      style: { gap: 8, padding: 20, flex: 1, width: "100%" },
      children: [
        MessageBubble("Hey! How's the W3C OS project going?", false),
        MessageBubble("Going great! Just finished the chat UI example 😄", true),
        MessageBubble("That's awesome! Can't wait to see it", false),
        MessageBubble("It's rendering entirely in native code. No browser needed!", true),
        MessageBubble("Pure native? That's impressive 🚀", false),
        MessageBubble("Thanks! The component system makes it really clean", true),
      ]
    }),

    // Input area
    Row({
      style: { padding: 12, background: "#1a1a2e", borderTop: "1px solid #2a2a3e", gap: 12, width: "100%" },
      children: [
        Text("Type a message...", { style: { flex: 1, padding: "12px 16px", background: "#0f0f1a", borderRadius: 24, color: "#606070", fontSize: 15, borderWidth: 1, borderColor: "#2a2a3e" } }),
        Text("➤", { style: { padding: "12px 16px", background: "#e94560", borderRadius: 24, color: "#ffffff", fontSize: 18, fontWeight: 700 } }),
      ]
    }),
  ]
})
