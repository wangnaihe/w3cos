import { Column, Row, Text, Button } from "@w3cos/std"

export default
<Column style={{ gap: 20, padding: 32, alignItems: "center", background: "#0f1419", flexGrow: 1 }}>
  <Text style={{ fontSize: 28, color: "#38bdf8", fontWeight: 700 }}>W3C OS Mobile</Text>
  <Text style={{ fontSize: 16, color: "#94a3b8" }}>Generic mobile-demo example</Text>
  <Row style={{ gap: 12, padding: 16 }}>
    <Button style={{ background: "#2563eb", color: "#ffffff", borderRadius: 8, fontSize: 15 }}>Tap</Button>
    <Button style={{ background: "#334155", color: "#e2e8f0", borderRadius: 8, fontSize: 15 }}>Action</Button>
  </Row>
  <Text style={{ fontSize: 12, color: "#64748b" }}>examples/mobile-demo · w3cos mobile M1</Text>
</Column>
