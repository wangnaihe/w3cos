import { Column, Row, Text, Button } from "@w3cos/std"

const count = signal(0)

export default
<Column style={{ gap: 24, padding: 64, alignItems: "center", justifyContent: "center", background: "#1e1e2e" }}>
  <Text style={{ fontSize: 32, color: "#cdd6f4", fontWeight: 700 }}>Counter</Text>
  <Text style={{ fontSize: 72, color: "#f38ba8" }}>{count}</Text>
  <Row style={{ gap: 16 }}>
    <Button onClick="decrement:count" style={{ fontSize: 24, background: "#45475a", color: "#cdd6f4", borderRadius: 12 }}>-</Button>
    <Button onClick="set:count:0" style={{ fontSize: 24, background: "#585b70", color: "#cdd6f4", borderRadius: 12 }}>Reset</Button>
    <Button onClick="increment:count" style={{ fontSize: 24, background: "#a6e3a1", color: "#1e1e2e", borderRadius: 12 }}>+</Button>
  </Row>
</Column>
