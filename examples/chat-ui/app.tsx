import { Column, Row, Text, Button } from "@w3cos/std"

export default
<Column style={{ gap: 0, padding: 0, background: "#0f0f1a", width: 400, height: 600, borderRadius: 16, overflow: "hidden" }}>
  <Column style={{ padding: "16px 20px", background: "#1a1a2e", borderBottom: "1px solid #2a2a3e", width: "100%" }}>
    <Row style={{ justifyContent: "space-between", alignItems: "center" }}>
      <Text style={{ fontSize: 20, color: "#e94560" }}>←</Text>
      <Column style={{ gap: 2 }}>
        <Text style={{ fontSize: 16, color: "#ffffff", fontWeight: 600 }}>Alice</Text>
        <Text style={{ fontSize: 12, color: "#4caf50" }}>Online</Text>
      </Column>
      <Text style={{ fontSize: 20, color: "#888899" }}>⋮</Text>
    </Row>
  </Column>
  <Column style={{ gap: 8, padding: 20, flex: 1, width: "100%" }}>
    <Row style={{ justifyContent: "flex-start", padding: "4px 0", width: "100%" }}>
      <Text style={{ padding: "10px 16px", borderRadius: 16, background: "#1a1a2e", color: "#e0e0e0", fontSize: 15, maxWidth: "75%" }}>Hey! How's the W3C OS project going?</Text>
    </Row>
    <Row style={{ justifyContent: "flex-end", padding: "4px 0", width: "100%" }}>
      <Text style={{ padding: "10px 16px", borderRadius: 16, background: "#e94560", color: "#ffffff", fontSize: 15, maxWidth: "75%" }}>Going great! Just finished the chat UI example 😄</Text>
    </Row>
    <Row style={{ justifyContent: "flex-start", padding: "4px 0", width: "100%" }}>
      <Text style={{ padding: "10px 16px", borderRadius: 16, background: "#1a1a2e", color: "#e0e0e0", fontSize: 15, maxWidth: "75%" }}>That's awesome! Can't wait to see it</Text>
    </Row>
    <Row style={{ justifyContent: "flex-end", padding: "4px 0", width: "100%" }}>
      <Text style={{ padding: "10px 16px", borderRadius: 16, background: "#e94560", color: "#ffffff", fontSize: 15, maxWidth: "75%" }}>It's rendering entirely in native code. No browser needed!</Text>
    </Row>
    <Row style={{ justifyContent: "flex-start", padding: "4px 0", width: "100%" }}>
      <Text style={{ padding: "10px 16px", borderRadius: 16, background: "#1a1a2e", color: "#e0e0e0", fontSize: 15, maxWidth: "75%" }}>Pure native? That's impressive 🚀</Text>
    </Row>
    <Row style={{ justifyContent: "flex-end", padding: "4px 0", width: "100%" }}>
      <Text style={{ padding: "10px 16px", borderRadius: 16, background: "#e94560", color: "#ffffff", fontSize: 15, maxWidth: "75%" }}>Thanks! The component system makes it really clean</Text>
    </Row>
  </Column>
  <Row style={{ padding: 12, background: "#1a1a2e", borderTop: "1px solid #2a2a3e", gap: 12, width: "100%" }}>
    <Text style={{ flex: 1, padding: "12px 16px", background: "#0f0f1a", borderRadius: 24, color: "#606070", fontSize: 15, borderWidth: 1, borderColor: "#2a2a3e" }}>Type a message...</Text>
    <Text style={{ padding: "12px 16px", background: "#e94560", borderRadius: 24, color: "#ffffff", fontSize: 18, fontWeight: 700 }}>➤</Text>
  </Row>
</Column>
