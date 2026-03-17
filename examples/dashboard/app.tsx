import { Column, Row, Text, Button } from "@w3cos/std"

export default
<Column style={{ gap: 0, background: "#0f172a" }}>
  <Row style={{ padding: 16, background: "#1e293b", justifyContent: "spaceBetween", alignItems: "center" }}>
    <Text style={{ fontSize: 20, color: "#f8fafc", fontWeight: 700 }}>Dashboard</Text>
    <Text style={{ fontSize: 14, color: "#64748b" }}>v0.1.0</Text>
  </Row>
  <Row style={{ gap: 16, padding: 24 }}>
    <Column style={{ padding: 20, background: "#1e293b", borderRadius: 12, flexGrow: 1 }}>
      <Text style={{ fontSize: 14, color: "#94a3b8" }}>Users</Text>
      <Text style={{ fontSize: 28, color: "#f8fafc", fontWeight: 700 }}>12,847</Text>
      <Text style={{ fontSize: 14, color: "#4ade80" }}>+14.2%</Text>
    </Column>
    <Column style={{ padding: 20, background: "#1e293b", borderRadius: 12, flexGrow: 1 }}>
      <Text style={{ fontSize: 14, color: "#94a3b8" }}>Revenue</Text>
      <Text style={{ fontSize: 28, color: "#f8fafc", fontWeight: 700 }}>$84,230</Text>
      <Text style={{ fontSize: 14, color: "#4ade80" }}>+8.7%</Text>
    </Column>
    <Column style={{ padding: 20, background: "#1e293b", borderRadius: 12, flexGrow: 1 }}>
      <Text style={{ fontSize: 14, color: "#94a3b8" }}>Active</Text>
      <Text style={{ fontSize: 28, color: "#f8fafc", fontWeight: 700 }}>3,291</Text>
      <Text style={{ fontSize: 14, color: "#f87171" }}>-2.1%</Text>
    </Column>
  </Row>
  <Column style={{ padding: 24, gap: 12 }}>
    <Text style={{ fontSize: 18, color: "#f8fafc", fontWeight: 700 }}>Recent Activity</Text>
    <Row style={{ padding: 12, background: "#1e293b", borderRadius: 8, justifyContent: "spaceBetween" }}>
      <Text style={{ fontSize: 14, color: "#cbd5e1" }}>New user registered</Text>
      <Text style={{ fontSize: 12, color: "#64748b" }}>2 min ago</Text>
    </Row>
    <Row style={{ padding: 12, background: "#1e293b", borderRadius: 8, justifyContent: "spaceBetween" }}>
      <Text style={{ fontSize: 14, color: "#cbd5e1" }}>Payment received</Text>
      <Text style={{ fontSize: 12, color: "#64748b" }}>5 min ago</Text>
    </Row>
    <Row style={{ padding: 12, background: "#1e293b", borderRadius: 8, justifyContent: "spaceBetween" }}>
      <Text style={{ fontSize: 14, color: "#cbd5e1" }}>Server scaled up</Text>
      <Text style={{ fontSize: 12, color: "#64748b" }}>12 min ago</Text>
    </Row>
  </Column>
</Column>
